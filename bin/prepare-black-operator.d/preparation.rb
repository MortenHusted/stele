# frozen_string_literal: true
require 'digest'
require 'fileutils'
require 'json'
require 'rubygems/package'
require 'zlib'
require 'zip'
require 'tty-command'

class BlackOperatorPreparation
  FILE_LIMIT = 128 * 1024 * 1024

  def self.relative(name)
    parts = name.delete_suffix('/').split('/', -1)
    raise ArgumentError, 'invalid archive or source path' if parts.empty? || parts.any? { |part| ['', '.', '..'].include?(part) || part.match?(/[\\\0]/) }
    parts.join('/')
  end

  def self.runtime_path(name)
    return unless name == 'python/bin/python3.14' || name.start_with?('python/lib/python3.14/') || name == 'python/lib/python3.14'
    path = relative(name)
    parts = path.split('/')
    return if parts.include?('__pycache__') || path.end_with?('.pyc') || parts[0, 4] == %w[python lib python3.14 site-packages]
    path.delete_prefix('python/')
  end

  def self.verify_pin(path, sha)
    raise ArgumentError, 'archive content pin mismatch' unless /\A[0-9a-f]{64}\z/.match?(sha) && Digest::SHA256.file(path).hexdigest == sha
  end

  def self.write_file(root, name, bytes, mode: 0o644)
    path = File.join(root, relative(name))
    raise ArgumentError, 'duplicate or oversized prepared file' if bytes.bytesize > FILE_LIMIT || File.exist?(path) || File.symlink?(path)
    FileUtils.mkdir_p(File.dirname(path))
    File.open(path, File::WRONLY | File::CREAT | File::EXCL, mode) { |file| file.write(bytes) }
  end

  def self.unpack_runtime(archive, root)
    Zlib::GzipReader.open(archive) do |gzip|
      Gem::Package::TarReader.new(gzip) do |tar|
        tar.each do |entry|
          prefix, leaf = [entry.header.prefix, entry.header.name].map { |field| field.partition("\0").first }
          name = runtime_path(prefix.empty? ? leaf : prefix + '/' + leaf)
          next unless name
          if entry.directory?
            FileUtils.mkdir_p(File.join(root, name))
          else
            raise ArgumentError, 'runtime archive must contain regular selected files' unless entry.file?
            write_file(root, name, entry.read(FILE_LIMIT + 1) || ''.b, mode: name == 'bin/python3.14' ? 0o755 : 0o644)
          end
        end
      end
    end
  end

  def self.unpack_wheel(archive, root)
    Zip::File.open(archive) do |zip|
      zip.each do |entry|
        name = relative(entry.name)
        raise ArgumentError, 'wheel links or special entries are forbidden' unless !entry.symlink? && (entry.file? || entry.directory?)
        if entry.directory?
          FileUtils.mkdir_p(File.join(root, name))
        else
          entry.get_input_stream { |input| write_file(root, name, input.read(FILE_LIMIT + 1) || ''.b) }
        end
      end
    end
  end

  def self.inventory(roots)
    files, directories = {}, {}
    roots.each do |tag, root|
      next unless root
      walk = lambda do |path, logical|
        raise ArgumentError, 'prepared operator must contain only regular files and directories' if File.symlink?(path) || (!File.file?(path) && !File.directory?(path))
        if File.directory?(path)
          entries = Dir.children(path).sort
          directories[logical] = entries.to_h { |name| [name, File.directory?(File.join(path, name)) ? 'directory' : 'file'] }
          entries.each { |name| walk.call(File.join(path, name), logical + '/' + relative(name)) }
        else
          raise ArgumentError, 'prepared file exceeds operator budget' if File.size(path) > FILE_LIMIT
          files[logical] = Digest::SHA256.file(path).hexdigest
        end
      end
      walk.call(root, "{#{tag}}")
    end
    {'files' => files.sort.to_h, 'directories' => directories.sort.to_h}
  end

  def self.runtime_profile(root, directory, prefix, tools)
    directory = File.realpath(directory)
    raise ArgumentError, 'cache prefix must be one nonempty filename component' unless relative(prefix) == prefix && !prefix.include?('/')
    names = Dir.children(directory).select { |name| name.start_with?(prefix) }.sort
    raise ArgumentError, 'selected cache family is empty' if names.empty?
    selected = {'python' => {'path' => File.join(root, 'bin/python3.14'), 'role' => 'python'}}.merge(tools)
    names.each { |name| selected['cache/' + name] = {'path' => File.join(directory, relative(name)), 'role' => 'shared_cache'} }
    files = selected.sort.to_h.transform_values do |file|
      path = file.fetch('path')
      raise ArgumentError, 'runtime image path must be canonical and regular' unless File.realpath(path) == path && File.file?(path)
      file.merge('sha256' => Digest::SHA256.file(path).hexdigest)
    end
    raise ArgumentError, 'cache family membership changed during preparation' unless Dir.children(directory).select { |name| name.start_with?(prefix) }.sort == names
    {'files' => files, 'cache_family' => {'directory' => directory, 'prefix' => prefix}}
  end

  def initialize
    @root = File.expand_path('../..', __dir__)
    @contract = JSON.parse(File.read(File.join(@root, 'benchmarks/python/black-operator.json')))
    source = JSON.parse(File.read(File.join(@root, 'benchmarks/python/black.json'))).fetch('inputs').fetch('query')
    query = File.join(@root, source.fetch('path'))
    self.class.verify_pin(query, source.fetch('sha256'))
    @query = JSON.parse(File.read(query))
    @runner = TTY::Command.new(printer: :null)
  end

  def run(archive:, compiler:, wheels:, cache_directory:, cache_prefix:, out:)
    compiler = File.realpath(compiler)
    out = File.join(File.realpath(File.dirname(out)), File.basename(out))
    raise ArgumentError, 'output must be new and outside Git' if File.exist?(out) || File.symlink?(out) || git(File.dirname(out), 'rev-parse', '--show-toplevel', check: false).success?
    spec = @contract.fetch('compiler')
    raise ArgumentError, 'compiler source must be a clean Git root at the public pin' unless git(compiler, 'rev-parse', 'HEAD').out.strip == spec.fetch('commit') && git(compiler, 'status', '--porcelain', '--untracked-files=all').out.empty? && File.realpath(git(compiler, 'rev-parse', '--show-toplevel').out.strip) == compiler
    raise ArgumentError, 'compiler origin differs from public pin' unless git(compiler, 'config', '--get', 'remote.origin.url').out.strip == spec.fetch('repository')
    self.class.verify_pin(archive, @contract.fetch('python_archive').fetch('sha256'))
    @contract.fetch('wheels').each { |wheel| self.class.verify_pin(File.join(wheels, wheel.fetch('name')), wheel.fetch('sha256')) }
    Dir.mkdir(out, 0o700)
    prepared_runtime, prepared_compiler = %w[runtime compiler].map { |name| path = File.join(out, name);Dir.mkdir(path);path }
    warn 'prepare-black-operator: copying pinned public runtime and compiler inputs'
    self.class.unpack_runtime(archive, prepared_runtime)
    self.class.verify_pin(File.join(prepared_runtime, 'bin/python3.14'), @contract.fetch('python_archive').fetch('interpreter_sha256'))
    copy_compiler(compiler, prepared_compiler, out)
    dependencies = File.join(prepared_compiler, 'dependencies');Dir.mkdir(dependencies)
    @contract.fetch('wheels').each { |wheel| self.class.unpack_wheel(File.join(wheels, wheel.fetch('name')), dependencies) }
    warn 'prepare-black-operator: pinning the explicitly selected host cache family and standard tools'
    profile = self.class.runtime_profile(prepared_runtime, cache_directory, cache_prefix, @contract.fetch('host_tools'))
    config = %w[parser native].to_h do |role|
      roots = {'runtime' => prepared_runtime, 'compiler' => role == 'native' ? prepared_compiler : nil}
      warn "prepare-black-operator: closed input inventory for #{role}"
      [role, {'roots' => roots, 'inputs' => self.class.inventory(roots), 'runtime' => profile}]
    end
    config['native_sources'] = @query.fetch('files').map { |file| file.fetch('path') }.select { |path| path.end_with?('.py') }.sort
    config['declarations'] = @query.fetch('files').map { |file| file.fetch('path') }.select { |path| path.end_with?('.pyi') }.sort
    config['package_profile'] = @query.fetch('package_profile')
    write(out, 'native-config.json', config)
    result = {'contract' => 'stele-black-operator-preparation-result/v1', 'inputs' => @contract, 'operator_inputs_sha256' => %w[parser native].to_h { |role| [role, Digest::SHA256.hexdigest(JSON.generate(config.fetch(role).fetch('inputs')))] }, 'native_config_sha256' => Digest::SHA256.file(File.join(out, 'native-config.json')).hexdigest, 'scope' => 'public operator preparation and content pins only; execution witnesses and semantic acceptance require the guarded benchmark'}
    write(out, 'result.json', result)
    result
  end

  private

  def copy_compiler(source, target, out)
    commit = @contract.fetch('compiler').fetch('commit')
    git(source, 'ls-tree', '-r', '-z', commit, '--', 'mypy', 'mypyc', 'LICENSE').out.split("\0").each do |entry|
      metadata, path = entry.split("\t", 2)
      mode, kind, pin = metadata.split(' ')
      raise ArgumentError, 'compiler source must contain regular Git blobs' unless %w[100644 100755].include?(mode) && kind == 'blob'
      physical = File.join(source, self.class.relative(path))
      raise ArgumentError, 'compiler source must be canonical' unless File.realpath(physical) == physical
      bytes = File.binread(physical, FILE_LIMIT + 1) || ''.b
      raise ArgumentError, 'compiler source differs from pinned Git blob' unless Digest::SHA1.hexdigest("blob #{bytes.bytesize}\0" + bytes) == pin
      root, name = path == 'LICENSE' ? [out, 'LICENSE.mypy'] : [target, path]
      self.class.write_file(root, name, bytes, mode: mode == '100755' ? 0o755 : 0o644)
    end
  end

  def write(out, name, value)
    File.open(File.join(out, name), 'w', 0o600) { |file| file.write(JSON.pretty_generate(value) + "\n") }
  end

  def git(repo, *args, check: true)
    env = ['env', *ENV.keys.grep(/\AGIT_/).flat_map { |key| ['-u', key] }, 'GIT_CONFIG_NOSYSTEM=1', 'GIT_CONFIG_GLOBAL=/dev/null', 'GIT_NO_LAZY_FETCH=1']
    method = check ? :run : :run!
    @runner.public_send(method, *env, 'git', '-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false', '-C', repo, *args)
  end
end
