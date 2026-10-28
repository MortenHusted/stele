# frozen_string_literal: true
require 'digest'
require 'fileutils'
require 'json'
require 'rubygems/package'
require 'zlib'

class ReleaseNotices
  PROJECT_FILES = %w[LICENSE NOTICE THIRD_PARTY_NOTICES.md
                     crates/codemap-topology/NOTICE crates/codemap-topology/VENDORED.md
                     crates/codemap-topology/LICENSE.apache-2.0 crates/codemap-topology/LICENSE.d3-ISC
                     crates/codemap/src/queries/rust/LICENSE-tree-sitter-rust.txt].freeze
  REGISTRY = 'registry+https://github.com/rust-lang/crates.io-index'

  def initialize(repo:, cargo_home:)
    @repo, @cargo_home = [repo, cargo_home].map { |path| File.realpath(path) }
    @supplements = JSON.parse(File.read(File.join(@repo, 'publication/dependency-notices.json'))).fetch('packages')
    @lock_bytes = File.binread(File.join(@repo, 'Cargo.lock'))
    @locked = @lock_bytes.split(/^\[\[package\]\]\r?\n/).drop(1).to_h do |table|
      values = %w[name version source checksum].to_h do |key|
        match = table.match(/^#{key} = ("[^\r\n]*")$/)
        [key, match && JSON.parse(match[1])]
      end
      [[values['name'], values['version'], values['source']], values['checksum']]
    end
  end

  def write(stage, metadata, sysroot: nil, compiler: nil)
    raise ArgumentError, 'metadata belongs to another workspace' unless File.realpath(metadata.fetch('workspace_root')) == @repo
    project = PROJECT_FILES.to_h do |path|
      physical = File.realpath(File.join(@repo, path))
      raise ArgumentError, 'project notice leaves the source tree' unless physical.start_with?(@repo + '/')
      output = path.include?('/') ? 'licenses/project/' + path : path
      [output, put(stage, output, File.binread(physical))]
    end
    packages = normal_packages(metadata).select { |package| package['source'] }.sort_by { |package| [package.fetch('name'), package.fetch('version')] }.map do |package|
      collect(stage, package)
    end
    raise ArgumentError, 'Cargo.lock changed during notice collection' unless File.binread(File.join(@repo, 'Cargo.lock')) == @lock_bytes
    rust = sysroot && {'compiler' => compiler, 'source' => 'https://github.com/rust-lang/rust', 'files' => rust_notices(stage, sysroot)}
    result = {'rust_standard_library' => rust, 'contract' => 'stele-release-notices/v1', 'scope' => 'target-filtered normal dependency closure from stele; includes proc-macro packages and is not a minimal linker map', 'review_status' => 'pending_owner_review', 'cargo_lock_sha256' => Digest::SHA256.hexdigest(@lock_bytes), 'project' => project, 'packages' => packages}
    put(stage, 'THIRD_PARTY_INVENTORY.json', JSON.pretty_generate(result) + "\n")
    result
  end

  private

  def rust_notices(stage, sysroot)
    root = File.realpath(File.join(sysroot, 'share/doc/rust'))
    paths = ['COPYRIGHT-library.html', *Dir.glob(File.join(root, 'licenses/*')).map { |path| path.delete_prefix(root + '/') }]
    raise ArgumentError, 'prepare the Rust standard-library notices' if paths.length < 2
    paths.sort.to_h do |path|
      physical = File.realpath(File.join(root, path))
      raise ArgumentError, 'Rust notice leaves the configured toolchain' unless physical.start_with?(root + '/') && File.file?(physical)
      [path, put(stage, 'licenses/rust/' + path, File.binread(physical))]
    end
  end

  def normal_packages(metadata)
    packages = metadata.fetch('packages').to_h { |package| [package.fetch('id'), package] }
    roots = packages.values.select { |package| package.fetch('name') == 'stele' && !package['source'] }
    raise ArgumentError, 'metadata must identify one owned stele package' unless roots.length == 1
    nodes = metadata.fetch('resolve').fetch('nodes').to_h { |node| [node.fetch('id'), node] }
    seen, queue = {}, [roots.first.fetch('id')]
    until queue.empty?
      id = queue.shift
      next if seen[id]
      seen[id] = true
      nodes.fetch(id).fetch('deps').each do |dependency|
        queue << dependency.fetch('pkg') if dependency.fetch('dep_kinds').any? { |kind| kind['kind'].nil? }
      end
    end
    seen.keys.map { |id| packages.fetch(id) }
  end

  def relative(path)
    raise ArgumentError, 'notice path must be repository relative' if path.empty? || path.split('/', -1).any? { |part| ['', '.', '..'].include?(part) } || path.match?(/[\\\0]/)
    path
  end

  def put(stage, path, bytes)
    destination = File.join(stage, relative(path))
    FileUtils.mkdir_p(File.dirname(destination))
    File.open(destination, File::WRONLY | File::CREAT | File::EXCL, 0o644) { |file| file.write(bytes) }
    Digest::SHA256.hexdigest(bytes)
  end

  def legal_path?(path, declared)
    return true if path == declared
    parts = path.split('/')
    parts.any? { |part| /\Alicen[cs]es?\z/i.match?(part) } || /\A(?:licen[cs]e|copying|copyright|notice|authors)(?:[._-].*)?\z/i.match?(parts.last)
  end

  def collect(stage, package)
    name, version, source = package.values_at('name', 'version', 'source')
    raise ArgumentError, 'unsupported dependency source or identity' unless source == REGISTRY && /\A[A-Za-z0-9_-]+\z/.match?(name) && /\A[0-9][A-Za-z0-9.+-]*\z/.match?(version)
    checksum = @locked.fetch([name, version, source])
    raise ArgumentError, 'dependency lacks a locked archive checksum' unless /\A[0-9a-f]{64}\z/.match?(checksum.to_s)
    candidates = Dir.glob(File.join(@cargo_home, 'registry/cache/*', name + '-' + version + '.crate')).sort
    archive = candidates.find { |path| Digest::SHA256.file(path).hexdigest == checksum }
    raise ArgumentError, "prepare the locked public crate archive: #{name} #{version}" unless archive
    declared = package['license_file']
    if declared && declared.start_with?('/')
      prefix = File.dirname(package.fetch('manifest_path')) + '/'
      raise ArgumentError, 'declared license leaves its package' unless declared.start_with?(prefix)
      declared = declared.delete_prefix(prefix)
    end
    relative(declared) if declared
    files, vcs, primary = {}, nil, false
    prefix = name + '-' + version + '/'
    Zlib::GzipReader.open(archive) do |gzip|
      Gem::Package::TarReader.new(gzip) do |tar|
        tar.each do |entry|
          next if entry.directory?
          raise ArgumentError, 'crate member leaves its package' unless entry.full_name.start_with?(prefix)
          path = relative(entry.full_name.delete_prefix(prefix))
          next unless path == '.cargo_vcs_info.json' || legal_path?(path, declared)
          raise ArgumentError, 'notice must be a regular bounded file' unless entry.file? && entry.size <= 8 * 1024 * 1024
          bytes = entry.read
          if path == '.cargo_vcs_info.json'
            vcs = JSON.parse(bytes)
          else
            files[path] = put(stage, 'licenses/' + name + '-' + version + '/' + path, bytes)
            primary ||= path == declared || (!path.include?('/') && /\A(?:licen[cs]e|copying)(?:[._-].*)?\z/i.match?(path))
          end
        end
      end
    end
    supplement = nil
    unless primary
      supplement = @supplements.fetch(name) { raise ArgumentError, "primary notice needs review: #{name} #{version}" }
      raise ArgumentError, 'notice supplement differs from locked package or VCS commit' unless supplement.fetch('version') == version && supplement.fetch('crate_sha256') == checksum && vcs&.dig('git', 'sha1') == supplement.fetch('commit')
      path = File.realpath(File.join(@repo, relative(supplement.fetch('path'))))
      raise ArgumentError, 'supplement leaves source or content pin differs' unless path.start_with?(@repo + '/') && Digest::SHA256.file(path).hexdigest == supplement.fetch('sha256')
      files['LICENSE.upstream'] = put(stage, 'licenses/' + name + '-' + version + '/LICENSE.upstream', File.binread(path))
    end
    {'name' => name, 'version' => version, 'source' => 'https://crates.io/crates/' + name + '/' + version, 'archive_sha256' => checksum, 'declared_license' => package['license'], 'notices' => files.sort.to_h, 'supplement' => supplement, 'review_status' => 'pending_owner_review'}
  end
end
