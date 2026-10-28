# frozen_string_literal: true
require 'digest'
require 'json'
require 'tty-command'

class JsTsPreparation
  def self.physical(logical, roots)
    name, *parts = logical.split('/', -1)
    root = roots[name.delete_prefix('{').delete_suffix('}')]
    valid = %w[{source} {typescript}].include?(name) && root && parts.all? { |part| !['', '.', '..'].include?(part) && !part.match?(/[\\\0]/) }
    raise ArgumentError, 'invalid logical input path' unless valid && !(name == '{source}' && parts.first == '.git')
    File.join(root, *parts)
  end

  def self.logical(path, roots)
    roots.each do |name, root|
      next unless path == root || path.start_with?(root + '/')
      value = "{#{name}}" + path.delete_prefix(root)
      physical(value, roots)
      return value
    end
    raise ArgumentError, 'input alias escapes declared roots'
  end

  def self.verify_inputs(inputs, roots)
    raise ArgumentError, 'source and compiler roots must be disjoint' if roots['source'].start_with?(roots['typescript'] + '/') || roots['typescript'].start_with?(roots['source'] + '/') || roots['source'] == roots['typescript']
    inputs.fetch('files').each do |name, sha|
      path = physical(name, roots)
      logical(File.realpath(path), roots)
      raise ArgumentError, 'frozen file hash differs' unless File.file?(path) && Digest::SHA256.file(path).hexdigest == sha
    end
    inputs.fetch('directories').each do |name, expected|
      path = physical(name, roots)
      raise ArgumentError, 'inventoried directory must be canonical' unless File.directory?(path) && File.realpath(path) == path
      actual = Dir.children(path).filter_map do |child|
        next if name == '{source}' && child == '.git'
        entry = File.join(path, child)
        raise ArgumentError, 'nonregular directory entry' unless File.file?(entry) || File.directory?(entry)
        value = {'kind' => File.directory?(entry) ? 'directory' : 'file'}
        value['target'] = logical(File.realpath(entry), roots) if File.symlink?(entry)
        [child, value]
      end.to_h
      raise ArgumentError, 'frozen directory membership or alias differs' unless actual == expected
    end
  end

  def initialize
    @root = File.expand_path('../..', __dir__)
    @contract = JSON.parse(File.read(File.join(@root, 'benchmarks/js_ts/excalidraw.json')))
    @inputs = @contract.fetch('inputs').to_h do |name, pin|
      path = File.join(@root, pin.fetch('path'))
      raise ArgumentError, 'frozen preparation input drifted' unless Digest::SHA256.file(path).hexdigest == pin.fetch('sha256')
      [name, JSON.parse(File.read(path))]
    end
    @runner = TTY::Command.new(printer: :null)
  end

  def run(repo:, bundle:, runtime:, out:)
    repo, bundle = [repo, bundle].map { |path| File.realpath(path) }
    out = File.join(File.realpath(File.dirname(out)), File.basename(out))
    raise ArgumentError, 'output must be new and outside Git' if File.exist?(out) || File.symlink?(out) || @runner.run!('git', '-C', File.dirname(out), 'rev-parse', '--show-toplevel').success?
    raise ArgumentError, 'source must be a clean Git root at the public pin' unless git(repo, 'rev-parse', 'HEAD').strip == @contract.fetch('commit') && git(repo, 'status', '--porcelain', '--untracked-files=all').empty? && File.realpath(git(repo, 'rev-parse', '--show-toplevel').strip) == repo
    raise ArgumentError, 'source origin differs from public pin' unless git(repo, 'config', '--get', 'remote.origin.url').strip == @contract.fetch('repository')
    raise ArgumentError, 'frozen source identities disagree' unless @inputs.fetch('oracle').fetch('commit') == @contract.fetch('commit') && @inputs.fetch('anchors').fetch('commit') == @contract.fetch('commit')
    @contract.fetch('bundle').each do |name, sha|
      raise ArgumentError, 'operator bundle differs from canonical tool files' unless Digest::SHA256.file(File.join(bundle, name)).hexdigest == sha
    end
    roots = {'source' => repo, 'typescript' => File.realpath(File.join(bundle, 'node_modules/typescript/lib'))}
    warn 'prepare-js-ts: verifying frozen files, directory membership and aliases'
    self.class.verify_inputs(@inputs.fetch('inputs'), roots)
    @inputs.fetch('anchors').fetch('files').each do |file|
      raise ArgumentError, 'source anchor differs from public checkout' unless Digest::SHA256.file(self.class.physical('{source}/' + file.fetch('path'), roots)).hexdigest == file.fetch('sha256')
    end
    runtime_bytes = File.binread(runtime)
    runtime_value = JSON.parse(runtime_bytes)
    raise ArgumentError, 'operator runtime profile must be a JSON object' unless runtime_value.is_a?(Hash)
    environment = {'roots' => roots, 'inputs' => @inputs.fetch('inputs'), 'bundle' => bundle, 'runtime' => runtime_value}
    Dir.mkdir(out, 0o700)
    write(out, 'environment.json', environment)
    result = {'contract' => 'stele-js-ts-source-preparation-result/v1', 'commit' => @contract.fetch('commit'), 'project' => @contract.fetch('project'), 'inputs' => @contract.fetch('inputs'), 'bundle' => @contract.fetch('bundle'), 'runtime_profile_sha256' => Digest::SHA256.hexdigest(runtime_bytes), 'environment_sha256' => Digest::SHA256.file(File.join(out, 'environment.json')).hexdigest, 'scope' => 'frozen source/input preparation only; runtime pins supplied unchanged, not validated as execution witnesses; no semantic acceptance'}
    write(out, 'result.json', result)
    result
  end

  private

  def write(out, name, value)
    File.open(File.join(out, name), 'w', 0o600) { |file| file.write(JSON.pretty_generate(value) + "\n") }
  end

  def git(repo, *args)
    environment = ['env', *ENV.keys.grep(/\AGIT_/).flat_map { |key| ['-u', key] }, 'GIT_CONFIG_NOSYSTEM=1', 'GIT_CONFIG_GLOBAL=/dev/null', 'GIT_NO_LAZY_FETCH=1']
    @runner.run(*environment, 'git', '-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false', '-C', repo, *args).out
  end
end
