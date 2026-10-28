# frozen_string_literal: true
require 'minitest/autorun'
require 'open3'
require 'tmpdir'
require 'fileutils'
require_relative '../runner'

class PythonBenchmarkCommandTest < Minitest::Test
  COMMAND = File.expand_path('../../python-benchmark', __dir__)

  def test_help_and_scripted_missing_options_never_prompt_or_install
    out, _err, status = Open3.capture3(COMMAND, '--help')
    assert status.success?
    assert_includes out, '--bridge-sha256'
    assert_includes out, '--clone'
    _out, err, status = Open3.capture3(COMMAND)
    assert_equal 2, status.exitstatus
    assert_includes err, 'Usage:'
    _out, err, status = Open3.capture3(COMMAND, '--original', '/absent')
    assert_equal 2, status.exitstatus
    refute_includes err, 'Would you like'
  end

  def test_output_inside_input_or_parent_symlink_cannot_write_sources
    Dir.mktmpdir do |dir|
      repo = File.join(dir, 'repo')
      Dir.mkdir(repo)
      sentinel = File.join(repo, 'main.py')
      File.write(sentinel, 'pass')
      linked = File.join(dir, 'linked')
      File.symlink(repo, linked)
      [File.join(repo, 'out'), File.join(linked, 'out')].each do |output|
        args = %w[original repo clone].flat_map { |key| ["--#{key}", repo] }
        args += %w[config stele oracle oracle-sha256 anchors anchors-sha256 bridge bridge-sha256 query query-sha256].flat_map { |key| ["--#{key}", '/absent'] }
        _out, _err, status = Open3.capture3(COMMAND, *args, '--out-dir', output)
        assert_equal 2, status.exitstatus
        refute File.exist?(File.join(repo, 'out'))
        assert_equal 'pass', File.read(sentinel)
      end
    end
  end

  def test_subprocess_failure_retains_stdout_stderr_in_private_operator_log
    Dir.mktmpdir do |dir|
      runner = PythonBenchmarkRunner.new(dir)
      assert_output(nil, /Benchmark diagnostics:/) do
        assert_raises(TTY::Command::ExitError) { runner.run('/bin/sh', '-c', 'echo stdout-proof; echo stderr-proof >&2; exit 7') }
      end
      paths = Dir.glob(File.join(dir, 'python-benchmark-failure-*.log'))
      assert_equal 1, paths.length
      bytes = File.read(paths.first)
      assert_includes bytes, 'stdout-proof'
      assert_includes bytes, 'stderr-proof'
      assert_includes bytes, 'exit status: 7'
      assert_equal 0o600, File.stat(paths.first).mode & 0o777
    end
  end
end
