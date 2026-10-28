# frozen_string_literal: true
require 'digest'
require 'json'
require 'pathname'
require 'tty-command'

# Local observations: no target command runs unless --execute was requested.
class EvidenceStatus
  class ChangedInputs < StandardError; end
  KINDS = %w[file_present snapshot_query gate_attestation gate_execution].freeze

  def initialize(repo:, plan:, config:, execute: false)
    @repo = File.realpath(repo)
    @plan = File.expand_path(plan)
    @config = File.expand_path(config)
    @declarations = File.binread(@config)
    @data = JSON.parse(@declarations)
    @execute = execute
    @runner = TTY::Command.new(printer: :null)
    raise ArgumentError, 'unsupported probe contract' unless @data['version'] == 'evidence-status/v1'
    raise ArgumentError, 'repository_id is required' unless @data['repository_id'].is_a?(String) && !@data['repository_id'].empty?
    @probes = @data.fetch('probes').sort_by { |p| p.fetch('id') }
    raise ArgumentError, 'probe IDs must be unique' unless @probes.map { |p| p['id'] }.uniq.size == @probes.size
    raise ArgumentError, 'declare at least one probe' if @probes.empty?
    @probes.each { |p| validate_probe(p) }
  end

  def identity
    raise ChangedInputs, 'probe declarations changed; reload before checking' unless File.binread(@config) == @declarations
    # Git lists paths only. Reading bytes avoids executing project clean filters.
    paths = @runner.run('git', '-C', @repo, 'ls-files', '--cached', '--others', '--exclude-standard', '-z').out.split("\0").uniq.sort
    source = paths.map do |path|
      full = File.join(@repo, path)
      stat = File.lstat(full) rescue nil
      content = if stat&.symlink?
                  ['symlink', File.readlink(full)]
                elsif stat&.file?
                  ['file', stat.mode & 0o111, Digest::SHA256.file(full).hexdigest]
                elsif stat.nil?
                  ['missing']
                else
                  raise ArgumentError, 'submodules and special files require an explicit input manifest'
                end
      [path, content]
    end
    inputs = Array(@data['tool_inputs']).map { |path| [path, file_digest(resolve_input(path))] }
    @probes.each do |probe|
      if probe['kind'] == 'gate_execution'
        inputs << ["executable:#{probe['id']}", file_digest(executable(probe.fetch('command').first))]
      elsif probe['kind'] == 'snapshot_query'
        inputs << ["snapshot:#{probe['id']}", file_digest(resolve_input(probe.fetch('path')))]
        inputs << ["codemap:#{probe['id']}", file_digest(snapshot_tool(probe.fetch('codemap')))]
      elsif probe['kind'] == 'file_present'
        path = source_path(probe.fetch('path'))
        state = File.file?(path) ? ['file', file_digest(path)] : ['absent']
        inputs << ["presence:#{probe['id']}", probe['path'], state]
      end
    end
    @identity_inputs = {'contract'=>'evidence-status/v1', 'repository_id'=>@data['repository_id'],
                        'source_sha256'=>Digest::SHA256.hexdigest(JSON.generate(source)),
                        'plan_sha256'=>file_digest(@plan), 'probes_sha256'=>Digest::SHA256.hexdigest(@declarations),
                        'runner_sha256'=>file_digest(__FILE__), 'ruby'=>RUBY_DESCRIPTION,
                        'environment_sha256'=>Digest::SHA256.hexdigest(JSON.generate(ENV.to_h.sort)),
                        'tool_inputs'=>inputs.sort}
    "sha256:#{Digest::SHA256.hexdigest(JSON.generate(@identity_inputs))}"
  end

  def run
    before = identity
    results = @probes.map { |probe| observe(probe, before) }
    raise ChangedInputs, 'source or tool inputs changed while probes ran; no evidence written' unless identity == before
    {'version'=>'evidence-status/v1', 'identity'=>before, 'inputs'=>@identity_inputs, 'probes'=>results,
     'evidence'=>evidence(before, results)}
  end

  private

  def validate_probe(probe)
    %w[id owner].each do |field|
      raise ArgumentError, "probe #{field} must be an identifier" unless probe[field].is_a?(String) && probe[field].match?(/\A[a-z][a-z0-9_]*\z/)
    end
    raise ArgumentError, 'unknown probe kind' unless KINDS.include?(probe['kind'])
    raise ArgumentError, 'acceptance must be boolean' unless [nil, true, false].include?(probe['acceptance'])
    if probe['acceptance'] && !%w[gate_execution gate_attestation].include?(probe['kind'])
      raise ArgumentError, 'only a declared verification gate can provide acceptance evidence'
    end
    if probe['kind'] == 'gate_execution'
      argv = probe['command']
      raise ArgumentError, 'gate command must be an explicit nonempty argv array' unless argv.is_a?(Array) && !argv.empty? && argv.all? { |s| s.is_a?(String) && !s.empty? && !s.include?("\0") }
      timeout = probe.fetch('timeout_seconds', 300)
      raise ArgumentError, 'timeout must be between 1 and 3600 seconds' unless timeout.is_a?(Integer) && (1..3600).cover?(timeout)
    else
      raise ArgumentError, 'probe path is required' unless probe['path'].is_a?(String) && !probe['path'].empty?
    end
    if probe['kind'] == 'file_present'
      source_path(probe['path'])
    elsif probe['kind'] == 'snapshot_query'
      raise ArgumentError, 'snapshot query needs codemap, a pinned stele executable' unless probe['codemap'].is_a?(String)
      match = probe['match']
      raise ArgumentError, 'snapshot query needs nonempty exact node fields' unless match.is_a?(Hash) && !match.empty? && (match.keys - %w[id symbol_key name kind]).empty? && match.values.all? { |v| v.is_a?(String) }
      raise ArgumentError, 'snapshot query count must be positive' unless probe.fetch('count', 1).is_a?(Integer) && probe.fetch('count', 1).positive?
    end
  end

  def resolve_input(path)
    File.expand_path(path, File.dirname(@config))
  end

  def source_path(path)
    raise ArgumentError, 'source path must be repository-relative' if Pathname.new(path).absolute? || path.split('/').any? { |p| ['', '.', '..'].include?(p) } || path.include?("\0") || path.include?('\\')
    full = File.join(@repo, path)
    # A symlinked parent must not make a probe inspect another repository.
    parent = File.dirname(full)
    parent = File.dirname(parent) until File.exist?(parent)
    real_parent = File.realpath(parent)
    raise ArgumentError, 'source path escapes repository' unless real_parent == @repo || real_parent.start_with?("#{@repo}/")
    raise ArgumentError, 'source probes do not follow symlinks' if File.symlink?(full)
    full
  end

  def file_digest(path)
    Digest::SHA256.file(path).hexdigest
  end

  def snapshot_tool(name)
    executable(name.include?('/') ? resolve_input(name) : name)
  end

  def executable(name)
    path = if name.include?('/')
             File.expand_path(name, @repo)
           else
             ENV.fetch('PATH', '').split(File::PATH_SEPARATOR).map { |p| File.join(p, name) }.find { |p| File.file?(p) && File.executable?(p) }
           end
    raise ArgumentError, "executable unavailable: #{name}" unless path && File.file?(path) && File.executable?(path)
    File.realpath(path)
  end

  def run_command(argv, timeout: 300)
    pid = Process.spawn([argv.first, argv.first], *argv.drop(1), chdir: @repo,
                        pgroup: true, out: File::NULL, err: File::NULL)
    waiter = Process.detach(pid)
    completed = waiter.join(timeout)
    completed&.value
  ensure
    if waiter && !completed
      begin
        Process.kill('KILL', -pid)
      rescue Errno::ESRCH
      end
      waiter.join
    end
  end

  def observe(probe, identity)
    result = {'id'=>probe['id'], 'owner'=>probe['owner'], 'kind'=>probe['kind'], 'passed'=>false}
    case probe['kind']
    when 'file_present'
      result['passed'] = File.file?(source_path(probe['path']))
      result['result'] = result['passed'] ? 'present' : 'absent'
    when 'snapshot_query'
      path = resolve_input(probe['path'])
      validation = run_command([snapshot_tool(probe['codemap']), 'code', 'validate', '--file', path])
      raise ArgumentError, 'snapshot validation failed or timed out' unless validation&.success?
      snapshot = JSON.parse(File.binread(path))
      raise ArgumentError, 'snapshot repository identity does not match declaration' unless snapshot.dig('identity', 'repository', 'id') == @data['repository_id']
      snapshot.fetch('files').each do |file|
        actual = source_path(file.fetch('rel_path'))
        raise ChangedInputs, 'snapshot source bytes do not match repository' unless File.file?(actual) && file_digest(actual) == file.fetch('sha256')
      end
      count = snapshot.fetch('nodes').count do |node|
        probe.fetch('match').all? do |key, value|
          (key == 'kind' ? node.dig('kind', 'kind') : node[key]) == value
        end
      end
      result.merge!('passed'=>count == probe.fetch('count', 1), 'result'=>'snapshot_query', 'count'=>count, 'snapshot_id'=>snapshot.fetch('identity').fetch('snapshot_id'))
    when 'gate_attestation'
      path = resolve_input(probe['path'])
      if File.file?(path)
        attestation = JSON.parse(File.binread(path))
        result['passed'] = attestation['identity'] == identity && attestation['probe'] == probe['id'] && attestation['exit_status'] == 0
        result['exit_status'] = attestation['exit_status']
        result['result'] = result['passed'] ? 'attested' : 'attestation_not_accepted'
      else
        result['result'] = 'attestation_missing'
      end
    when 'gate_execution'
      result['command'] = probe['command']
      if @execute
        argv = probe['command'].dup
        argv[0] = executable(argv[0])
        execution = run_command(argv, timeout: probe.fetch('timeout_seconds', 300))
        if execution
          result.merge!('passed'=>execution.success? == true, 'result'=>'executed', 'exit_status'=>execution.exitstatus)
        else
          result['result'] = 'timed_out'
        end
      else
        result['result'] = 'not_executed'
      end
    end
    result
  end

  def quote(value)
    # LemmaSpec accepts JSON's quote/backslash/newline/tab escapes, not \u.
    '"' + value.to_s.gsub('\\') { '\\\\' }.gsub('"') { '\\"' }.gsub("\n", '\\n').gsub("\r", '\\r').gsub("\t", '\\t') + '"'
  end

  def evidence(identity, results)
    lines = ["// Have all declared acceptance gates passed for this exact input identity?", 'spec observed_status {',
             "  fact target { relation: target_identity args: [#{quote(identity)}] basis: reviewer_declared source: \"status-runner:requested-target\" }"]
    @probes.zip(results).each do |probe, result|
      if probe['acceptance'] || result['passed']
        label = probe.fetch('label', "#{probe['kind'].tr('_', ' ')}: #{probe['id']}")
        lines << "  symbol #{probe['id']} { label: #{quote(label)} source: #{quote("#probe-#{probe['id']}")} }"
      end
      if probe['acceptance']
        lines << "  fact acceptance_#{probe['id']} { relation: acceptance_claim args: [#{probe['id']}, #{probe['owner']}] basis: reviewer_declared source: #{quote("probe:#{probe['id']}")} }"
      end
      next unless result['passed']
      lines << "  fact observed_#{probe['id']} { relation: observed args: [#{probe['id']}, #{quote(identity)}] basis: observed source: #{quote("probe:#{probe['id']}/#{probe['kind']}")} identity: #{quote(identity)} }"
    end
    lines << '  expect one_target { query: "target_identity(I)" count: 1 }'
    lines << '  expect acceptance_is_observed { query: "accepted(plan)" count: 1 }'
    lines << '}'
    lines.join("\n") + "\n"
  end
end
