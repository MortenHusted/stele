# frozen_string_literal: true
require 'minitest/autorun'
require_relative '../preparation'
require_relative '../../python-benchmark.d/score'
require 'open3'

class BlackPreparationTest < Minitest::Test
  def test_rejections_must_equal_the_frozen_list
    BlackPreparation.verify_rejections(%w[tests/data/a.py tests/data/b.py], %w[tests/data/b.py tests/data/a.py])
    [[], %w[tests/data/a.py], %w[tests/data/a.py tests/data/b.py src/c.py]].each do |observed|
      assert_raises(ArgumentError) { BlackPreparation.verify_rejections(%w[tests/data/a.py tests/data/b.py], observed) }
    end
  end

  def test_frozen_removals_cannot_overlap_labelled_or_native_inputs
    BlackPreparation.verify_removals(['tests/data/a.py'], ['src/b.py'])
    [%w[tests/data/a.py tests/data/a.py], ['src/a.py'], ['tests/data/../a.py'], ['tests/data/a.py']].each_with_index do |removed, index|
      protected = index == 3 ? ['tests/data/a.py'] : []
      assert_raises(ArgumentError) { BlackPreparation.verify_removals(removed, protected) }
    end
  end

  def test_query_materialization_requires_the_frozen_source_hash
    require 'tmpdir'
    Dir.mktmpdir do |root|
      File.write(File.join(root, 'a.py'), 'pass')
      files = [{'path' => 'a.py', 'sha256' => Digest::SHA256.hexdigest('pass')}]
      result = BlackPreparation.materialize(files, root)
      assert_equal 'pass', Base64.strict_decode64(result.first.fetch('source_base64'))
      File.write(File.join(root, 'a.py'), 'changed')
      assert_raises(ArgumentError) { BlackPreparation.materialize(files, root) }
    end
  end

  def test_unknown_arguments_cannot_start_preparation
    command = File.expand_path('../../prepare-black', __dir__)
    out, err, status = Open3.capture3(RbConfig.ruby, command, '--unknown', '--help')
    assert_equal 2, status.exitstatus
    assert_empty out
    assert_includes err, 'invalid arguments'
  end

  def test_shipped_labels_keep_the_existing_complete_body_and_threshold_contract
    root = File.expand_path('../../..', __dir__)
    contract = JSON.parse(File.read(File.join(root, 'benchmarks/python/black.json')))
    inputs = contract.fetch('inputs')
    scorer = PythonBenchmarkScore.new(
      oracle: File.join(root, inputs.fetch('oracle').fetch('path')), oracle_sha256: inputs.fetch('oracle').fetch('sha256'),
      anchors: File.join(root, inputs.fetch('anchors').fetch('path')), anchors_sha256: inputs.fetch('anchors').fetch('sha256')
    )
    assert_equal contract.fetch('original_commit'), scorer.original_commit
    BlackPreparation.new
  end
end
