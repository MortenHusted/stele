# frozen_string_literal: true
require 'digest'
require 'json'
require 'rbconfig'
require 'tty-command'

class CppBenchmarkPreparation
  attr_reader :contract

  def initialize
    @root = File.expand_path('../..', __dir__)
    @contract = JSON.parse(File.read(File.join(@root, 'benchmarks/cpp/double-conversion.json')))
    @runner = TTY::Command.new(printer: :null)
    %w[oracle application_oracle].each do |name|
      pin = @contract.fetch(name)
      self.class.verify_pin(File.join(@root, pin.fetch('path')), pin.fetch('sha256'))
    end
  end

  def self.verify_pin(path, sha)
    raise ArgumentError, 'declared input content changed' unless Digest::SHA256.file(path).hexdigest == sha
  end

  def self.relative(path)
    raise ArgumentError, 'invalid relative path' if path.empty? || path.split('/', -1).any? { |part| ['', '.', '..'].include?(part) } || path.match?(/[\\\0]/)
    path
  end

  def self.new_output(out, runner)
    out = File.join(File.realpath(File.dirname(out)), File.basename(out))
    raise ArgumentError, 'output must be new and outside Git' if File.exist?(out) || File.symlink?(out) || runner.run!('git', '-C', File.dirname(out), 'rev-parse', '--show-toplevel').success?
    Dir.mkdir(out, 0o700)
    out
  end

  def verify_source(repo)
    repo = File.realpath(repo)
    raise ArgumentError, 'source must be a clean Git root at the public commit' unless git(repo, 'rev-parse', 'HEAD') == @contract.fetch('commit') && git(repo, 'status', '--porcelain', '--untracked-files=all').empty? && File.realpath(git(repo, 'rev-parse', '--show-toplevel')) == repo
    raise ArgumentError, 'source origin must match the public repository' unless git(repo, 'config', '--get', 'remote.origin.url') == @contract.fetch('repository')
    @contract.fetch('source_files').each { |path, sha| self.class.verify_pin(File.join(repo, self.class.relative(path)), sha) }
    repo
  end

  def source_manifest(repo)
    {'files' => @contract.fetch('source_files').map { |path, sha| {'path' => path, 'sha256' => sha, 'text' => File.read(File.join(repo, path))} }}
  end

  def run(repo:, clangd:, sdk:, resource:, cxx:, cache_directory:, cache_prefix:, out:)
    raise ArgumentError, 'this recipe is tested only on macOS arm64' unless RbConfig::CONFIG['host_os'].include?('darwin') && RbConfig::CONFIG['host_cpu'] == 'arm64'
    repo = verify_source(repo)
    clangd, sdk, resource, cxx = [clangd, sdk, resource, cxx].map { |path| File.realpath(path) }
    verify_sdk(sdk)
    self.class.verify_pin(clangd, @contract.fetch('operator').fetch('image_pins').fetch('clangd'))
    out = self.class.new_output(out, @runner)
    warn 'prepare-cpp-benchmark: inventorying declared SDK and headers; application commands are never executed'
    roots = {'source' => repo, 'build' => nil, 'sysroot' => sdk, 'dependencies' => [['resource', resource], ['cxx', cxx]]}
    inputs = inventory(repo, {'sysroot' => sdk, 'resource' => resource, 'cxx' => cxx})
    runtime = runtime_manifest(clangd, cache_directory, cache_prefix)
    verify_operator(runtime)
    commands = @contract.fetch('translation_units').map do |unit|
      file = File.join(repo, unit)
      {'directory' => repo, 'file' => file, 'arguments' => ['clang++', '-std=' + @contract.fetch('standard'), '-x', 'c++', '--target=' + @contract.fetch('target'), '-isysroot', sdk, '-resource-dir', resource, '-nostdinc', '-nostdinc++', '-isystem', cxx, '-isystem', File.join(resource, 'include'), '-isystem', File.join(sdk, 'usr/include'), '-iframework', File.join(sdk, 'System/Library/Frameworks'), '-I', repo, '-c', file]}
    end
    write(out, 'compile_commands.json', commands)
    write(out, 'runtime.json', runtime)
    write(out, 'source-manifest.json', source_manifest(repo))
    profile = {'compilation_database' => File.join(out, 'compile_commands.json'), 'translation_units' => @contract.fetch('translation_units'), 'query_lanes' => 1, 'roots' => roots, 'inputs' => {'files' => inputs}, 'runtime' => runtime}
    write(out, 'profile.json', profile)
    {'contract' => 'stele-cpp-preparation-result/v1', 'profile' => File.join(out, 'profile.json'), 'sdk_version' => @contract.fetch('operator').fetch('sdk_version'), 'scope' => 'data preparation only; guarded execution and acceptance require cpp-public-benchmark'}
  end

  def verify_sdk(sdk)
    settings = JSON.parse(File.read(File.join(sdk, 'SDKSettings.json')))
    operator = @contract.fetch('operator')
    raise ArgumentError, 'SDK differs from tested macOS 27.0 profile' unless settings.fetch('Version') == operator.fetch('sdk_version') && settings.fetch('CanonicalName') == operator.fetch('sdk_canonical_name')
  end

  def verify_operator(runtime)
    @contract.fetch('operator').fetch('image_pins').each do |name, sha|
      entry = runtime.fetch('files').fetch(name)
      raise ArgumentError, 'operator differs from pinned public LLVM distribution' unless entry.fetch('sha256') == sha
      self.class.verify_pin(entry.fetch('path'), sha)
    end
  end

  def self.write(out, name, value)
    File.open(File.join(out, name), File::WRONLY | File::CREAT | File::EXCL, 0o600) { |file| file.write(JSON.pretty_generate(value) + "\n") }
  end

  private

  def write(out, name, value)
    self.class.write(out, name, value)
  end

  def git(repo, *args)
    env = ['env', *ENV.keys.grep(/\AGIT_/).flat_map { |key| ['-u', key] }, 'GIT_CONFIG_NOSYSTEM=1', 'GIT_CONFIG_GLOBAL=/dev/null', 'GIT_NO_LAZY_FETCH=1']
    @runner.run(*env, 'git', '-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false', '-C', repo, *args).out.strip
  end

  def inventory(repo, dependencies)
    files = {}
    roots = [repo, *dependencies.values].map { |path| File.realpath(path) }
    pin = lambda do |path, logical|
      real = File.realpath(path)
      raise ArgumentError, 'input alias leaves declared roots' unless roots.any? { |root| real == root || real.start_with?(root + '/') }
      raise ArgumentError, 'profile inputs must be regular' unless File.file?(real)
      files[logical] = Digest::SHA256.file(real).hexdigest
    end
    git(repo, 'ls-files', '-z').split("\0").each { |path| pin.call(File.join(repo, self.class.relative(path)), '{source}/' + path) }
    dependencies.each do |tag, root|
      walk = lambda do |path, logical, ancestors|
        real = File.realpath(path)
        raise ArgumentError, 'input alias leaves declared roots' unless roots.any? { |allowed| real == allowed || real.start_with?(allowed + '/') }
        if File.directory?(real)
          return if ancestors.include?(real)
          Dir.children(path).sort.each { |name| walk.call(File.join(path, name), logical + '/' + self.class.relative(name), ancestors + [real]) }
        else
          pin.call(path, logical)
        end
      end
      walk.call(root, "{#{tag}}", [])
    end
    files.sort.to_h
  end

  def runtime_manifest(clangd, directory, prefix)
    self.class.relative(prefix)
    raise ArgumentError, 'cache prefix must be one filename component' if prefix.include?('/')
    directory = File.realpath(directory)
    names = Dir.children(directory).select { |name| name.start_with?(prefix) }.sort
    raise ArgumentError, 'selected cache family is empty' if names.empty?
    selected = {'clangd' => {'path' => clangd, 'role' => 'clangd'}}
    %w[dwarfdump sample vmmap].each { |name| selected['inspector/' + name] = {'path' => '/usr/bin/' + name, 'role' => 'inspector'} }
    selected['guard/sandbox-exec'] = {'path' => '/usr/bin/sandbox-exec', 'role' => 'sandbox'}
    queue, seen = [clangd], []
    until queue.empty?
      path = queue.shift
      next if seen.include?(path)
      seen << path
      # otool inspects bytes; no inspected executable or application command runs.
      load_commands = @runner.run('/usr/bin/otool', '-l', path).out
      rpaths = load_commands.scan(/cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset \d+\)/).flatten.map do |base|
        base.sub('@loader_path', File.dirname(path)).sub('@executable_path', File.dirname(clangd))
      end
      lines = @runner.run('/usr/bin/otool', '-L', path).out.lines.drop(1)
      lines.each do |line|
        dependency = line.strip.split(' (', 2).first
        next if dependency.start_with?('/usr/lib/', '/System/Library/')
        if dependency.start_with?('@rpath/')
          candidates = rpaths.map { |base| File.join(base, dependency.delete_prefix('@rpath/')) }.select { |candidate| File.file?(candidate) }.map { |candidate| File.realpath(candidate) }.uniq
          raise ArgumentError, 'runtime rpath must identify one installed library' unless candidates.length == 1
          dependency = candidates.first
        end
        dependency = File.join(File.dirname(path), dependency.delete_prefix('@loader_path/')) if dependency.start_with?('@loader_path/')
        raise ArgumentError, 'runtime dependency needs an explicit absolute install name' unless dependency.start_with?('/')
        dependency = File.realpath(dependency)
        next if dependency == path
        key = 'library/' + File.basename(dependency)
        raise ArgumentError, 'runtime library basename collision' if selected[key] && selected[key]['path'] != dependency
        selected[key] = {'path' => dependency, 'role' => 'library'}
        queue << dependency
      end
    end
    names.each { |name| selected['cache/' + name] = {'path' => File.join(directory, name), 'role' => 'shared_cache'} }
    warn 'prepare-cpp-benchmark: hashing actual operator images and the complete explicit cache family'
    files = selected.sort.to_h.transform_values do |entry|
      path = entry.fetch('path')
      raise ArgumentError, 'runtime paths must be canonical regular files' unless File.realpath(path) == path && File.file?(path)
      entry.merge('sha256' => Digest::SHA256.file(path).hexdigest)
    end
    raise ArgumentError, 'cache family membership changed' unless Dir.children(directory).select { |name| name.start_with?(prefix) }.sort == names
    {'files' => files, 'cache_family' => {'directory' => directory, 'prefix' => prefix}}
  end
end
