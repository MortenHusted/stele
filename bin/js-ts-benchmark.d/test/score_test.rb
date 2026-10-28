# frozen_string_literal: true
require 'minitest/autorun'
require 'tmpdir'
require_relative '../score'

class JsTsBenchmarkScoreTest < Minitest::Test
  def setup
    @dir = Dir.mktmpdir
    owner = {'span' => span(0, 1000), 'body_span' => span(1, 999), 'callable_span' => span(0, 1000),
             'name_span' => {'start_line' => 1}, 'normalized_name' => 'entry', 'scope' => [], 'kind' => 'Function'}
    target = {'span' => span(0, 20), 'body_span' => span(10, 19), 'callable_span' => span(0, 20),
              'name_span' => {'start_line' => 1}, 'normalized_name' => 'target', 'scope' => [], 'kind' => 'Function'}
    calls = (0...60).map { |i| {'span' => span(10 + i * 4, 12 + i * 4), 'name_span' => span(10 + i * 4, 11 + i * 4), 'callee_span' => span(10 + i * 4, 11 + i * 4), 'caller_body' => owner['body_span']} }
    @anchors = {'contract' => 'codemap-js-ts-source-anchors/v1', 'commit' => 'c' * 40, 'files' => [
      {'path' => 'src/entry.tsx', 'sha256' => 'a' * 64, 'bodies' => [owner], 'calls' => calls, 'jsx_names' => [{'name' => 'View', 'span' => span(300, 304)}]},
      {'path' => 'src/target.js', 'sha256' => 'b' * 64, 'bodies' => [target], 'calls' => [], 'jsx_names' => []}]}
    @oracle = {'commit' => 'c' * 40, 'files' => {'src/entry.tsx' => {'sha256' => 'a' * 64}, 'src/target.js' => {'sha256' => 'b' * 64}},
               'targets' => {'target' => {'path' => 'src/target.js', 'definition_line' => 1, 'symbol' => 'target'}},
               'sites' => (0...60).map { |i| {'id' => "site#{i}", 'path' => 'src/entry.tsx', 'caller' => 'entry', 'spelling' => 'target', 'byte_offset' => 10 + i * 4,
                 'classification' => i < 20 ? 'known_in_repository' : (i < 40 ? 'external' : 'unknown'), 'regime' => i < 10 ? 'supported' : 'unsupported_shape', 'target' => i < 20 ? 'target' : nil} },
               'cross_module_queries' => (0...10).map { |i| {'site_id' => "site#{i}", 'caller_path' => 'src/entry.tsx', 'caller' => 'entry', 'target' => 'target'} },
               'non_call_decoys' => [{'path' => 'src/entry.tsx', 'line' => 1, 'spelling' => '<View>', 'kind' => 'jsx'}]}
    @snapshot = {'identity' => {'language' => 'js_ts', 'js_ts_semantic' => {}, 'dirty' => false, 'commit_sha' => 'c' * 40, 'snapshot_id' => 'synthetic'},
                 'files' => @anchors['files'].map { |f| {'rel_path' => f['path'], 'sha256' => f['sha256']} },
                 'nodes' => [{'id' => 'entry', 'kind' => 'Function', 'symbol_key' => 'js_ts:src/entry.tsx#entry', 'provenance' => {'rel_path' => 'src/entry.tsx', 'span' => owner['span']}},
                             {'id' => 'target', 'kind' => 'Function', 'symbol_key' => 'js_ts:src/target.js#target', 'provenance' => {'rel_path' => 'src/target.js', 'span' => target['span']}}],
                 'edges' => (0...10).map { |i| {'id' => "edge#{i}", 'kind' => 'Calls', 'from' => 'entry', 'to' => 'target', 'provenance' => {'rel_path' => 'src/entry.tsx', 'span' => calls[i]['span']}} },
                 'call_sites' => calls.map { |c| {'rel_path' => 'src/entry.tsx', 'span' => c['span'], 'category' => 'Unmapped'} }}
    @anchors['files'][1]['bodies'] = (0...10).map do |i|
      body = Marshal.load(Marshal.dump(target))
      body['span'], body['body_span'], body['callable_span'] = span(i * 30, i * 30 + 20), span(i * 30 + 10, i * 30 + 19), span(i * 30, i * 30 + 20)
      body['name_span'], body['normalized_name'] = {'start_line' => i + 1}, "target#{i}"
      body
    end
    @oracle['targets'] = (0...10).to_h { |i| ["target#{i}", {'path' => 'src/target.js', 'definition_line' => i + 1, 'symbol' => "target#{i}"}] }
    @oracle['sites'].each_with_index { |site, i| site['target'] = "target#{i % 10}" if i < 20 }
    @oracle['cross_module_queries'].each_with_index { |query, i| query['target'] = "target#{i}" }
    @snapshot['nodes'] = [@snapshot['nodes'][0]] + @anchors['files'][1]['bodies'].map { |body| {'id' => body['normalized_name'], 'kind' => 'Function', 'symbol_key' => "js_ts:src/target.js##{body['normalized_name']}", 'provenance' => {'rel_path' => 'src/target.js', 'span' => body['span']}} }
    @snapshot['edges'].each_with_index { |edge, i| edge['to'] = "target#{i}" }
    @oracle_path, @anchor_path = File.join(@dir, 'oracle.json'), File.join(@dir, 'anchors.json')
  end

  def teardown
    FileUtils.remove_entry(@dir)
  end

  def span(first, last)
    {'start_byte' => first, 'end_byte' => last, 'start_line' => 1}
  end

  def scorer
    File.write(@oracle_path, JSON.generate(@oracle))
    File.write(@anchor_path, JSON.generate(@anchors))
    JsTsBenchmarkScore.new(oracle: @oracle_path, oracle_sha256: Digest::SHA256.file(@oracle_path).hexdigest,
                          anchors: @anchor_path, anchors_sha256: Digest::SHA256.file(@anchor_path).hexdigest)
  end

  def test_frozen_denominators_and_same_line_real_calls
    report = scorer.score(@snapshot)
    assert report['passed']
    assert_equal 10, report['supported_denominator']
    assert_equal 20, report['known_denominator']
    assert_equal 0.5, report['overall_known_target_recall']
    assert_equal({'known_in_repository' => 20, 'external' => 20, 'unknown' => 20}, report['classification_counts'])
    assert_empty report['decoy_edges']
    @snapshot['edges'].shift(2)
    refute scorer.score(@snapshot)['passed']
  end

  def test_wrong_caller_span_cannot_hide_behind_the_same_name
    @snapshot['nodes'][0]['provenance']['span'] = span(500, 999)
    assert_equal 10, scorer.score(@snapshot)['false_positive_sites']
  end

  def test_signature_span_wrong_target_name_and_kind_are_not_body_proof
    @snapshot['nodes'][1]['provenance']['span'] = span(0, 10)
    assert_equal 1, scorer.score(@snapshot)['false_positive_sites']
    @snapshot['nodes'][1]['provenance']['span'] = span(0, 20)
    @snapshot['nodes'][1]['symbol_key'] = 'js_ts:src/target.js#decoy'
    assert_equal 1, scorer.score(@snapshot)['false_positive_sites']
    @snapshot['nodes'][1]['symbol_key'] = 'js_ts:src/target.js#target0'
    @snapshot['nodes'][1]['kind'] = 'Method'
    assert_equal 1, scorer.score(@snapshot)['false_positive_sites']
  end

  def test_repeated_sites_do_not_substitute_for_distinct_queries
    @oracle['sites'].first(10).each { |site| site['target'] = 'target0' }
    @oracle['cross_module_queries'].each { |query| query['target'] = 'target0' }
    assert_raises(ArgumentError) { scorer }
  end

  def test_alias_labels_for_one_body_do_not_substitute_for_distinct_queries
    @oracle['targets'].transform_values! { @oracle['targets']['target0'].dup }
    assert_raises(ArgumentError) { scorer }
  end

  def test_duplicate_and_external_edges_do_not_raise_recall
    @snapshot['edges'] << @snapshot['edges'][0].merge('id' => 'duplicate')
    report = scorer.score(@snapshot)
    refute report['passed']
    assert_equal 1, report['false_positive_sites']
    @snapshot['edges'].pop
    extra = Marshal.load(Marshal.dump(@snapshot['edges'][0]))
    extra['id'], extra['provenance']['span'] = 'external', span(90, 92)
    @snapshot['edges'] << extra
    assert_equal 1, scorer.score(@snapshot)['false_positive_sites']
  end

  def test_jsx_name_is_a_noncall_decoy_even_outside_labelled_sites
    extra = Marshal.load(Marshal.dump(@snapshot['edges'][0]))
    extra['id'], extra['provenance']['span'] = 'fake-jsx', span(299, 305)
    @snapshot['edges'] << extra
    report = scorer.score(@snapshot)
    refute report['passed']
    assert_equal ['fake-jsx'], report['decoy_edges']
  end

  def test_shifted_or_shortened_edge_spans_cannot_escape_precision
    extra = Marshal.load(Marshal.dump(@snapshot['edges'][0]))
    extra['id'], extra['provenance']['span'] = 'short-external', span(90, 91)
    @snapshot['edges'] << extra
    report = scorer.score(@snapshot)
    refute report['passed']
    assert_equal 1, report['false_positive_sites']
    assert_equal ['short-external'], report['unanchored_edges']
    extra['provenance']['span'] = span(91, 92)
    report = scorer.score(@snapshot)
    refute report['passed']
    assert_equal ['short-external'], report['unanchored_edges']
  end

  def test_correct_enclosing_call_is_not_attributed_to_nested_calls
    outer = span(10, 20)
    @anchors['files'][0]['calls'][0]['span'] = outer
    @snapshot['edges'][0]['provenance']['span'] = outer
    assert scorer.score(@snapshot)['passed']
  end

  def test_anonymous_line_labels_still_require_exact_source_owned_body
    owner = @anchors['files'][0]['bodies'][0]
    owner['normalized_name'], owner['name_span'] = 'anonymous@0', nil
    @oracle['sites'].each { |site| site['caller'] = 'anonymous@1' }
    @oracle['cross_module_queries'].each { |query| query['caller'] = 'anonymous@1' }
    @snapshot['nodes'][0]['symbol_key'] = 'js_ts:src/entry.tsx#anonymous@0'
    assert scorer.score(@snapshot)['passed']
    owner['callable_span']['start_line'] = 2
    assert_raises(ArgumentError) { scorer }
  end

  def test_changed_source_duplicate_labels_and_invalid_identity_fail
    @snapshot['files'][0]['sha256'] = 'changed'
    assert_raises(ArgumentError) { scorer.score(@snapshot) }
    @snapshot['files'][0]['sha256'] = 'a' * 64
    @snapshot['identity']['dirty'] = true
    assert_raises(ArgumentError) { scorer.score(@snapshot) }
    @snapshot['identity']['dirty'] = false
    @snapshot['identity']['js_ts_semantic'] = nil
    assert_raises(ArgumentError) { scorer.score(@snapshot) }
    @oracle['sites'][1]['id'] = @oracle['sites'][0]['id']
    assert_raises(ArgumentError) { scorer }
  end

  def test_missing_or_reused_call_anchors_and_bad_decoy_labels_fail
    first = @anchors['files'][0]['calls'].shift
    assert_raises(ArgumentError) { scorer }
    @anchors['files'][0]['calls'].unshift(first)
    @oracle['non_call_decoys'][0]['spelling'] = '<Missing>'
    assert_raises(ArgumentError) { scorer }
    @oracle['non_call_decoys'][0]['spelling'] = '<View>'
    @oracle['sites'][1]['byte_offset'] = 11
    @oracle['sites'][1]['spelling'] = 'module.target'
    first['callee_span']['start_byte'] = 11
    assert_raises(ArgumentError) { scorer }
  end
end
