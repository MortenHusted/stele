# frozen_string_literal: true
require 'minitest/autorun'
require 'tmpdir'
require 'fileutils'
require_relative '../score'

class PythonBenchmarkScoreTest < Minitest::Test
  def setup
    @dir = Dir.mktmpdir
    @owner = body('entry', 0, 1000)
    @target = body('target', 1001, 1020)
    @anchors = {'contract' => 'codemap-python-benchmark-anchors/v1', 'original_commit' => 'a' * 40,
                'files' => %w[a.py b.py c.py].map do |path|
                  {'path' => path, 'sha256' => 'a' * 64, 'definitions' => [@owner, @target],
                   'calls' => (0...20).map { |i| call(i) },
                   'non_call_decoys' => [{'spelling' => 'target', 'role' => 'reference', 'span' => span(1100, 1106)}]}
                end}
    @oracle = {'source_commit' => 'a' * 40, 'selected_areas' => %w[area1 area2 area3],
               'selected_complete_functions' => %w[a.py b.py c.py].to_h { |path| [path, ['entry']] },
               'thresholds' => {'false_positives' => 0, 'minimum_areas' => 3, 'minimum_census' => 50,
                                'minimum_supported_recall' => 0.9, 'minimum_supported_share' => 0.2},
               'calls' => @anchors['files'].flat_map { |file| file['calls'].each_with_index.map { |value, i| value.merge('path' => file['path'], 'expected_supported' => i < 4, 'expected_target' => {'path' => file['path'], 'qualified_name' => 'target', 'span' => @target['span']}) } },
               'non_call_decoys' => [{'path' => 'a.py', 'spelling' => 'target', 'role' => 'reference'}]}
    @bridge = {'prepared_commit' => 'b' * 40, 'prepared_tree' => 'c' * 40, 'repository' => {'id' => 'synthetic'}}
    @snapshot = {'identity' => {'language' => 'python', 'python' => {'native' => {}}, 'dirty' => false,
                              'commit_sha' => @bridge['prepared_commit'], 'tree_sha' => @bridge['prepared_tree'], 'repository' => @bridge['repository'], 'snapshot_id' => 'synthetic'},
                 'files' => @anchors['files'].map { |file| {'rel_path' => file['path'], 'sha256' => file['sha256']} },
                 'nodes' => @anchors['files'].flat_map { |file| file['definitions'].map { |value| {'id' => "#{file['path']}:#{value['qualified_name']}", 'kind' => {'kind' => 'Function'}, 'symbol_key' => "python:#{file['path']}##{value['qualified_name']}", 'provenance' => {'rel_path' => file['path'], 'span' => value['span']}} } },
                 'edges' => @oracle['calls'].select { |value| value['expected_supported'] }.each_with_index.map { |value, i| {'id' => "edge#{i}", 'kind' => 'Calls', 'from' => "#{value['path']}:entry", 'to' => "#{value['path']}:target", 'provenance' => {'rel_path' => value['path'], 'span' => value['span']}} },
                 'call_sites' => @oracle['calls'].map { |value| {'rel_path' => value['path'], 'span' => value['span'], 'raw_name' => value['callee'], 'category' => value['expected_supported'] ? 'Resolved' : 'Unsupported'} }}
  end

  def teardown
    FileUtils.remove_entry(@dir)
  end

  def span(first, last)
    {'start_byte' => first, 'end_byte' => last, 'start_line' => 1, 'end_line' => 1}
  end

  def body(name, first, last)
    {'qualified_name' => name, 'kind' => 'FunctionDef', 'span' => span(first, last), 'declaration_only' => false, 'decorators' => []}
  end

  def call(index)
    {'span' => span(10 + index * 10, 19 + index * 10), 'callee_span' => span(10 + index * 10, 16 + index * 10),
     'callee' => 'target', 'callee_kind' => 'Name', 'caller' => 'entry', 'evaluation_context' => 'body'}
  end

  def scorer
    args = {}
    {oracle: @oracle, anchors: @anchors}.each do |key, value|
      path = File.join(@dir, "#{key}.json")
      File.binwrite(path, JSON.generate(value))
      args[key], args["#{key}_sha256".to_sym] = path, Digest::SHA256.file(path).hexdigest
    end
    PythonBenchmarkScore.new(**args)
  end

  def score
    scorer.score(@snapshot, bridge: @bridge)
  end

  def test_frozen_census_supported_share_recall_and_complete_categories
    report = score
    assert report['passed']
    assert_equal 60, report['census_denominator']
    assert_equal 12, report['supported_denominator']
    assert_equal 0.2, report['supported_share']
    assert_equal({'Resolved' => 12, 'Unsupported' => 48}, report['complete_call_categories'])
    @snapshot['edges'].shift(2)
    refute score['passed']
    assert_equal 12, score['supported_denominator']
  end

  def test_wrong_caller_target_signature_and_same_name_never_establish_exact_body
    @snapshot['nodes'][0]['provenance']['span'] = span(1, 999)
    refute score['passed']
    @snapshot['nodes'][0]['provenance']['span'] = @owner['span']
    @snapshot['nodes'][1]['provenance']['span'] = span(1001, 1010)
    refute score['passed']
    @snapshot['nodes'][1]['provenance']['span'] = @target['span']
    @snapshot['nodes'][1]['symbol_key'] = 'python:a.py#decoy'
    refute score['passed']
  end

  def test_negative_duplicate_and_shifted_edges_are_not_recall
    edge = Marshal.load(Marshal.dump(@snapshot['edges'][0]))
    @snapshot['edges'] << edge.merge('id' => 'duplicate')
    assert_equal 1, score['false_positive_sites']
    @snapshot['edges'].pop
    edge['provenance']['span'] = call(5)['span']
    @snapshot['edges'] << edge
    assert_equal 1, score['false_positive_sites']
    edge['provenance']['span'] = span(61, 69)
    assert_equal ['edge0'], score['unanchored_edges']
    refute score['passed']
  end

  def test_non_call_reference_and_missing_observation_fail
    edge = Marshal.load(Marshal.dump(@snapshot['edges'][0]))
    edge['provenance']['span'] = span(1100, 1106)
    @snapshot['edges'] << edge
    assert_equal ['edge0'], score['decoy_edges']
    @snapshot['edges'].pop
    @snapshot['call_sites'].pop
    assert_raises(ArgumentError) { score }
  end

  def test_identity_source_and_profile_must_match_prepared_tree
    %w[commit_sha tree_sha repository].each do |field|
      original = @snapshot['identity'][field]
      @snapshot['identity'][field] = 'different'
      assert_raises(ArgumentError) { score }
      @snapshot['identity'][field] = original
    end
    @snapshot['identity']['python']['native'] = nil
    assert_raises(ArgumentError) { score }
    @snapshot['identity']['python']['native'] = {}
    @snapshot['files'][0]['sha256'] = 'changed'
    assert_raises(ArgumentError) { score }
  end

  def test_oracle_thresholds_denominators_source_spans_and_decoys_are_immutable
    @oracle['thresholds']['minimum_supported_recall'] = 0.8
    assert_raises(ArgumentError) { scorer }
    @oracle['thresholds']['minimum_supported_recall'] = 0.9
    @oracle['calls'][0]['expected_supported'] = false
    assert_raises(ArgumentError) { scorer }
    @oracle['calls'][0]['expected_supported'] = true
    @oracle['calls'][0]['callee_span'] = span(11, 16)
    assert_raises(ArgumentError) { scorer }
  end

  def test_unlabelled_call_in_declared_complete_body_cannot_escape_scoring
    @anchors['files'][0]['calls'] << call(30)
    assert_raises(ArgumentError) { scorer }
    @anchors['files'][0]['calls'].pop
    @oracle['calls'].delete_at(19)
    assert_raises(ArgumentError) { scorer }
  end

  def test_neutral_kind_object_is_required_and_methods_need_source_class_ownership
    @snapshot['nodes'][0]['kind'] = 'Function'
    refute score['passed']
    @snapshot['nodes'][0]['kind'] = {'kind' => 'Method'}
    refute score['passed']
    @snapshot['nodes'][0]['kind'] = {'kind' => 'Function', 'extra' => 'unsupported'}
    refute score['passed']
  end
end
