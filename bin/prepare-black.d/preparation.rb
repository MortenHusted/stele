# frozen_string_literal: true
require 'base64'
require 'digest'
require 'json'
require 'tty-command'

class BlackPreparation
  def self.verify_rejections(expected, observed)
    raise ArgumentError, 'parser rejection set differs from frozen removals' unless observed.sort == expected
  end

  def self.verify_removals(removed, protected)
    valid = removed.any? && removed == removed.uniq.sort && removed.all? do |path|
      path.start_with?('tests/data/') && path.end_with?('.py') && path.split('/').none? { |part| ['', '.', '..'].include?(part) }
    end
    raise ArgumentError, 'invalid frozen removal list or overlap with protected inputs' unless valid && (removed & protected).empty?
  end

  def self.materialize(files, root)
    root = File.realpath(root)
    files.map do |file|
      path = file.fetch('path')
      raise ArgumentError, 'invalid source path' if path.start_with?('/') || path.split('/').any? { |part| ['', '.', '..'].include?(part) }
      source = File.join(root, path)
      raise ArgumentError, 'source must be a regular file inside its checkout' unless File.file?(source) && File.realpath(source).start_with?(root + '/') && !File.symlink?(source)
      bytes = File.binread(source)
      raise ArgumentError, 'source hash differs from frozen input' unless Digest::SHA256.hexdigest(bytes) == file.fetch('sha256')
      file.merge('source_base64' => Base64.strict_encode64(bytes))
    end
  end

  def initialize
    @root = File.expand_path('../..', __dir__)
    @contract = JSON.parse(File.read(File.join(@root, 'benchmarks/python/black.json')))
    @inputs = @contract.fetch('inputs').to_h do |name, input|
      path = File.join(@root, input.fetch('path'))
      raise ArgumentError, 'frozen preparation input drifted' unless Digest::SHA256.file(path).hexdigest == input.fetch('sha256')
      [name, path]
    end
    @runner = TTY::Command.new(printer: :null)
  end

  def run(repo:, python:, out:)
    repo = File.realpath(repo)
    out = File.join(File.realpath(File.dirname(out)), File.basename(out))
    raise ArgumentError, 'output must be new and outside Git' if File.exist?(out) || File.symlink?(out) || @runner.run!('git', '-C', File.dirname(out), 'rev-parse', '--show-toplevel').success?
    original = @contract.fetch('original_commit')
    raise ArgumentError, 'input must be a clean Git root at the public pin' unless git(repo, 'rev-parse', 'HEAD').strip == original && git(repo, 'status', '--porcelain', '--untracked-files=all').empty? && File.realpath(git(repo, 'rev-parse', '--show-toplevel').strip) == repo
    raise ArgumentError, 'source origin differs from public pin' unless git(repo, 'config', '--get', 'remote.origin.url').strip == @contract.fetch('repository').fetch('origin')
    oracle = JSON.parse(File.read(@inputs.fetch('oracle')))
    anchors = JSON.parse(File.read(@inputs.fetch('anchors')))
    query = JSON.parse(File.read(@inputs.fetch('query')))
    raise ArgumentError, 'frozen source identities disagree' unless oracle.fetch('source_commit') == original && anchors.fetch('original_commit') == original && anchors.fetch('parser_helper_sha256') == @contract.fetch('inputs').fetch('probe').fetch('sha256') && query.fetch('package_profile') == @contract.fetch('package_profile')
    protected = anchors.fetch('files').map { |file| file.fetch('path') } + query.fetch('files').map { |file| file.fetch('path') } + oracle.fetch('calls').map { |call| call.fetch('path') } + oracle.fetch('non_call_decoys').map { |call| call.fetch('path') }
    removed = @contract.fetch('removed')
    self.class.verify_removals(removed, protected)
    inventory = git(repo, 'ls-tree', '-r', '-z', original).split("\0").filter_map do |entry|
      metadata, path = entry.split("\t", 2)
      next unless path.end_with?('.py', '.pyi')
      raise ArgumentError, 'Python input must be a regular tracked file' unless metadata.start_with?('100644 blob ', '100755 blob ')
      {'path' => path, 'sha256' => Digest::SHA256.hexdigest(git(repo, 'show', "#{original}:#{path}"))}
    end
    anchored_paths = inventory.map { |file| file['path'] }.select { |path| path.end_with?('.py') } - removed
    anchored_paths += query.fetch('files').map { |file| file['path'] }.select { |path| path.end_with?('.pyi') }
    raise ArgumentError, 'frozen anchors do not cover every kept module and queried declaration' unless anchored_paths.uniq.sort == anchors.fetch('files').map { |file| file['path'] }.sort
    self.class.materialize(anchors.fetch('files').map { |file| file.slice('path', 'sha256') }, repo)
    local_query = query.merge('files' => self.class.materialize(query.fetch('files'), repo))
    Dir.mkdir(out, 0o700)
    write(out, 'parser-inputs.json', inventory)
    warn 'prepare-black: checking every tracked Python input with the pinned parser'
    parsed = @runner.run(python, '-I', '-B', File.join(__dir__, 'check_parser.py'), @inputs.fetch('probe'), File.join(out, 'parser-inputs.json'), repo)
    observed = JSON.parse(parsed.out)
    self.class.verify_rejections(removed, observed.fetch('rejected'))
    prepared = File.join(out, 'prepared')
    git(nil, 'clone', '--quiet', '--no-local', '--depth', '1', '--single-branch', '--no-tags', '--no-checkout', repo, prepared)
    git(prepared, 'checkout', '--quiet', '--detach', original)
    git(prepared, 'rm', '--quiet', '--', *removed)
    tree = git(prepared, 'write-tree').strip
    raise ArgumentError, 'prepared tree differs from the frozen identity' unless tree == @contract.fetch('prepared_tree')
    identity = {'GIT_AUTHOR_NAME' => 'Benchmark Preparation', 'GIT_AUTHOR_EMAIL' => 'benchmark@example.invalid', 'GIT_AUTHOR_DATE' => '2000-01-01T00:00:00Z', 'GIT_COMMITTER_NAME' => 'Benchmark Preparation', 'GIT_COMMITTER_EMAIL' => 'benchmark@example.invalid', 'GIT_COMMITTER_DATE' => '2000-01-01T00:00:00Z'}
    commit = git(prepared, 'commit-tree', tree, '-p', original, '-m', 'Prepare public Python parser corpus', env: identity).strip
    raise ArgumentError, 'prepared commit differs from the frozen identity' unless commit == @contract.fetch('prepared_commit')
    git(prepared, 'reset', '--quiet', '--hard', commit)
    git(prepared, 'remote', 'set-url', 'origin', @contract.fetch('repository').fetch('origin'))
    clone = File.join(out, 'clone')
    git(nil, 'clone', '--quiet', '--no-local', '--depth', '2', '--single-branch', '--no-tags', prepared, clone)
    git(clone, 'remote', 'set-url', 'origin', @contract.fetch('repository').fetch('origin'))
    raise ArgumentError, 'prepared clone identity differs' unless git(clone, 'rev-parse', 'HEAD').strip == commit && git(clone, 'rev-parse', 'HEAD^{tree}').strip == tree
    [prepared, clone].each do |checkout|
      raise ArgumentError, 'prepared checkout must remain clean' unless git(checkout, 'status', '--porcelain', '--untracked-files=all').empty?
      self.class.materialize(inventory.reject { |file| removed.include?(file.fetch('path')) }, checkout)
    end
    bridge = @contract.slice('original_commit', 'prepared_tree', 'removed', 'repository', 'package_profile').merge('contract' => 'codemap-python-prepared-tree/v1', 'prepared_commit' => commit)
    write(out, 'bridge.json', bridge)
    write(out, 'query.json', local_query)
    result = {'contract' => 'stele-black-source-preparation-result/v1', 'prepared_commit' => commit, 'prepared_tree' => tree, 'removed' => removed.length, 'kept' => observed.fetch('kept'), 'scope' => 'source preparation and parser witnesses only; no native semantic acceptance', 'inputs' => @contract.fetch('inputs'), 'outputs' => %w[bridge.json query.json].to_h { |name| [name, Digest::SHA256.file(File.join(out, name)).hexdigest] }}
    write(out, 'result.json', result)
    result
  end

  private

  def write(out, name, value)
    File.open(File.join(out, name), 'w', 0o600) { |file| file.write(JSON.pretty_generate(value) + "\n") }
  end

  def git(repo, *args, env: {})
    prefix = ['env', *ENV.keys.grep(/\AGIT_/).flat_map { |key| ['-u', key] }]
    prefix += ['GIT_CONFIG_NOSYSTEM=1', 'GIT_CONFIG_GLOBAL=/dev/null', 'GIT_NO_LAZY_FETCH=1']
    prefix += env.map { |key, value| "#{key}=#{value}" }
    prefix += ['git', '-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false', '-c', 'commit.gpgSign=false']
    prefix += ['-C', repo] if repo
    @runner.run(*prefix, *args).out
  end
end
