# frozen_string_literal: true
require 'digest'
require 'json'

class PythonBenchmarkBridge
  attr_reader :manifest

  def initialize(path:, sha256:, runner:)
    bytes = File.binread(path)
    raise ArgumentError, 'preparation manifest digest mismatch' unless /\A[0-9a-f]{64}\z/.match?(sha256) && Digest::SHA256.hexdigest(bytes) == sha256
    @manifest = JSON.parse(bytes)
    raise ArgumentError, 'unsupported preparation bridge contract' unless @manifest.fetch('contract') == 'codemap-python-prepared-tree/v1'
    @runner = runner
  end

  def verify(original:, prepared:, clone:, scorer:, query:)
    original, prepared, clone = [original, prepared, clone].map { |path| File.realpath(path) }
    raise ArgumentError, 'independent checkouts are required' unless [original, prepared, clone].uniq.length == 3
    original_pin, prepared_pin = @manifest.fetch('original_commit'), @manifest.fetch('prepared_commit')
    raise ArgumentError, 'original oracle pin was changed' unless scorer.original_commit == original_pin
    raise ArgumentError, 'prepared tree must have its own commit' if original_pin == prepared_pin
    [original, prepared, clone].each do |repo|
      raise ArgumentError, 'benchmark checkout must be clean' unless git(repo, 'status', '--porcelain', '--untracked-files=all').empty?
      raise ArgumentError, 'benchmark path must be the Git root' unless File.realpath(git(repo, 'rev-parse', '--show-toplevel').strip) == repo
    end
    raise ArgumentError, 'original checkout pin differs from manifest' unless git(original, 'rev-parse', 'HEAD').strip == original_pin
    [prepared, clone].each do |repo|
      raise ArgumentError, 'prepared checkout commit/tree differs from manifest' unless git(repo, 'rev-parse', 'HEAD').strip == prepared_pin && git(repo, 'rev-parse', 'HEAD^{tree}').strip == @manifest.fetch('prepared_tree')
      raise ArgumentError, 'prepared commit must directly descend from original pin' unless git(repo, 'rev-parse', 'HEAD^').strip == original_pin
      raise ArgumentError, 'clone remote differs from original source' unless git(repo, 'config', '--get', 'remote.origin.url') == git(original, 'config', '--get', 'remote.origin.url')
    end
    before = inventory(prepared, original_pin)
    after = inventory(prepared, prepared_pin)
    removed = @manifest.fetch('removed')
    raise ArgumentError, 'removed paths must be a nonempty unique sorted list of declared invalid test data' unless removed.any? && removed == removed.uniq.sort && removed.all? { |path| path.start_with?('tests/data/') && path.end_with?('.py') && path.split('/').none? { |part| ['', '.', '..'].include?(part) } }
    raise ArgumentError, 'preparation must only delete the declared invalid test inputs' unless (before.keys - after.keys).sort == removed && (after.keys - before.keys).empty? && after.all? { |path, blob| before.fetch(path) == blob }
    clone_inventory = inventory(clone, prepared_pin)
    raise ArgumentError, 'equivalent clone tree differs' unless clone_inventory == after
    source_paths = after.keys.select { |path| path.end_with?('.py') } + query.fetch('files').map { |file| file.fetch('path') }.select { |path| path.end_with?('.pyi') }
    raise ArgumentError, 'independent anchors must cover every surviving Python file and selected declaration' unless scorer.files.keys.sort == source_paths.uniq.sort
    scorer.files.each do |path, file|
      raise ArgumentError, 'source anchor was removed during preparation' unless after.key?(path)
      bytes = git(prepared, 'show', "#{prepared_pin}:#{path}")
      raise ArgumentError, 'source anchor bytes differ from original tracked blob' unless Digest::SHA256.hexdigest(bytes) == file.fetch('sha256')
    end
    raise ArgumentError, 'native package profile differs from frozen query' unless query.fetch('package_profile') == @manifest.fetch('package_profile')
    query.fetch('files').each do |file|
      bytes = git(prepared, 'show', "#{prepared_pin}:#{file.fetch('path')}")
      raise ArgumentError, 'native input changed or was removed during preparation' unless after.key?(file['path']) && Digest::SHA256.hexdigest(bytes) == file.fetch('sha256')
    end
    @manifest
  end

  private

  def git(repo, *args)
    environment = ['env', *ENV.keys.grep(/\AGIT_/).flat_map { |key| ['-u', key] },
                   'GIT_OPTIONAL_LOCKS=0', 'GIT_NO_LAZY_FETCH=1', 'GIT_ALLOW_PROTOCOL=']
    base = [*environment, 'git', '--no-pager', '-c', 'core.fsmonitor=false', '-c', 'core.hooksPath=/dev/null',
            '-c', 'log.showSignature=false', '-C', repo]
    config = @runner.run(*base, 'config', '--null', '--name-only', '--list').out
    filters = config.split("\0").select { |key| /\Afilter\..*\.(clean|smudge|process|required)\z/.match?(key) }
    overrides = filters.flat_map do |key|
      raise ArgumentError, 'Git filter cannot be disabled safely' if key.include?('=')
      ['-c', "#{key}=#{key.end_with?('.required') ? 'false' : ''}"]
    end
    @runner.run(*base, *overrides, *args).out
  end

  def inventory(repo, commit)
    git(repo, 'ls-tree', '-r', '-z', commit).split("\0").to_h do |entry|
      metadata, path = entry.split("\t", 2)
      raise ArgumentError, 'malformed Git tree inventory' unless path && metadata
      [path, metadata]
    end
  end
end
