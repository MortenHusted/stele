# frozen_string_literal: true
require 'minitest/autorun'
require 'tmpdir'
require 'json'
require 'rbconfig'
require 'open3'
require_relative '../status'

class EvidenceStatusTest < Minitest::Test
  def setup
    @dir = Dir.mktmpdir('status-synthetic')
    @repo = File.join(@dir, 'repo')
    Dir.mkdir(@repo)
    @runner = TTY::Command.new(printer: :null)
    @runner.run('git', 'init', '-q', @repo)
    @runner.run('git', '-C', @repo, 'config', 'user.name', 'Synthetic')
    @runner.run('git', '-C', @repo, 'config', 'user.email', 'synthetic@example.invalid')
    File.write(File.join(@repo, 'source.txt'), 'one')
    @runner.run('git', '-C', @repo, 'add', '.')
    @runner.run('git', '-C', @repo, 'commit', '-qm', 'synthetic fixture')
    @plan = File.join(@dir, 'plan.md')
    File.write(@plan, '# Synthetic plan')
    @config = File.join(@dir, 'probes.json')
  end

  def teardown
    FileUtils.remove_entry(@dir)
  end

  def status(probes, **options)
    File.write(@config, JSON.generate({'version'=>'evidence-status/v1', 'repository_id'=>'synthetic', 'probes'=>probes}))
    EvidenceStatus.new(repo: @repo, plan: @plan, config: @config, **options)
  end

  def gate(command = [RbConfig.ruby, '-e', 'exit 0'])
    {'id'=>'gate', 'owner'=>'u1', 'kind'=>'gate_execution', 'command'=>command, 'acceptance'=>true}
  end

  def test_identity_tracks_untracked_bytes_plan_and_probe_declarations
    s = status([gate])
    before = s.identity
    File.write(File.join(@repo, 'untracked'), 'one')
    one = s.identity
    File.write(File.join(@repo, 'untracked'), 'two')
    refute_equal before, one
    refute_equal one, s.identity
    current = s.identity
    File.write(@plan, '# changed')
    refute_equal current, s.identity
    refute_equal s.identity, status([gate([RbConfig.ruby, '-e', 'exit 1'])]).identity
  end

  def test_presence_is_progress_only_and_cannot_be_acceptance
    result = status([{'id'=>'exists','owner'=>'u1','kind'=>'file_present','path'=>'source.txt'}]).run
    assert result['probes'][0]['passed']
    refute_includes result['evidence'], 'relation: acceptance_claim'
    assert_raises(ArgumentError) { status([{'id'=>'exists','owner'=>'u1','kind'=>'file_present','path'=>'source.txt','acceptance'=>true}]) }
  end

  def test_ignored_presence_inputs_bind_missing_state_and_content
    File.write(File.join(@repo, '.gitignore'), "generated\n")
    path = File.join(@repo, 'generated')
    observer = status([{'id'=>'exists', 'owner'=>'u1', 'kind'=>'file_present', 'path'=>'generated'}])
    missing = observer.run
    refute missing['probes'][0]['passed']
    File.write(path, 'one')
    present = observer.run
    assert present['probes'][0]['passed']
    refute_equal missing['identity'], present['identity']
    File.write(path, 'two')
    refute_equal present['identity'], observer.identity
    File.unlink(path)
    assert_equal missing['identity'], observer.identity
    File.write(path, 'three')
    changing = gate([RbConfig.ruby, '-e', 'File.unlink("generated")'])
    probe = {'id'=>'exists', 'owner'=>'u1', 'kind'=>'file_present', 'path'=>'generated'}
    assert_raises(EvidenceStatus::ChangedInputs) { status([probe, changing], execute: true).run }
  end

  def test_gate_executable_path_and_arguments_are_literal_and_use_repo_directory
    tool = File.join(@dir, 'tool space; exit 9; #')
    File.write(tool, "#!#{RbConfig.ruby}\nexit(ARGV == ['literal; text'] && File.file?('source.txt') ? 0 : 8)\n")
    File.chmod(0o755, tool)
    result = status([gate([tool, 'literal; text'])], execute: true).run
    assert result['probes'][0]['passed']
    assert_equal 0, result['probes'][0]['exit_status']
  end

  def test_snapshot_validator_executable_path_is_literal_and_failure_is_rejected
    tool = File.join(@dir, 'validator space; exit 0; #')
    File.write(tool, "#!#{RbConfig.ruby}\nexit 7\n")
    File.chmod(0o755, tool)
    snapshot = File.join(@dir, 'snapshot.json')
    File.write(snapshot, JSON.generate({'identity'=>{'repository'=>{'id'=>'synthetic'}, 'snapshot_id'=>'fixture'}, 'files'=>[], 'nodes'=>[{'name'=>'main'}]}))
    probe = {'id'=>'main', 'owner'=>'u1', 'kind'=>'snapshot_query', 'path'=>snapshot, 'codemap'=>tool, 'match'=>{'name'=>'main'}}
    assert_raises(ArgumentError) { status([probe]).run }
    File.write(tool, "#!#{RbConfig.ruby}\nexit(ARGV == ['code', 'validate', '--file', #{snapshot.inspect}] ? 0 : 8)\n")
    assert status([probe]).run['probes'][0]['passed']
  end

  def test_timeout_stops_gate_and_descendant
    pids_file = File.join(@dir, 'pids')
    code = "child = fork { sleep 30 }; File.write(ARGV[0], [Process.pid, child].join(',')); sleep 30"
    declaration = gate([RbConfig.ruby, '-e', code, pids_file]).merge('timeout_seconds'=>1)
    result = status([declaration], execute: true).run
    assert_equal 'timed_out', result['probes'][0]['result']
    refute result['probes'][0]['passed']
    pids = File.read(pids_file).split(',').map(&:to_i)
    pids.each do |pid|
      state, = Open3.capture2('ps', '-o', 'stat=', '-p', pid.to_s)
      assert state.empty? || state.strip.start_with?('Z'), "process #{pid} survived timeout: #{state}"
    end
  ensure
    if pids_file && File.exist?(pids_file)
      File.read(pids_file).split(',').map(&:to_i).each do |pid|
        Process.kill('KILL', pid)
      rescue Errno::ESRCH
      end
    end
  end

  def test_gate_execution_is_explicit_and_failing_gate_stays_unobserved
    declaration = gate([RbConfig.ruby, '-e', 'exit 3'])
    skipped = status([declaration]).run
    assert_equal 'not_executed', skipped['probes'][0]['result']
    result = status([declaration], execute: true).run
    assert_equal 3, result['probes'][0]['exit_status']
    refute result['probes'][0]['passed']
    assert_includes result['evidence'], 'relation: acceptance_claim'
    refute_includes result['evidence'], 'relation: observed'
    signalled = gate([RbConfig.ruby, '-e', 'Process.kill("TERM", Process.pid)'])
    assert_equal false, status([signalled], execute: true).run['probes'][0]['passed']
  end

  def test_success_is_bound_to_identity_and_changes_during_gate_are_rejected
    result = status([gate], execute: true).run
    assert result['probes'][0]['passed']
    assert_includes result['evidence'], 'basis: observed'
    assert_includes result['evidence'], result['identity']
    changing = gate([RbConfig.ruby, '-e', 'File.write("source.txt", "changed")'])
    assert_raises(EvidenceStatus::ChangedInputs) { status([changing], execute: true).run }
  end

  def test_attestation_requires_matching_identity_and_names_its_origin
    path = File.join(@dir, 'attestation.json')
    declaration = {'id'=>'gate','owner'=>'u1','kind'=>'gate_attestation','path'=>path,'acceptance'=>true}
    s = status([declaration])
    File.write(path, JSON.generate({'identity'=>s.identity,'probe'=>'gate','exit_status'=>0}))
    result = s.run
    assert result['probes'][0]['passed']
    assert_equal 'gate_attestation', result['probes'][0]['kind']
    File.write(path, JSON.generate({'identity'=>'old','probe'=>'gate','exit_status'=>0}))
    refute s.run['probes'][0]['passed']
  end

  def test_snapshot_query_uses_canonical_validation_and_checks_source_bytes
    skip 'set STELE_BIN to run snapshot integration' unless ENV['STELE_BIN']
    File.write(File.join(@repo, 'main.rs'), "fn main() {}\n")
    @runner.run('git', '-C', @repo, 'add', '.')
    @runner.run('git', '-C', @repo, 'commit', '-qm', 'source')
    snapshot = File.join(@dir, 'snapshot.json')
    @runner.run(ENV['STELE_BIN'], 'code', 'extract', '--path', @repo, '--language', 'rust', '--out', snapshot)
    data = JSON.parse(File.read(snapshot))
    config = {'version'=>'evidence-status/v1', 'repository_id'=>data.dig('identity','repository','id'),
              'probes'=>[{'id'=>'main','owner'=>'u1','kind'=>'snapshot_query','path'=>snapshot,
                          'codemap'=>ENV['STELE_BIN'],'match'=>{'kind'=>'Function','name'=>'main'}}]}
    File.write(@config, JSON.generate(config))
    observer = EvidenceStatus.new(repo: @repo, plan: @plan, config: @config)
    result = observer.run
    assert result['probes'][0]['passed']
    assert_equal data.dig('identity','snapshot_id'), result['probes'][0]['snapshot_id']
    File.write(File.join(@repo, 'main.rs'), "fn changed() {}\n")
    assert_raises(EvidenceStatus::ChangedInputs) { observer.run }
  end

  def test_full_status_plan_pipeline_and_explicit_freshness
    skip 'set LEMMASPEC_BIN and LEMMASPEC_PLAN_BIN for integration' unless ENV['LEMMASPEC_BIN'] && ENV['LEMMASPEC_PLAN_BIN']
    File.write(@plan, "---\nartifact_contract: ce-unified-plan/v1\n---\n# Synthetic\n## Requirements\n- R1. Verify\n## Work\n### U1. Verify\n**Requirements:** R1. **Dependencies:** None.\n## Gates\n- None\n## Deferred\n- None\n## Semantic inputs\n- None\n")
    result = status([gate], execute: true).run
    receipt = File.join(@dir, 'status.lemmaspec')
    evidence = File.join(@dir, 'evidence.lemmaspec')
    checker = File.join(@dir, 'checker.lemmaspec')
    bound = File.join(@dir, 'bound.lemmaspec')
    File.write(receipt, result['evidence'])
    @runner.run(ENV['LEMMASPEC_PLAN_BIN'], 'project', @plan, '--status', receipt, '-o', evidence)
    @runner.run(ENV['LEMMASPEC_PLAN_BIN'], 'checker', '-o', checker)
    checked = @runner.run(ENV['LEMMASPEC_BIN'], 'check', checker, evidence, '--json')
    assert_equal 'clean', JSON.parse(checked.out)['status']
    @runner.run(ENV['LEMMASPEC_BIN'], 'bind', checker, evidence, '-o', bound)
    markdown = File.join(@dir, 'report.md')
    rendered = @runner.run!(ENV['LEMMASPEC_BIN'], 'render', bound, '--format', 'md', '--target-identity', 'changed-inputs', '-o', markdown)
    assert_equal 1, rendered.exit_status
    assert_includes File.read(markdown), 'Not current for target'
  end

  def test_cli_rejects_publishable_output_before_executing_any_gate
    status([gate([RbConfig.ruby, '-e', 'File.write("ran", "yes")'])])
    command = File.expand_path('../../evidence-status', __dir__)
    result = @runner.run!(RbConfig.ruby, command, '--repo', @repo, '--plan', @plan,
                          '--config', @config, '--execute', '--out', File.join(@repo, 'status.lemmaspec'))
    assert_equal 2, result.exit_status
    assert_includes result.err, 'git-ignored'
    refute File.exist?(File.join(@repo, 'ran'))
  end

  def test_cli_rejects_parent_aliases_before_running_or_writing
    marker = File.join(@dir, 'ran')
    status([gate([RbConfig.ruby, '-e', "File.write(#{marker.inspect}, 'yes')"])])
    alias_dir = File.join(@dir, 'alias')
    File.symlink(@dir, alias_dir)
    output = File.join(@dir, 'output')
    File.write(output, 'original')
    command = File.expand_path('../../evidence-status', __dir__)
    [[File.join(alias_dir, 'plan.md'), nil], [output, File.join(alias_dir, 'output')]].each do |out, report|
      before = [File.read(@plan), File.read(@config), File.read(output)]
      args = [RbConfig.ruby, command, '--repo', @repo, '--plan', @plan, '--config', @config, '--execute', '--out', out]
      args.concat(['--report', report]) if report
      _, _, result = Open3.capture3(*args)
      assert_equal 2, result.exitstatus
      assert_equal before, [File.read(@plan), File.read(@config), File.read(output)]
      refute File.exist?(marker)
    end
  end
end
