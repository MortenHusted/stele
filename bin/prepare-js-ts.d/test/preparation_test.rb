# frozen_string_literal: true
require 'minitest/autorun'
require 'tmpdir'
require 'fileutils'
require_relative '../preparation'
require_relative '../../js-ts-benchmark.d/score'
require 'open3'

class JsTsPreparationTest < Minitest::Test
  def setup
    @scratch = Dir.mktmpdir
    @roots = {'source' => File.realpath(@scratch) + '/source', 'typescript' => File.realpath(@scratch) + '/typescript'}
    @roots.each_value { |root| Dir.mkdir(root) }
    File.write(@roots['source'] + '/a.ts', 'export {}')
    @inputs = {'files' => {'{source}/a.ts' => Digest::SHA256.hexdigest('export {}')}, 'directories' => {'{source}' => {'a.ts' => {'kind' => 'file'}}}}
  end

  def teardown
    FileUtils.remove_entry(@scratch)
  end

  def test_frozen_files_and_directory_membership_must_both_match
    JsTsPreparation.verify_inputs(@inputs, @roots)
    File.write(@roots['source'] + '/a.ts', 'changed')
    assert_raises(ArgumentError) { JsTsPreparation.verify_inputs(@inputs, @roots) }
    File.write(@roots['source'] + '/a.ts', 'export {}')
    File.write(@roots['source'] + '/extra.ts', '')
    assert_raises(ArgumentError) { JsTsPreparation.verify_inputs(@inputs, @roots) }
  end

  def test_git_metadata_is_excluded_and_alias_targets_are_explicit
    Dir.mkdir(@roots['source'] + '/.git')
    File.symlink('a.ts', @roots['source'] + '/link.ts')
    @inputs['directories']['{source}']['link.ts'] = {'kind' => 'file', 'target' => '{source}/a.ts'}
    JsTsPreparation.verify_inputs(@inputs, @roots)
    @inputs['directories']['{source}']['link.ts'].delete('target')
    assert_raises(ArgumentError) { JsTsPreparation.verify_inputs(@inputs, @roots) }
  end

  def test_unknown_roots_traversal_and_git_reads_are_rejected
    ['{source}/../a.ts', '{source}/.git/config', '{unknown}/a.ts', '{source}/a\\b.ts'].each do |path|
      assert_raises(ArgumentError) { JsTsPreparation.physical(path, @roots) }
    end
  end

  def test_alias_cannot_escape_declared_roots
    File.write(File.join(@scratch, 'outside.ts'), '')
    File.symlink('../outside.ts', @roots['source'] + '/link.ts')
    @inputs['directories']['{source}']['link.ts'] = {'kind' => 'file', 'target' => '{source}/a.ts'}
    assert_raises(ArgumentError) { JsTsPreparation.verify_inputs(@inputs, @roots) }
  end

  def test_source_and_compiler_roots_cannot_overlap
    assert_raises(ArgumentError) { JsTsPreparation.verify_inputs(@inputs, @roots.merge('typescript' => @roots['source'])) }
  end

  def test_unknown_arguments_cannot_start_preparation
    command = File.expand_path('../../prepare-js-ts', __dir__)
    out, err, status = Open3.capture3(RbConfig.ruby, command, '--unknown', '--help')
    assert_equal 2, status.exitstatus
    assert_empty out
    assert_includes err, 'invalid arguments'
  end

  def test_shipped_oracle_preserves_the_existing_source_label_contract
    root = File.expand_path('../../..', __dir__)
    contract = JSON.parse(File.read(File.join(root, 'benchmarks/js_ts/excalidraw.json')))
    inputs = contract.fetch('inputs')
    scorer = JsTsBenchmarkScore.new(
      oracle: File.join(root, inputs.fetch('oracle').fetch('path')), oracle_sha256: inputs.fetch('oracle').fetch('sha256'),
      anchors: File.join(root, inputs.fetch('anchors').fetch('path')), anchors_sha256: inputs.fetch('anchors').fetch('sha256')
    )
    assert_equal contract.fetch('commit'), scorer.commit
    JsTsPreparation.new
  end
end
