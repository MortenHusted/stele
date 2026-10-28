# frozen_string_literal: true
require 'digest'
require 'fileutils'
require 'find'
require 'json'
require 'rbconfig'
require 'rubygems/package'
require 'zlib'
require 'tty-command'
require_relative '../scrub.d/scan'

class ExportReview
  def initialize
    @root = File.expand_path('../..', __dir__)
    @policy = JSON.parse(File.read(File.join(@root, 'publication/provenance.json')))
    @scanner = JSON.parse(File.read(File.join(@root, 'toolchain/security/gitleaks.json')))
    @notice_pins = JSON.parse(File.read(File.join(@root, 'publication/dependency-notices.json'))).fetch('packages')
    @notice_pins.each_value { |pin| self.class.verify_pin(File.join(@root, pin.fetch('path')), pin.fetch('sha256')) }
    path_pin = @scanner.fetch('default_paths')
    self.class.verify_pin(File.join(@root, path_pin.fetch('path')), path_pin.fetch('sha256'))
    @path_patterns = JSON.parse(File.read(File.join(@root, path_pin.fetch('path')))).fetch('paths').map { |pattern| Regexp.new(pattern) }
    @runner = TTY::Command.new(printer: :null)
  end

  def self.platform
    os = RbConfig::CONFIG['host_os']
    cpu = RbConfig::CONFIG['host_cpu']
    return 'darwin_arm64' if os.include?('darwin') && cpu == 'arm64'
    return 'linux_x64' if os.include?('linux') && cpu == 'x86_64'
    return 'linux_arm64' if os.include?('linux') && %w[aarch64 arm64].include?(cpu)
    raise ArgumentError, 'no pinned scanner for this host'
  end

  def self.verify_pin(path, sha)
    raise ArgumentError, 'pinned input content differs' unless Digest::SHA256.file(path).hexdigest == sha
  end

  def run(repo:, deny_list:, scanner_archive:, out:, artifacts: [])
    repo = File.realpath(repo)
    artifacts = artifacts.map { |path| File.realpath(path) }
    raise ArgumentError, 'artifacts must be unpacked directories' unless artifacts.all? { |path| File.directory?(path) }
    before = inventory(repo, artifacts)
    out = File.join(File.realpath(File.dirname(out)), File.basename(out))
    raise ArgumentError, 'reports must be new, outside Git and outside scanned inputs' if File.exist?(out) || File.symlink?(out) || git(File.dirname(out), 'rev-parse', '--show-toplevel', check: false).success? || [repo, *artifacts].any? { |input| out == input || out.start_with?(input + '/') || input.start_with?(out + '/') }
    pin = @scanner.fetch('configuration')
    config = File.join(@root, pin.fetch('path'))
    self.class.verify_pin(config, pin.fetch('sha256'))
    asset = @scanner.fetch('assets').fetch(self.class.platform)
    self.class.verify_pin(scanner_archive, asset.fetch('archive_sha256'))
    Dir.mkdir(out, 0o700)
    binary = prepare_scanner(scanner_archive, out, asset)
    @scanner_attributes = nil
    raise ArgumentError, 'scanner version differs from pin' unless @runner.run(*ScrubGitContext.command, binary, 'version').out.strip == @scanner.fetch('version_output')
    write(out, 'inventory.json', before)
    scrub = IdentifierScrub.new(repo: repo, deny_list: deny_list, artifacts: artifacts, report: File.join(out, 'identifiers.json')).run
    # Scan a captured tracked tree: ignored local build/operator inputs are not shipped.
    mirror = File.join(out, 'history.git')
    git(@root, 'clone', '--mirror', '--no-local', '--no-hardlinks', repo, mirror)
    raise ArgumentError, 'history refs changed before capture' unless refs(mirror) == before.fetch('refs')
    @scanner_attributes = git(mirror, 'hash-object', '-w', '-t', 'tree', File::NULL).out.strip
    tree = File.join(out, 'tree');Dir.mkdir(tree, 0o700)
    capture(repo, before.fetch('files'), File.join(tree, 'content'))
    counts = {'tree' => scan(binary, config, out, 'tree', 'dir', tree), 'history' => scan(binary, config, out, 'history', 'git', mirror, '--log-opts=--all --text --no-ext-diff --no-textconv')}
    artifacts.each_with_index do |path, index|
      name = 'artifact-' + index.to_s
      payload = File.join(out, name);Dir.mkdir(payload, 0o700)
      capture(path, before.fetch('artifacts').fetch(index).fetch('files'), File.join(payload, 'content'))
      counts[name] = scan(binary, config, out, name, 'dir', payload)
    end
    raise ArgumentError, 'review inputs changed during scanning' unless before == inventory(repo, artifacts)
    self.class.verify_pin(binary, asset.fetch('binary_sha256'))
    self.class.verify_pin(config, pin.fetch('sha256'))
    result = {'contract' => 'stele-export-review/v1', 'scans_clean' => scrub.fetch('matches').empty? && counts.values.all?(&:zero?), 'provenance_review' => 'pending_owner_review', 'artifact_count' => artifacts.length, 'manual_secret_review_count' => (before.fetch('files') + before.fetch('artifacts').flat_map { |payload| payload.fetch('files') }).count { |entry| entry.fetch('default_allowlist_path_match') }, 'inventory_sha256' => Digest::SHA256.file(File.join(out, 'inventory.json')).hexdigest, 'secret_findings' => counts, 'identifier_findings' => scrub.fetch('matches').length, 'scanner_sha256' => asset.fetch('binary_sha256'), 'configuration_sha256' => pin.fetch('sha256'), 'scope' => 'captured tracked tree, reachable Git history, all local objects for identifiers, and explicitly supplied unpacked artifacts under pinned rules and built-in allowlists; manual review of excluded paths and provenance is required'}
    write(out, 'result.json', result)
    result
  end

  def inventory(repo, artifacts)
    raise ArgumentError, 'review needs a clean, non-shallow Git root' unless File.realpath(git(repo, 'rev-parse', '--show-toplevel').out.strip) == repo && git(repo, 'status', '--porcelain', '--untracked-files=all').out.empty? && git(repo, 'rev-parse', '--is-shallow-repository').out.strip == 'false'
    algorithm = {'sha1' => Digest::SHA1, 'sha256' => Digest::SHA256}.fetch(git(repo, 'rev-parse', '--show-object-format').out.strip)
    entries = git(repo, 'ls-tree', '-rz', '--full-tree', 'HEAD').out.split("\0").map do |line|
      metadata, path = line.split("\t", 2)
      path.force_encoding(Encoding::UTF_8)
      raise ArgumentError, 'source paths must be valid UTF-8 for review' unless path.valid_encoding?
      mode, type, oid = metadata.split
      raise ArgumentError, 'submodules and non-file Git entries require separate review' unless type == 'blob' && %w[100644 100755 120000].include?(mode)
      bytes = File.symlink?(File.join(repo, path)) ? File.readlink(File.join(repo, path)).b : File.binread(File.join(repo, path))
      raise ArgumentError, 'working file differs from committed content' unless algorithm.hexdigest("blob #{bytes.bytesize}\0".b + bytes) == oid
      source = @policy.fetch('overrides').find { |rule| rule['path'] == path || (rule['prefix'] && path.start_with?(rule.fetch('prefix'))) }&.fetch('source') || 'project'
      {'path' => path, 'mode' => mode, 'sha256' => Digest::SHA256.hexdigest(bytes), 'source' => source, 'default_allowlist_path_match' => default_allowlist_match?(path), 'notices' => @policy.fetch('sources').fetch(source).fetch('notices'), 'review_status' => 'pending_owner_review'}
    end
    paths = entries.map { |entry| entry.fetch('path') }
    @notice_pins.each_value do |pin|
      self.class.verify_pin(File.join(repo, pin.fetch('path')), pin.fetch('sha256')) if paths.include?(pin.fetch('path'))
    end
    entries.each do |entry|
      raise ArgumentError, 'required source notice is absent from export' unless (entry.fetch('notices') - paths).empty?
      next unless entry.fetch('mode') == '120000'
      target = File.realpath(File.join(repo, entry.fetch('path')))
      raise ArgumentError, 'shipped source link leaves the tracked tree' unless target.start_with?(repo + '/') && paths.include?(target.delete_prefix(repo + '/')) && File.file?(target)
    end
    payloads = artifacts.map.with_index do |root, index|
      artifact_entries = []
      Find.find(root) do |path|
        next if File.directory?(path) && !File.symlink?(path)
        raise ArgumentError, 'artifact contains a nonregular entry' unless File.file?(path)
        real = File.realpath(path)
        raise ArgumentError, 'artifact link leaves its payload' unless real.start_with?(root + '/')
        bytes = File.symlink?(path) ? File.readlink(path).b : File.binread(path)
        artifact_entries << {'path' => path.delete_prefix(root + '/'), 'mode' => format('%06o', File.lstat(path).mode & 0o177777), 'sha256' => Digest::SHA256.hexdigest(bytes), 'source' => nil, 'notices' => nil, 'default_allowlist_path_match' => default_allowlist_match?(path.delete_prefix(root + '/')), 'review_status' => 'pending_build_and_notice_provenance'}
      end
      {'index' => index, 'files' => artifact_entries.sort_by { |entry| entry.fetch('path') }}
    end
    {'contract' => 'stele-export-inventory/v1', 'head' => git(repo, 'rev-parse', 'HEAD').out.strip, 'refs' => refs(repo), 'policy_sha256' => Digest::SHA256.file(File.join(@root, 'publication/provenance.json')).hexdigest, 'sources' => @policy.fetch('sources'), 'files' => entries, 'artifacts' => payloads, 'review_status' => 'pending_owner_review'}
  end

  private

  def capture(root, entries, destination)
    Dir.mkdir(destination, 0o700)
    entries.each do |entry|
      source = File.join(root, entry.fetch('path'))
      bytes = File.symlink?(source) ? File.readlink(source).b : File.binread(source)
      raise ArgumentError, 'input changed before capture' unless Digest::SHA256.hexdigest(bytes) == entry.fetch('sha256')
      target = File.join(destination, entry.fetch('path'))
      FileUtils.mkdir_p(File.dirname(target), mode: 0o700)
      # Link spellings are Git blob bytes; scan them as data, never follow them.
      File.open(target, File::WRONLY | File::CREAT | File::EXCL, 0o600) { |file| file.write(bytes) }
    end
  end

  def prepare_scanner(archive, out, pin)
    binary = File.join(out, 'gitleaks')
    found = {}
    Zlib::GzipReader.open(archive) do |gzip|
      Gem::Package::TarReader.new(gzip) do |tar|
        tar.each do |entry|
          name = entry.full_name
          next unless %w[gitleaks LICENSE].include?(name)
          raise ArgumentError, 'scanner archive needs regular, unique binary and notice' if found[name] || !entry.file? || entry.size > 64 * 1024 * 1024
          target = name == 'gitleaks' ? binary : File.join(out, 'gitleaks.LICENSE')
          File.open(target, File::WRONLY | File::CREAT | File::EXCL, name == 'gitleaks' ? 0o700 : 0o600) { |file| file.write(entry.read) }
          found[name] = true
        end
      end
    end
    raise ArgumentError, 'scanner executable or notice absent' unless found.keys.sort == %w[LICENSE gitleaks]
    self.class.verify_pin(File.join(out, 'gitleaks.LICENSE'), pin.fetch('license_sha256'))
    self.class.verify_pin(binary, pin.fetch('binary_sha256'))
    binary
  end

  def scan(binary, config, out, name, mode, target, *extra)
    report = File.join(out, 'secrets-' + name + '.json')
    args = [*ScrubGitContext.command(attributes: @scanner_attributes), binary, mode, target, *extra, '--config', config, '--gitleaks-ignore-path', File::NULL, '--ignore-gitleaks-allow', '--redact=100', '--no-banner', '--no-color', '--report-format=json', '--report-path', report]
    File.open(report, File::WRONLY | File::CREAT | File::EXCL, 0o600) {}
    result = @runner.run!(*args, chdir: out)
    File.chmod(0o600, report)
    File.open(File.join(out, name + '.log'), File::WRONLY | File::CREAT | File::EXCL, 0o600) { |file| file.write(result.out + result.err) }
    raise ArgumentError, 'secret scan failed; see private report directory' unless [0, 1].include?(result.exit_status)
    findings = JSON.parse(File.read(report))
    raise ArgumentError, 'scanner report and exit status disagree' unless findings.is_a?(Array) && (findings.empty? == result.success?)
    findings.length
  end

  def write(out, name, value)
    File.open(File.join(out, name), File::WRONLY | File::CREAT | File::EXCL, 0o600) { |file| file.write(JSON.pretty_generate(value) + "\n") }
  end

  def default_allowlist_match?(path)
    @path_patterns.any? { |pattern| pattern.match?(path) || pattern.match?('content/' + path) }
  end

  def refs(repo)
    git(repo, 'for-each-ref', '--sort=refname', '--format=%(refname) %(objectname)').out.lines.to_h { |line| line.strip.split(' ', 2) }
  end

  def git(repo, *args, check: true)
    @runner.public_send(check ? :run : :run!, *ScrubGitContext.command, 'git', '-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false', '-C', repo, *args, binmode: true)
  end
end
