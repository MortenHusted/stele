# frozen_string_literal: true
require 'minitest/autorun'
require 'tmpdir'
require_relative '../score'

class CppBenchmarkScoreTest < Minitest::Test
  def setup
    @dir = Dir.mktmpdir
    caller_span = {'start_byte' => 0, 'end_byte' => 1000}
    target_span = {'start_byte' => 0, 'end_byte' => 20}
    calls = (0...60).map { |i| {'span' => {'start_byte' => 10 + i * 4, 'end_byte' => 12 + i * 4}, 'name_span' => {'start_byte' => 10 + i * 4}} }
    anchors = {'files' => [
      {'path' => 'src/caller.cpp', 'sha256' => 'a' * 64, 'calls' => calls, 'bodies' => [{'span' => caller_span, 'normalized_name' => 'main()', 'name_span' => {'start_line' => 1}}]},
      {'path' => 'src/target.cpp', 'sha256' => 'b' * 64, 'calls' => [], 'bodies' => [{'span' => target_span, 'normalized_name' => 'target()', 'name_span' => {'start_line' => 1}}]}
    ]}
    @oracle = {'commit' => 'c' * 40, 'targets' => {'target' => {'path' => 'src/target.cpp', 'definition_line' => 1, 'symbol' => 'target()'}},
               'sites' => (0...60).map do |i|
                 {'id' => "site#{i}", 'path' => 'src/caller.cpp', 'caller' => 'main', 'byte_offset' => 10 + i * 4,
                  'classification' => i < 20 ? 'known_in_repository' : (i < 40 ? 'external' : 'unknown'),
                  'regime' => i < 10 ? 'supported' : 'unsupported_shape', 'target' => i < 20 ? 'target' : nil}
               end,
               'cross_module_queries' => (0...10).map { |i| {'caller_path' => 'src/caller.cpp', 'caller' => 'main', 'target' => 'target', 'site_id' => "site#{i}"} }}
    @snapshot = {'identity' => {'language' => 'cpp', 'cpp_semantic' => {}, 'dirty' => false, 'commit_sha' => 'c' * 40, 'snapshot_id' => 'synthetic'},
                 'files' => anchors['files'].map { |f| {'rel_path' => f['path'], 'sha256' => f['sha256']} },
                 'nodes' => [{'id' => 'caller', 'symbol_key' => 'cpp:src/caller.cpp#main()', 'provenance' => {'rel_path' => 'src/caller.cpp', 'span' => caller_span}},
                             {'id' => 'target', 'symbol_key' => 'cpp:src/target.cpp#target()', 'provenance' => {'rel_path' => 'src/target.cpp', 'span' => target_span}}],
                 'edges' => (0...10).map { |i| {'id' => "edge#{i}", 'kind' => 'Calls', 'from' => 'caller', 'to' => 'target', 'provenance' => {'rel_path' => 'src/caller.cpp', 'span' => calls[i]['span']}} },
                 'call_sites' => calls.map { |call| {'rel_path' => 'src/caller.cpp', 'span' => call['span'], 'category' => 'Unmapped'} }}
    @oracle_path = File.join(@dir, 'oracle.json')
    @anchors_path = File.join(@dir, 'anchors.json')
    File.write(@anchors_path, JSON.generate(anchors))
    @anchor_pin = Digest::SHA256.file(@anchors_path).hexdigest
  end

  def teardown
    FileUtils.remove_entry(@dir)
  end

  def scorer(pin: nil)
    File.write(@oracle_path, JSON.generate(@oracle))
    CppBenchmarkScore.new(oracle: @oracle_path, oracle_sha256: pin || Digest::SHA256.file(@oracle_path).hexdigest, anchors: @anchors_path, anchors_sha256: @anchor_pin)
  end

  def test_supported_and_all_known_denominators_are_independent
    report = scorer.score(@snapshot)
    assert report['passed']
    assert_equal 10, report['supported_denominator']
    assert_equal 20, report['known_denominator']
    assert_equal 0.5, report['overall_known_target_recall']
    assert_equal({'known_in_repository' => 20, 'external' => 20, 'unknown' => 20}, report['classification_counts'])
    @snapshot['edges'].shift(2)
    refute scorer.score(@snapshot)['passed']
  end

  def test_wrong_target_and_external_admission_fail_precision
    @snapshot['nodes'][1]['symbol_key'] = 'cpp:src/target.cpp#decoy()'
    assert_equal 10, scorer.score(@snapshot)['false_positive_sites']
    @snapshot['nodes'][1]['symbol_key'] = 'cpp:src/target.cpp#target()'
    extra = Marshal.load(Marshal.dump(@snapshot['edges'][0]))
    extra['id'] = 'extra'
    extra['provenance']['span'] = {'start_byte' => 90, 'end_byte' => 92}
    @snapshot['edges'] << extra
    refute scorer.score(@snapshot)['passed']
  end

  def test_duplicate_admission_is_not_recall_credit
    @snapshot['edges'] << @snapshot['edges'][0].merge('id' => 'duplicate')
    report = scorer.score(@snapshot)
    refute report['passed']
    assert_equal 1, report['false_positive_sites']
  end

  def test_changed_source_anchor_is_rejected_before_scoring
    anchors = JSON.parse(File.read(@anchors_path))
    anchors['files'][0]['calls'][1]['span'] = anchors['files'][0]['calls'][0]['span']
    File.write(@anchors_path, JSON.generate(anchors))
    assert_raises(ArgumentError) { scorer }
  end

  def test_pin_source_identity_and_sample_controls
    assert_raises(ArgumentError) { scorer(pin: '0' * 64) }
    @snapshot['files'][0]['sha256'] = 'd' * 64
    assert_raises(ArgumentError) { scorer.score(@snapshot) }
    @snapshot['files'][0]['sha256'] = 'a' * 64
    @snapshot['identity']['dirty'] = true
    assert_raises(ArgumentError) { scorer.score(@snapshot) }
    @oracle['cross_module_queries'].pop
    assert_raises(ArgumentError) { scorer }
  end

  def test_omitted_unsupported_anchor_cannot_hide_an_admitted_edge
    anchors = JSON.parse(File.read(@anchors_path))
    anchors['files'][0]['calls'].delete_at(20)
    File.write(@anchors_path, JSON.generate(anchors))
    @anchor_pin = Digest::SHA256.file(@anchors_path).hexdigest
    extra = Marshal.load(Marshal.dump(@snapshot['edges'][0]))
    extra['id'] = 'external'
    extra['provenance']['span'] = {'start_byte' => 90, 'end_byte' => 92}
    @snapshot['edges'] << extra
    report = scorer.score(@snapshot)
    refute report['passed']
    assert_equal 1, report['false_positive_sites']
  end

  def test_signature_free_target_label_cannot_select_same_line_overloads
    @oracle['targets']['target']['symbol'] = 'target'
    anchors = JSON.parse(File.read(@anchors_path))
    anchors['files'][1]['bodies'] << {'span' => {'start_byte' => 21, 'end_byte' => 40}, 'normalized_name' => 'target(int)', 'name_span' => {'start_line' => 1}}
    File.write(@anchors_path, JSON.generate(anchors))
    @anchor_pin = Digest::SHA256.file(@anchors_path).hexdigest
    assert_raises(ArgumentError) { scorer }
  end
end
