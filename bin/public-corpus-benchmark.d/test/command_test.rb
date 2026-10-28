# frozen_string_literal: true

require 'minitest/autorun'
require 'open3'
require 'tmpdir'
require 'fileutils'

class PublicCorpusCommandTest < Minitest::Test
  COMMAND = File.expand_path('../../public-corpus-benchmark', __dir__)

  def test_help_missing_and_unknown_options_never_run_a_gate
    out, _, status = Open3.capture3(COMMAND, '--help')
    assert status.success?
    assert_includes out, '--bundle'
    assert_equal 2, Open3.capture3(COMMAND).last.exitstatus
    out, err, status = Open3.capture3(COMMAND, '--unknown')
    assert_equal 2, status.exitstatus
    assert_empty out
    refute_includes err, 'Would you like'
  end

  def test_output_inside_git_or_its_symlink_is_rejected_before_any_gate
    root = File.expand_path('../../..', __dir__)
    Dir.mktmpdir do |dir|
      alias_root = File.join(dir, 'alias')
      File.symlink(root, alias_root)
      [root, alias_root].each do |path|
        output = File.join(path, 'must-not-create-public-results')
        _, err, status = Open3.capture3(COMMAND, '--language', 'go', '--repo', root, '--out', output)
        assert_equal 2, status.exitstatus
        assert_includes err, 'outside Git'
        refute File.exist?(output)
      end
    end
  end
end
