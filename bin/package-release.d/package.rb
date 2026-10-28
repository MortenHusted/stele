# frozen_string_literal: true
require 'digest'
require 'fileutils'
require 'json'
require 'rbconfig'
require 'shellwords'
require 'tmpdir'
require 'tempfile'
require 'tty-command'
require_relative 'notices'
require_relative 'archive'
require_relative '../scrub.d/git_context'

class ReleasePackage
  TARGETS = %w[x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu aarch64-apple-darwin x86_64-apple-darwin].freeze

  def initialize
    @root = File.realpath(File.expand_path('../..', __dir__))
    @runner = TTY::Command.new(printer: :null)
  end

  def run(version:, target:, dist_dir: nil)
    raise ArgumentError, 'unsupported release target' unless TARGETS.include?(target)
    workspace = File.read(File.join(@root, 'Cargo.toml')).split('[workspace.package]', 2).fetch(1).split(/^\[/, 2).first
    expected = workspace.match(/^version = "([^"]+)"$/)[1]
    raise ArgumentError, 'version must equal workspace version' unless version == expected
    head = clean_head
    epoch = Integer(command(*ScrubGitContext.command, 'git', '-C', @root, 'show', '-s', '--format=%ct', 'HEAD').out.strip)
    dist = File.expand_path(dist_dir || File.join(@root, 'dist'))
    FileUtils.mkdir_p(dist)
    archive = File.join(dist, "stele-#{version}-#{target}.tar.gz")
    raise ArgumentError, 'archive and checksum must be new' if [archive, archive + '.sha256'].any? { |path| File.exist?(path) || File.symlink?(path) }
    cargo_home = File.expand_path(ENV['CARGO_HOME'] || File.join(Dir.home, '.cargo'))
    metadata = JSON.parse(command('cargo', 'metadata', '--locked', '--offline', '--format-version', '1', '--filter-platform', target).out)
    target_dir = File.expand_path(metadata.fetch('target_directory'))
    FileUtils.mkdir_p(target_dir)
    sysroot = command('rustc', '--print', 'sysroot').out.strip
    compiler = command('rustc', '-vV').out.lines.filter_map { |line| line.include?(': ') ? line.strip.split(': ', 2) : nil }.to_h.slice('release', 'commit-hash', 'host', 'LLVM version')
    paths = [[Dir.home, '/stele/builder'], [@root, '/stele/source'], [cargo_home, '/stele/dependencies'], [sysroot, '/stele/compiler'], [target_dir, '/stele/build-output']]
    paths << [File.expand_path(ENV['CARGO_TARGET_DIR'], @root), '/stele/build-output'] if ENV['CARGO_TARGET_DIR']
    maps = path_maps(paths)
    prefixes = maps.map(&:first)
    if target.end_with?('linux-gnu')
      maps << ['/target', '/stele/build-output']
      prefixes << '/target/'
    end
    old_flags = ENV['CARGO_ENCODED_RUSTFLAGS'] ? ENV.fetch('CARGO_ENCODED_RUSTFLAGS').split("\x1f") : Shellwords.split(ENV.fetch('RUSTFLAGS', ''))
    flags = old_flags + maps.map { |physical, logical| '--remap-path-prefix=' + physical + '=' + logical }
    build_env = {'CARGO_ENCODED_RUSTFLAGS' => flags.join("\x1f")}
    %w[CFLAGS CXXFLAGS].each do |name|
      native_flags = Shellwords.split(ENV.fetch(name, '')) + maps.map { |physical, logical| '-ffile-prefix-map=' + physical + '=' + logical }
      build_env[name] = Shellwords.join(native_flags)
    end
    build_env['CC_SHELL_ESCAPED_FLAGS'] = '1'
    if target.end_with?('linux-gnu')
      command(File.join(@root, 'scripts/ci/verify-cross-runner.sh'))
      builder = 'cross'
    else
      builder = 'cargo'
    end
    warn 'package-release: building Stele with explicit path remapping'
    command(builder, 'build', '--release', '--locked', '--target', target, '--bin', 'stele', env: build_env)
    inventory = nil
    Dir.mktmpdir('stele-package') do |stage|
      binary = File.join(target_dir, target, 'release/stele')
      raise ArgumentError, 'build did not produce an executable' unless File.executable?(binary)
      bytes = File.binread(binary)
      check_paths!(bytes, prefixes)
      File.open(File.join(stage, 'stele'), File::WRONLY | File::CREAT | File::EXCL, 0o755) { |file| file.write(bytes) }
      if native_target?(target)
        output = command(File.join(stage, 'stele'), '--version').out.strip
        raise ArgumentError, 'built binary version differs' unless output == 'stele ' + version
      elsif target == 'x86_64-apple-darwin' && RbConfig::CONFIG['host_os'].include?('darwin') && @runner.run!('arch', '-x86_64', '/usr/bin/true').success?
        output = command('arch', '-x86_64', File.join(stage, 'stele'), '--version').out.strip
        raise ArgumentError, 'built binary version differs' unless output == 'stele ' + version
      end
      inventory = ReleaseNotices.new(repo: @root, cargo_home: cargo_home).write(stage, metadata, sysroot: sysroot, compiler: compiler)
      raise ArgumentError, 'source changed during release preparation' unless clean_head == head
      receipt = {'contract' => 'stele-release-build/v1', 'source_commit' => head, 'target' => target, 'version' => version, 'binary_sha256' => Digest::SHA256.hexdigest(bytes), 'notices_sha256' => Digest::SHA256.file(File.join(stage, 'THIRD_PARTY_INVENTORY.json')).hexdigest, 'path_policy' => 'measured lexical/physical builder prefixes absent; registry and Rust library source paths under /stele/; Cross target-toolchain sources may use /stele/builder', 'review_status' => 'pending_owner_review'}
      File.write(File.join(stage, 'BUILD.json'), JSON.pretty_generate(receipt) + "\n")
      Tempfile.create(['stele-package-', '.tar.gz'], dist) do |temporary|
        ReleaseArchive.write(temporary, stage: stage, root: "stele-#{version}-#{target}", epoch: epoch)
        temporary.flush
        tar_bytes = Zlib::GzipReader.open(temporary.path, &:read)
        check_paths!(tar_bytes, prefixes)
        File.chmod(0o644, temporary.path)
        File.link(temporary.path, archive)
      end
    end
    digest = Digest::SHA256.file(archive).hexdigest
    File.open(archive + '.sha256', File::WRONLY | File::CREAT | File::EXCL, 0o644) { |file| file.write(digest + '  ' + File.basename(archive) + "\n") }
    {'archive' => archive, 'sha256' => digest, 'dependency_notice_scope' => inventory.fetch('scope'), 'review_status' => 'pending_owner_review'}
  end

  private

  def path_maps(paths)
    paths.flat_map do |path, logical|
      [File.expand_path(path), File.realpath(path)].uniq.map { |physical| [physical, logical] }
    end.uniq
  end

  def check_paths!(bytes, prefixes)
    bytes = bytes.b
    raise ArgumentError, 'payload contains a measured builder path; no archive produced' if prefixes.any? { |prefix| bytes.include?(prefix.b) }
    bytes.to_enum(:scan, %r{registry/src/index\.crates\.io-|lib/rustlib/src/rust/library}n).each do
      offset = Regexp.last_match.begin(0)
      start = [offset - 4096, 0].max
      prefix = bytes.byteslice(start, offset - start)[%r{[A-Za-z0-9_/.+@=-]*\z}n]
      slash = prefix.index('/')
      raise ArgumentError, 'source path is outside /stele/ logical roots; no archive produced' unless slash && prefix.byteslice(slash..).start_with?('/stele/')
    end
  end

  def native_target?(target)
    os = RbConfig::CONFIG['host_os'].include?('darwin') ? 'apple-darwin' : 'unknown-linux-gnu'
    cpu = %w[arm64 aarch64].include?(RbConfig::CONFIG['host_cpu']) ? 'aarch64' : 'x86_64'
    target == cpu + '-' + os
  end

  def clean_head
    raise ArgumentError, 'release source must be clean and committed' unless command(*ScrubGitContext.command, 'git', '-C', @root, 'status', '--porcelain', '--untracked-files=all').out.empty?
    command(*ScrubGitContext.command, 'git', '-C', @root, 'rev-parse', 'HEAD').out.strip
  end

  def command(*args, env: {})
    prefix = env.empty? ? [] : ['env', *env.map { |key, value| key + '=' + value }]
    @runner.run(*prefix, *args, chdir: @root) { |_stdout, stderr| $stderr.write(stderr.to_s) }
  end
end
