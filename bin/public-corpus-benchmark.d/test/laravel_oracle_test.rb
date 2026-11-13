# frozen_string_literal: true

require 'json'
require 'minitest/autorun'
require_relative '../laravel_oracle'

class LaravelOracleTest < Minitest::Test
  MODEL = 'app/Models/Post.php'
  TARGET = 'app/Models/User.php'
  VIEW = 'resources/views/layouts/page.blade.php'
  PARTIAL = 'resources/views/layouts/header.blade.php'
  DYNAMIC = 'resources/views/dynamic.blade.php'

  def setup
    @snapshot = {
      'identity' => {'language' => 'php', 'commit_sha' => 'a' * 40, 'dirty' => false},
      'files' => [MODEL, TARGET, VIEW, PARTIAL, DYNAMIC].map { |path| source(path).merge('rel_path' => path) },
      'nodes' => [class_node(MODEL, 'Post'), class_node(TARGET, 'User'), file_node(VIEW), file_node(PARTIAL), file_node(DYNAMIC)],
      'edges' => [], 'call_sites' => [], 'report' => {'parse_errors' => 0}
    }
    @association = {
      'policy' => LaravelOracle::ASSOCIATION,
      'source' => '@container:' + class_node(MODEL, 'Post').fetch('id'),
      'target' => '@container:' + class_node(TARGET, 'User').fetch('id'),
      'rel_path' => MODEL, 'start_line' => 12, 'end_line' => 12,
      'declaration' => 'user: belongsTo(App\\Models\\User)'
    }
    @template = {
      'policy' => LaravelOracle::TEMPLATE, 'source' => '@file:' + VIEW,
      'target' => '@file:' + PARTIAL, 'rel_path' => VIEW, 'start_line' => 3, 'end_line' => 3,
      'declaration' => "include 'layouts.header'"
    }
    @topology = {'codemap' => {'identity' => @snapshot.fetch('identity').dup,
                              'policies' => [], 'policy_relations' => [@association, @template]}}
    @oracle = {
      'contract' => LaravelOracle::CONTRACT, 'basis' => 'reviewer_declared public source labels', 'commit' => 'a' * 40,
      'scope' => {'blade_selected_files' => [VIEW]},
      'required_eloquent' => [{
        'id' => 'Post.user', 'source' => source(MODEL), 'owner' => source(MODEL).merge('qualified_name' => 'App\\Models\\Post'),
        'required_target' => source(TARGET).merge('qualified_name' => 'App\\Models\\User'), 'factory' => 'belongsTo',
        'declaration' => {'name' => 'user', 'span' => span(10, 13)}, 'call_span' => span(12)
      }],
      'required_blade' => [{'id' => 'page include', 'source' => source(VIEW), 'required_target' => source(PARTIAL), 'span' => span(3)}],
      'must_remain_unresolved_or_explicitly_out_of_scope' => [
        {'id' => 'Post.dynamic', 'source' => source(MODEL), 'declaration' => {'name' => 'dynamic', 'span' => span(20, 23)}},
        {'id' => 'dynamic include', 'source' => source(DYNAMIC), 'span' => span(6)}
      ]
    }
  end

  def source(path)
    {'path' => path, 'sha256' => "hash of #{path}"}
  end

  def span(first, last = first)
    {'start_line' => first, 'end_line' => last}
  end

  def class_node(path, name)
    symbol = "php:#{path}#App\\Models::#{name}"
    {'id' => 'php:type:' + symbol.delete_prefix('php:'), 'symbol_key' => symbol,
     'kind' => {'kind' => 'type', 'subtype' => 'Class'}, 'provenance' => {'rel_path' => path}}
  end

  def file_node(path)
    {'id' => 'php:file:' + path, 'kind' => {'kind' => 'File'}, 'provenance' => {'rel_path' => path}}
  end

  def verify
    LaravelOracle.verify(@snapshot, @topology, @oracle, reader_scope: @reader_scope)
  end

  def refused(pattern)
    error = assert_raises(ArgumentError) { verify }
    assert_match pattern, error.message
  end

  def test_matching_independent_labels_pass
    result = verify
    assert_equal 1, result.fetch('required_eloquent_declarations')
    assert_equal 1, result.fetch('required_blade_references')
    assert_equal 2, result.fetch('omission_controls')
  end

  def test_wrong_or_dirty_source_and_topology_are_refused
    @snapshot['identity']['dirty'] = true
    refused(/dirty/)
    @snapshot['identity']['dirty'] = false
    @snapshot['identity']['commit_sha'] = 'b' * 40
    refused(/commit differs/)
    @snapshot['identity']['commit_sha'] = 'a' * 40
    @topology['codemap']['identity']['snapshot_id'] = 'another'
    refused(/another snapshot/)
  end

  def test_changed_source_hash_or_conflicting_labels_are_refused
    @snapshot['files'][0]['sha256'] = 'changed'
    refused(/source pin differs/)
    @snapshot['files'][0]['sha256'] = source(MODEL).fetch('sha256')
    @oracle['required_eloquent'][0]['owner']['sha256'] = 'another'
    refused(/conflicting.*labels/)
  end

  def test_source_and_template_parser_errors_are_refused
    @snapshot['report']['parse_errors'] = 1
    refused(/source has parser errors/)
    @snapshot['report']['parse_errors'] = 0
    @snapshot['report']['observations'] = {'blade_parse_errors' => 1}
    refused(/templates have parser errors/)
    @snapshot['report']['observations'].clear
    @snapshot['php'] = {'blade' => [{'kind' => 'parse_error'}]}
    refused(/templates have parser errors/)
  end

  def test_canonical_calls_or_template_definitions_edges_and_measures_are_refused
    @snapshot['edges'] << {'kind' => 'Calls'}
    refused(/canonical calls/)
    @snapshot['edges'].clear
    @snapshot['nodes'] << file_node(VIEW).merge('kind' => {'kind' => 'Function'})
    refused(/contributed a definition/)
    @snapshot['nodes'].pop
    @snapshot['edges'] << {'kind' => 'Contains', 'from' => 'php:file:' + VIEW, 'to' => 'other'}
    refused(/canonical edge/)
    @snapshot['edges'].clear
    @snapshot['file_measures'] = [{'file_id' => 'php:file:' + VIEW}]
    refused(/contributed a measure/)
  end

  def test_missing_or_extra_associations_and_wrong_targets_are_refused
    @topology['codemap']['policy_relations'].delete(@association)
    refused(/association count/)
    @topology['codemap']['policy_relations'] << @association << @association.dup
    refused(/association count/)
    @topology['codemap']['policy_relations'].pop
    @association['target'] = @association['source']
    refused(/association target/)
  end

  def test_wrong_association_line_or_declaration_is_refused
    @association['start_line'] = 11
    refused(/another line/)
    @association['start_line'] = 12
    @association['declaration'] = 'unrelated declaration'
    refused(/another declaration/)
  end

  def test_class_anchors_must_be_unique_and_qualified
    @snapshot['nodes'][1]['symbol_key'] = "php:#{TARGET}#Other\\Models::User"
    refused(/class anchor is not unique/)
    @snapshot['nodes'][1] = class_node(TARGET, 'User')
    @snapshot['nodes'] << class_node(TARGET, 'User')
    refused(/class anchor is not unique/)
  end

  def test_missing_extra_wrong_target_and_wrong_line_blade_relations_are_refused
    @template['target'] = '@file:missing.blade.php'
    refused(/Blade relations differ/)
    @template['target'] = '@file:' + PARTIAL
    @template['start_line'] = 2
    refused(/Blade relations differ/)
    @template['start_line'] = 3
    @topology['codemap']['policy_relations'] << @template.dup
    refused(/Blade relations differ/)
    @topology['codemap']['policy_relations'].delete(@template)
    refused(/Blade relations differ/)
  end

  def test_an_unresolved_summary_cannot_replace_a_required_relation
    @topology['codemap']['policies'] = [{'policy' => LaravelOracle::ASSOCIATION, 'relations' => 0, 'unresolved' => {'dynamic' => 1}}]
    @topology['codemap']['policy_relations'].delete(@association)
    refused(/association count/)
  end

  def test_omission_controls_cannot_acquire_relations
    @topology['codemap']['policy_relations'] << @association.merge('start_line' => 21, 'end_line' => 21)
    refused(/excluded declaration acquired/)
    @topology['codemap']['policy_relations'].pop
    @topology['codemap']['policy_relations'] << @template.merge('source' => '@file:' + DYNAMIC, 'rel_path' => DYNAMIC, 'start_line' => 6, 'end_line' => 6)
    refused(/excluded declaration acquired/)
  end

  def test_snapshot_generated_oracle_and_resolved_template_sites_are_refused
    @oracle['basis'] = 'snapshot'
    refused(/contract or basis/)
    @oracle['basis'] = 'reviewer_declared'
    @snapshot['call_sites'] << {'rel_path' => VIEW, 'category' => 'Resolved'}
    refused(/resolver read/)
  end

  def reviewed_template_refusal
    @reader_scope = {'contract' => 'stele-laravel-blade-reader-scope/v1', 'commit' => @oracle.fetch('commit'), 'reviewed_files' => [source(DYNAMIC)]}
    @snapshot['report']['observations'] = {'blade_parse_errors' => 1}
    @snapshot['php'] = {'blade' => [{'kind' => 'parse_error', 'provenance' => {'rel_path' => DYNAMIC}}]}
  end

  def test_only_source_reviewed_whole_template_refusals_are_accepted
    reviewed_template_refusal
    assert_equal 1, verify.fetch('refused_templates')
    @snapshot['report']['observations']['blade_parse_errors'] = 0
    refused(/template refusals differ/)
    @snapshot['report']['observations']['blade_parse_errors'] = 1
    @snapshot['php']['blade'][0]['provenance']['rel_path'] = PARTIAL
    refused(/template refusals differ/)
  end

  def test_reviewed_refusal_requires_unchanged_source_and_membership
    reviewed_template_refusal
    @reader_scope['reviewed_files'][0]['sha256'] = 'changed'
    refused(/reader scope source pin differs/)
    @reader_scope['reviewed_files'][0] = source(DYNAMIC)
    @snapshot['php']['blade'].clear
    @snapshot['report']['observations']['blade_parse_errors'] = 0
    refused(/template refusals differ/)
  end

  def test_refused_templates_cannot_emit_references_or_relations
    reviewed_template_refusal
    @snapshot['php']['blade'] << {'kind' => 'include', 'provenance' => {'rel_path' => DYNAMIC}}
    refused(/refused template contributed a usable reference/)
    @snapshot['php']['blade'].pop
    @topology['codemap']['policy_relations'] << @template.merge('source' => '@file:' + DYNAMIC, 'rel_path' => DYNAMIC, 'start_line' => 6, 'end_line' => 6)
    refused(/refused template contributed a policy relation/)
  end

  def test_selected_required_templates_cannot_be_excluded_by_a_scope_change
    reviewed_template_refusal
    @reader_scope['reviewed_files'][0] = source(VIEW)
    @snapshot['php']['blade'][0]['provenance']['rel_path'] = VIEW
    refused(/selected required template was refused/)
  end

end
