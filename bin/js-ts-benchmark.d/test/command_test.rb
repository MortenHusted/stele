# frozen_string_literal: true
require 'minitest/autorun'
require 'tmpdir'
require 'open3'
require 'pty'
require 'rbconfig'

class JsTsBenchmarkCommandTest < Minitest::Test
  CLI = File.expand_path('../../js-ts-benchmark', __dir__)

  def test_repository_subdirectory_cannot_bypass_output_boundary
    Dir.mktmpdir do |repo|
      _output, status = Open3.capture2e('git', 'init', repo)
      assert status.success?
      subdirectory = File.join(repo, 'src')
      Dir.mkdir(subdirectory)
      output = File.join(repo, 'benchmark-out')
      _stdout, stderr, status = Open3.capture3(RbConfig.ruby, CLI,
        '--repo', subdirectory, '--project', 'tsconfig.json', '--environment', 'unused',
        '--stele', 'unused', '--oracle', 'unused', '--oracle-sha256', '0' * 64,
        '--anchors', 'unused', '--anchors-sha256', '0' * 64, '--out-dir', output)
      assert_equal 2, status.exitstatus
      assert_includes stderr, 'output must stay outside the analyzed repository'
      refute File.exist?(output)
    end
  end

  def test_argument_driven_invocation_never_prompts_to_install_gems
    output = +''
    PTY.spawn(RbConfig.ruby, '--disable-gems', CLI, '--help') do |reader, _writer, pid|
      begin
        loop { output << reader.readpartial(4096) }
      rescue EOFError, Errno::EIO
        # Preserve chunks before either platform's end-of-stream signal.
      end
      Process.wait(pid)
    end
    assert_includes output, 'prepare dependencies before scripted use'
    refute_includes output, 'Would you like to install'
  end
end
