# frozen_string_literal: true
require 'minitest/autorun'
require 'tmpdir'
require 'fileutils'
require 'open3'
require_relative '../bridge'

class PythonBenchmarkBridgeTest < Minitest::Test
  class Runner
    Result = Struct.new(:out)
    def run(*args)
      out, err, status = Open3.capture3(*args)
      raise "fixture command failed: #{err}" unless status.success?
      Result.new(out)
    end
  end

  def setup
    @dir = Dir.mktmpdir
    @runner = Runner.new
    @original, @prepared, @clone = %w[original prepared clone].map { |name| File.join(@dir, name) }
    Dir.mkdir(@original)
    git(@original, 'init', '-b', 'main')
    git(@original, 'remote', 'add', 'origin', 'https://example.invalid/application.git')
    File.write(File.join(@original, 'main.py'), "def entry():\n    return 1\n")
    FileUtils.mkdir_p(File.join(@original, 'tests/data'))
    File.write(File.join(@original, 'tests/data/invalid.py'), "def !!!\n")
    commit(@original, 'source')
    @original_pin = git(@original, 'rev-parse', 'HEAD').strip
    @runner.run('git', 'clone', @original, @prepared)
    git(@prepared, 'remote', 'set-url', 'origin', 'https://example.invalid/application.git')
    File.delete(File.join(@prepared, 'tests/data/invalid.py'))
    commit(@prepared, 'prepare')
    @runner.run('git', 'clone', @prepared, @clone)
    git(@clone, 'remote', 'set-url', 'origin', 'https://example.invalid/application.git')
    @query = {'package_profile' => {}, 'files' => [{'path' => 'main.py', 'sha256' => Digest::SHA256.file(File.join(@original, 'main.py')).hexdigest}]}
    @scorer = Struct.new(:original_commit, :files).new(@original_pin, {'main.py' => {'sha256' => @query['files'][0]['sha256']}})
    @manifest = {'contract' => 'codemap-python-prepared-tree/v1', 'original_commit' => @original_pin,
                 'removed' => ['tests/data/invalid.py'], 'package_profile' => {}}
    update_pins
  end

  def teardown
    FileUtils.remove_entry(@dir)
  end

  def git(repo, *args)
    @runner.run('git', '-C', repo, *args).out
  end

  def commit(repo, message, amend: false)
    git(repo, 'add', '-A')
    args = ['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', message]
    args << '--amend' if amend
    git(repo, *args)
  end

  def update_pins
    @manifest['prepared_commit'] = git(@prepared, 'rev-parse', 'HEAD').strip
    @manifest['prepared_tree'] = git(@prepared, 'rev-parse', 'HEAD^{tree}').strip
  end

  def synchronize_changed_clone
    git(@clone, 'fetch', @prepared, 'main')
    git(@clone, 'reset', '--hard', 'FETCH_HEAD')
    update_pins
  end

  def bridge
    path = File.join(@dir, 'bridge.json')
    File.write(path, JSON.generate(@manifest))
    PythonBenchmarkBridge.new(path: path, sha256: Digest::SHA256.file(path).hexdigest, runner: @runner)
  end

  def verify
    bridge.verify(original: @original, prepared: @prepared, clone: @clone, scorer: @scorer, query: @query)
  end

  def test_explicit_prepared_pin_preserves_original_labels_and_native_bytes
    assert_equal @manifest, verify
    assert_equal @original_pin, @scorer.original_commit
    assert File.exist?(File.join(@original, 'tests/data/invalid.py'))
  end

  def test_changed_surviving_application_blob_refuses_even_with_new_prepared_pin
    File.write(File.join(@prepared, 'main.py'), "def entry():\n    return 2\n")
    commit(@prepared, 'changed', amend: true)
    synchronize_changed_clone
    error = assert_raises(ArgumentError) { verify }
    assert_match(/only delete/, error.message)
  end

  def test_added_source_and_undeclared_removal_cannot_change_acceptance_inventory
    File.write(File.join(@prepared, 'new.py'), 'pass')
    commit(@prepared, 'added', amend: true)
    synchronize_changed_clone
    assert_raises(ArgumentError) { verify }
    @manifest['removed'] = ['main.py']
    assert_raises(ArgumentError) { verify }
  end

  def test_original_pin_label_pin_clone_identity_and_dirty_sources_are_checked
    @scorer.original_commit = 'f' * 40
    assert_raises(ArgumentError) { verify }
    @scorer.original_commit = @original_pin
    git(@clone, 'remote', 'set-url', 'origin', 'https://example.invalid/other.git')
    assert_raises(ArgumentError) { verify }
    git(@clone, 'remote', 'set-url', 'origin', 'https://example.invalid/application.git')
    File.write(File.join(@clone, 'untracked.py'), 'pass')
    assert_raises(ArgumentError) { verify }
  end

  def test_changed_native_input_and_source_anchor_pins_refuse
    @query['files'][0]['sha256'] = '0' * 64
    assert_raises(ArgumentError) { verify }
    @query['files'][0]['sha256'] = @scorer.files['main.py']['sha256']
    @scorer.files['main.py']['sha256'] = '1' * 64
    assert_raises(ArgumentError) { verify }
  end

  def test_equivalent_checkout_is_required_and_manifest_digest_is_enforced
    assert_raises(ArgumentError) { bridge.verify(original: @original, prepared: @prepared, clone: @prepared, scorer: @scorer, query: @query) }
    path = File.join(@dir, 'bad.json')
    File.write(path, '{}')
    assert_raises(ArgumentError) { PythonBenchmarkBridge.new(path: path, sha256: '0' * 64, runner: @runner) }
  end

  def test_configured_monitor_and_filter_cannot_execute_during_bridge_reads
    sentinel = File.join(@dir, 'executed')
    hook = File.join(@dir, 'monitor')
    File.write(hook, "#!/bin/sh\ntouch '#{sentinel}'\nexit 0\n")
    File.chmod(0o755, hook)
    [@original, @prepared, @clone].each do |repo|
      git(repo, 'config', 'core.fsmonitor', hook)
      git(repo, 'config', 'filter.danger.clean', hook)
      git(repo, 'config', 'filter.danger.required', 'true')
    end
    assert_equal @manifest, verify
    refute File.exist?(sentinel)
  end

  def test_missing_promisor_blob_refuses_without_running_fetch_transport
    sentinel = File.join(@dir, 'network-executed')
    helper = File.join(@dir, 'fetch-helper')
    File.write(helper, "#!/bin/sh\ntouch '#{sentinel}'\nexit 1\n")
    File.chmod(0o755, helper)
    [@original, @prepared, @clone].each do |repo|
      git(repo, 'config', 'remote.origin.url', "ext::#{helper}")
      git(repo, 'config', 'protocol.ext.allow', 'always')
    end
    git(@prepared, 'config', 'remote.origin.promisor', 'true')
    object = git(@prepared, 'rev-parse', 'HEAD:main.py').strip
    File.delete(File.join(@prepared, '.git', 'objects', object[0, 2], object[2..]))
    assert_raises(RuntimeError) { verify }
    refute File.exist?(sentinel)
  end
end
