# frozen_string_literal: true
require 'minitest/autorun'
require 'tmpdir'
require 'open3'
require_relative '../replay'

class IntentReplayTest < Minitest::Test
  def bundle
    JSON.parse(File.read(File.expand_path('../../../publication/intent-mutations.json', __dir__)))
  end

  def test_owned_mutations_are_complete_unique_and_apply_without_old_metadata
    cases = IntentReplay.cases(JSON.generate(bundle))
    assert_equal 18, cases.length
    assert_equal 18, cases.map { |entry| [entry.fetch('example'), entry.fetch('channel')] }.uniq.length
    repo = File.expand_path('../../..', __dir__)
    cases.each do |entry|
      refute_match(/^index /, entry.fetch('patch'))
      Tempfile.create('stele-mutation') do |file|
        file.write(entry.fetch('patch'));file.flush
        _out, err, status = Open3.capture3(*ScrubGitContext.command, 'git', '-C', repo, 'apply', '--check', '--', file.path)
        assert status.success?, entry.fetch('example') + ': ' + err
      end
    end
  end

  def test_mutations_cannot_edit_tests_metadata_or_carry_old_record_fields
    original = bundle
    bad = Marshal.load(Marshal.dump(original));bad['cases'][0]['base'] = 'private-reference'
    assert_raises(ArgumentError) { IntentReplay.cases(JSON.generate(bad)) }
    bad = Marshal.load(Marshal.dump(original));bad['cases'] << bad['cases'][0]
    assert_raises(ArgumentError) { IntentReplay.cases(JSON.generate(bad)) }
    %w[crates/stele/tests/case.rs .git/config crates/stele/src/../tests/case.rs].each do |path|
      bad = Marshal.load(Marshal.dump(original))
      bad['cases'][0]['patch'] = "diff --git a/#{path} b/#{path}\n--- a/#{path}\n+++ b/#{path}\n@@ -1 +1 @@\n-a\n+b\n"
      assert_raises(ArgumentError) { IntentReplay.cases(JSON.generate(bad)) }
    end
    bad = Marshal.load(Marshal.dump(original));bad['cases'][0]['patch'].sub!("\n---", "\nindex 1234567..7654321 100644\n---")
    assert_raises(ArgumentError) { IntentReplay.cases(JSON.generate(bad)) }
  end

  def test_run_parser_preserves_observed_failures_and_rejects_unparsed_content
    text = "run {\n  example: synthetic\n  digest: \"sha256:digest\"\n  digest_version: 1\n  channel: cli\n  source: \"sha256:source\"\n  base: \"root\"\n  outcome: fail\n  diff: \"synthetic diff\\n\"\n}\n"
    runs = IntentReplay.runs(text)
    assert_equal 'fail', runs.first.fetch('outcome')
    assert_equal "synthetic diff\n", runs.first.fetch('diff')
    assert_raises(ArgumentError) { IntentReplay.runs(text + 'unparsed') }
    assert_raises(ArgumentError) { IntentReplay.runs(text.sub('  outcome: fail', '  unexpected: fail')) }
  end

  def fixture
    Dir.mktmpdir do |scratch|
      root = File.realpath(scratch);repo = File.join(root, 'repo');Dir.mkdir(repo)
      runner = TTY::Command.new(printer: :null)
      git = ->(*args) { runner.run(*ScrubGitContext.command, 'git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false', '-C', repo, *args) }
      git.call('init', '--quiet', '--template=', '--initial-branch=main')
      File.write(File.join(repo, 'README'), 'synthetic root');git.call('add', '.');git.call('commit', '--quiet', '-m', 'root')
      options = {repo: repo, out: File.join(root, 'report'), deny_list: File.join(root, 'deny'), scanner_archive: File.join(root, 'scanner')}
      yield root, repo, git, options
    end
  end

  def test_private_history_dirty_inputs_and_inherited_runs_refuse_before_build
    fixture do |_root, repo, git, options|
      File.write(File.join(repo, 'README'), 'dirty')
      assert_raises(ArgumentError) { IntentReplay.new.run(**options) }
      git.call('restore', 'README')
      Dir.mkdir(File.join(repo, 'intent'));File.write(File.join(repo, 'intent/runs.stele'), 'inherited evidence')
      git.call('add', '.');git.call('commit', '--quiet', '--amend', '--no-edit')
      assert_raises(ArgumentError) { IntentReplay.new.run(**options) }
      File.unlink(File.join(repo, 'intent/runs.stele'));git.call('add', '.');git.call('commit', '--quiet', '-m', 'second commit')
      assert_raises(ArgumentError) { IntentReplay.new.run(**options) }
      refute File.exist?(options[:out])
    end
  end

  def test_existing_and_in_git_report_destinations_are_preserved
    fixture do |_root, repo, _git, options|
      File.write(options[:out], 'preserve')
      assert_raises(ArgumentError) { IntentReplay.new.run(**options) }
      assert_equal 'preserve', File.read(options[:out])
      assert_raises(ArgumentError) { IntentReplay.new.run(**options.merge(out: File.join(repo, 'report'))) }
    end
  end

  def test_single_commit_output_keeps_source_and_ledger_but_removes_temporary_history
    fixture do |_root, repo, git, _options|
      root = git.call('rev-parse', 'HEAD').out.strip
      Dir.mkdir(File.join(repo, 'intent'))
      File.write(File.join(repo, 'intent/runs.stele'), "observed synthetic evidence\n")
      replay = IntentReplay.new
      replay.instance_variable_set(:@repo, repo)
      commit = replay.send(:commit_ledger, root, single_commit: true)
      refute_equal root, commit
      assert_equal commit, git.call('rev-list', '--all', '--parents').out.strip
      assert_equal 'refs/heads/main', git.call('for-each-ref', '--format=%(refname)').out.strip
      assert_equal "observed synthetic evidence\n", git.call('show', commit + ':intent/runs.stele').out
      assert_equal 'synthetic root', git.call('show', commit + ':README').out
      assert_equal '', git.call('status', '--porcelain', '--untracked-files=all').out
      _out, _err, status = Open3.capture3(*ScrubGitContext.command, 'git', '-C', repo, 'cat-file', '-e', root)
      refute status.success?, 'temporary recording commit must not remain in object storage'
      all = git.call('cat-file', '--batch-all-objects', '--batch-check=%(objectname)').out.lines.map(&:strip).sort
      reachable = git.call('rev-list', '--objects', '--all').out.lines.map { |line| line.split.first }.sort
      assert_equal reachable, all
    end
  end

  def test_default_output_retains_legacy_ledger_child
    fixture do |_root, repo, git, _options|
      root = git.call('rev-parse', 'HEAD').out.strip
      Dir.mkdir(File.join(repo, 'intent'))
      File.write(File.join(repo, 'intent/runs.stele'), "observed synthetic evidence\n")
      replay = IntentReplay.new
      replay.instance_variable_set(:@repo, repo)
      commit = replay.send(:commit_ledger, root)
      assert_equal [commit + ' ' + root, root], git.call('rev-list', '--all', '--parents').out.lines.map(&:strip)
    end
  end

  def test_wrong_observed_source_is_refused_and_the_applied_patch_is_restored
    fixture do |_root, repo, git, _options|
      root = git.call('rev-parse', 'HEAD').out.strip
      File.write(File.join(repo, 'README'), 'observed mutation')
      patch = git.call('diff', root).out
      git.call('restore', 'README')
      replay = IntentReplay.new
      replay.instance_variable_set(:@repo, repo)
      replay.define_singleton_method(:capture) do |*_args|
        Struct.new(:out).new('Source identity `sha256:actual-observed-source`')
      end
      entry = {'example' => 'synthetic', 'paths' => ['README']}
      error = assert_raises(ArgumentError) do
        replay.send(:verify_observed_source, root, entry, {'diff' => patch, 'source' => 'sha256:wrong-source'}, 'recorder')
      end
      assert_includes error.message, 'does not reproduce'
      assert_equal 'synthetic root', File.read(File.join(repo, 'README'))
      assert_equal '', git.call('status', '--porcelain').out
    end
  end

  def test_cli_help_and_noninteractive_failures
    command = File.expand_path('../../replay-intent-evidence', __dir__)
    out, _err, status = Open3.capture3(RbConfig.ruby, command, '--help')
    assert status.success?
    assert_includes out, '--repo'
    assert_includes out, '--single-commit'
    _out, _err, status = Open3.capture3(RbConfig.ruby, command, '--unknown', '--help')
    assert_equal 2, status.exitstatus
    _out, _err, status = Open3.capture3(RbConfig.ruby, command)
    assert_equal 2, status.exitstatus
  end
end
