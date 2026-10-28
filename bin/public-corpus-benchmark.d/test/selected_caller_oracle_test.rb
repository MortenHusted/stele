# frozen_string_literal: true

require 'minitest/autorun'
require_relative '../selected_caller_oracle'

class SelectedCallerOracleTest < Minitest::Test
  def setup
    @body = span(0, 100)
    @site = span(10, 20)
    @trait = span(30, 40)
    @closure = span(50, 60)
    @oracle = {
      'files' => {'src/lib.rs' => 'source-hash'},
      'callers' => [anchor('caller', @body)],
      'targets' => [anchor('target', span(101, 150)), anchor('decoy', span(151, 200))],
      'expected_edges' => [label('target', @site)],
      'forbidden_edges' => [label('decoy', @site)],
      'noncanonical_sites' => [label(nil, @closure)],
      'definition_only_sites' => [label('target', @trait)]
    }
    @snapshot = {
      'files' => [{'rel_path' => 'src/lib.rs', 'sha256' => 'source-hash'}],
      'nodes' => (@oracle['callers'] + @oracle['targets']).map do |entry|
        {'id' => entry['symbol_key'], 'symbol_key' => entry['symbol_key'], 'kind' => {'kind' => 'Function'},
         'provenance' => {'rel_path' => entry['path'], 'span' => entry['span']}}
      end,
      'edges' => [edge('target', @site)],
      'call_sites' => [observation(@site, 'Resolved'), observation(@trait, 'StandardLibrary'), observation(@closure, 'Unmapped')]
    }
  end

  def test_accepts_exact_required_sites_and_reports_the_trait_observation
    result = SelectedCallerOracle.verify(@snapshot, @oracle, fact_source: 'RustAnalyzer')
    assert_equal 1, result['required_sites']
    assert_equal 'StandardLibrary', result['definition_only_sites'].first['category']
    refute result['definition_only_sites'].first['owned_target_admitted']
  end

  def test_accepts_only_the_declared_trait_target_at_its_exact_site
    @snapshot['edges'] << edge('target', @trait)
    @snapshot['call_sites'][1]['category'] = 'Resolved'
    assert SelectedCallerOracle.verify(@snapshot, @oracle, fact_source: 'RustAnalyzer')['definition_only_sites'].first['owned_target_admitted']
    @snapshot['edges'][-1] = edge('decoy', @trait)
    assert_raises(ArgumentError) { SelectedCallerOracle.verify(@snapshot, @oracle, fact_source: 'RustAnalyzer') }
  end

  def test_rejects_missing_extra_duplicate_wrong_target_and_wrong_span_calls
    [[], [edge('decoy', @site)], [edge('target', span(11, 21))],
     [edge('target', @site), edge('target', @site)],
     [edge('target', @site), edge('target', @closure)]].each do |edges|
      assert_raises(ArgumentError) { SelectedCallerOracle.verify(@snapshot.merge('edges' => edges), @oracle, fact_source: 'RustAnalyzer') }
    end
  end

  def test_a_wrong_caller_cannot_hide_an_extra_edge_inside_the_selected_body
    @snapshot['edges'] << edge('decoy', @closure).merge('from' => 'rust:src/lib.rs#target')
    assert_raises(ArgumentError) { SelectedCallerOracle.verify(@snapshot, @oracle, fact_source: 'RustAnalyzer') }
  end

  def test_rejects_drifted_files_nodes_and_fact_sources
    @snapshot['files'][0]['sha256'] = 'different'
    assert_raises(ArgumentError) { SelectedCallerOracle.verify(@snapshot, @oracle, fact_source: 'RustAnalyzer') }
    @snapshot['files'][0]['sha256'] = 'source-hash'
    @snapshot['nodes'][0]['provenance']['span'] = span(1, 100)
    assert_raises(ArgumentError) { SelectedCallerOracle.verify(@snapshot, @oracle, fact_source: 'RustAnalyzer') }
    @snapshot['nodes'][0]['provenance']['span'] = @body
    @snapshot['edges'][0]['provenance']['fact_source'] = 'TreeSitter'
    assert_raises(ArgumentError) { SelectedCallerOracle.verify(@snapshot, @oracle, fact_source: 'RustAnalyzer') }
  end

  def test_missing_or_duplicate_observations_do_not_count_as_coverage
    @snapshot['call_sites'].pop
    assert_raises(ArgumentError) { SelectedCallerOracle.verify(@snapshot, @oracle, fact_source: 'RustAnalyzer') }
    @snapshot['call_sites'] << observation(@closure, 'Unmapped') << observation(@closure, 'Unmapped')
    assert_raises(ArgumentError) { SelectedCallerOracle.verify(@snapshot, @oracle, fact_source: 'RustAnalyzer') }
  end

  def test_categories_must_agree_with_owned_edge_admission
    @snapshot['call_sites'][0]['category'] = 'Unmapped'
    assert_raises(ArgumentError) { SelectedCallerOracle.verify(@snapshot, @oracle, fact_source: 'RustAnalyzer') }
    @snapshot['call_sites'][0]['category'] = 'Resolved'
    @snapshot['call_sites'][1]['category'] = 'Resolved'
    assert_raises(ArgumentError) { SelectedCallerOracle.verify(@snapshot, @oracle, fact_source: 'RustAnalyzer') }
  end

  def test_ruby_requires_its_own_semantic_fact_source
    @snapshot['edges'][0]['provenance']['fact_source'] = 'RubyLsp'
    assert SelectedCallerOracle.verify(@snapshot, @oracle, fact_source: 'RubyLsp')
    assert_raises(ArgumentError) { SelectedCallerOracle.verify(@snapshot, @oracle, fact_source: 'RustAnalyzer') }
  end

  private

  def span(first, last)
    {'start_byte' => first, 'end_byte' => last, 'start_line' => 1, 'end_line' => 1, 'start_col' => first, 'end_col' => last}
  end

  def anchor(name, source_span)
    {'symbol_key' => "rust:src/lib.rs##{name}", 'path' => 'src/lib.rs', 'span' => source_span}
  end

  def label(target, source_span)
    {'caller' => 'rust:src/lib.rs#caller', 'callee' => "rust:src/lib.rs##{target}", 'path' => 'src/lib.rs', 'span' => source_span}
  end

  def edge(target, source_span)
    {'kind' => 'Calls', 'from' => 'rust:src/lib.rs#caller', 'to' => "rust:src/lib.rs##{target}",
     'provenance' => {'rel_path' => 'src/lib.rs', 'span' => source_span, 'fact_source' => 'RustAnalyzer'}}
  end

  def observation(source_span, category)
    {'rel_path' => 'src/lib.rs', 'span' => source_span, 'category' => category}
  end
end
