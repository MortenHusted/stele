# frozen_string_literal: true

require 'json'
require 'minitest/autorun'
require_relative '../template_oracle'

class TemplateOracleTest < Minitest::Test
  VIEW = 'app/views/posts/show.html.erb'
  PARTIAL = 'app/views/posts/_card.html.erb'
  MODEL = 'app/models/post.rb'
  POLICY = 'policy:template-render/ruby'

  def setup
    @snapshot = {
      'identity' => {'commit_sha' => 'a' * 40},
      'files' => [VIEW, PARTIAL, MODEL].map { |path| {'rel_path' => path, 'sha256' => "hash of #{path}"} },
      'report' => {'observations' => {}},
      'edges' => [{'kind' => 'Calls'}],
      'call_sites' => [
        site(VIEW, 1, 'render'), site(VIEW, 2, 'title'), site(VIEW, 2, 'title'),
        site(MODEL, 3, 'strip', 'Resolved')
      ]
    }
    @topology = {'codemap' => {
      'policies' => [{'policy' => POLICY, 'relations' => 1, 'unresolved' => {'dynamic' => 1}}],
      'policy_relations' => [{
        'policy' => POLICY, 'source' => "@file:#{VIEW}", 'target' => "@file:#{PARTIAL}",
        'declaration' => 'render "card"', 'rel_path' => VIEW, 'start_line' => 1
      }]
    }}
    @oracle = {
      'contract' => 'stele-ruby-templates/v1', 'basis' => 'reviewer_declared', 'commit' => 'a' * 40,
      'census' => {'templates' => 2, 'template_parse_errors' => 0,
                   'policies' => {POLICY => {'relations' => 1, 'unresolved' => {'dynamic' => 1}}}},
      'files' => {VIEW => "hash of #{VIEW}", PARTIAL => "hash of #{PARTIAL}", MODEL => "hash of #{MODEL}"},
      'templates' => [{'path' => VIEW, 'sites' => [
        {'line' => 1, 'name' => 'render'}, {'line' => 2, 'name' => 'title'}, {'line' => 2, 'name' => 'title'}
      ]}],
      'required' => [{'policy' => POLICY, 'from' => "ruby:file:#{VIEW}", 'to' => "ruby:file:#{PARTIAL}", 'declaration' => 'render "card"'}],
      'forbidden' => [{'policy' => POLICY, 'from' => "ruby:file:#{VIEW}", 'to' => "ruby:type:#{MODEL}#Post", 'reason' => 'a model is not rendered'}],
      'unrelated' => [{'path' => VIEW, 'line' => 2, 'reason' => 'no render on this line'}]
    }
  end

  def site(path, line, name, category = 'Unsupported')
    {'rel_path' => path, 'span' => {'start_line' => line}, 'raw_name' => name, 'category' => category}
  end

  def verify
    TemplateOracle.verify(@snapshot, @topology, @oracle)
  end

  def refused(pattern)
    error = assert_raises(ArgumentError) { verify }
    assert_match pattern, error.message
  end

  def test_matching_labels_pass_and_are_counted
    assert_equal(
      {'selected_templates' => 1, 'selected_sites' => 3, 'required_relations' => 1,
       'forbidden_relations' => 1, 'unrelated_declarations' => 1},
      verify
    )
  end

  def test_another_contract_basis_or_commit_is_refused
    @oracle['basis'] = 'snapshot'
    refused(/contract or basis/)
    @oracle['basis'] = 'reviewer_declared'
    @oracle['commit'] = 'b' * 40
    refused(/another commit/)
  end

  def test_a_changed_source_file_is_refused
    @snapshot['files'][1]['sha256'] = 'edited'
    refused(/source pin differs: #{Regexp.escape(PARTIAL)}/)
  end

  def test_the_census_is_exact
    @snapshot['report']['observations'] = {'template_parse_errors' => 1}
    refused(/template census differs/)
    @snapshot['report']['observations'] = {}
    @topology['codemap']['policies'][0]['unresolved'] = {'dynamic' => 2}
    refused(/template census differs/)
  end

  def test_a_missing_and_an_invented_call_site_are_both_refused
    @snapshot['call_sites'].delete_at(2)
    refused(/missing \[\[2, "title"\]\], unexpected \[\]/)
    @snapshot['call_sites'] << site(VIEW, 2, 'title') << site(VIEW, 4, 'link_to')
    refused(/missing \[\], unexpected \[\[4, "link_to"\]\]/)
  end

  def test_a_template_site_a_resolver_read_is_refused
    @snapshot['call_sites'][0]['category'] = 'Resolved'
    refused(/a resolver read/)
  end

  def test_a_missing_required_relation_is_refused
    @topology['codemap']['policy_relations'][0]['target'] = '@file:app/views/posts/_other.html.erb'
    refused(/required relation is missing/)
  end

  def test_a_required_relation_must_rest_on_the_labelled_declaration
    @topology['codemap']['policy_relations'][0]['declaration'] = 'render partial: "card"'
    refused(/rests on another declaration/)
  end

  def test_a_forbidden_relation_is_refused
    @topology['codemap']['policy_relations'] << {
      'policy' => POLICY, 'source' => "@file:#{VIEW}", 'target' => "@container:ruby:type:#{MODEL}#Post",
      'declaration' => 'render Post', 'rel_path' => VIEW, 'start_line' => 3
    }
    refused(/forbidden relation exists/)
  end

  def test_a_relation_resting_on_an_unrelated_declaration_is_refused
    @topology['codemap']['policy_relations'] << {
      'policy' => POLICY, 'source' => "@file:#{VIEW}", 'target' => "@file:#{PARTIAL}",
      'declaration' => 'render "card"', 'rel_path' => VIEW, 'start_line' => 2
    }
    refused(/must relate nothing/)
  end

  def test_labels_about_unpinned_files_are_refused
    @oracle['files'].delete(VIEW)
    refused(/is not pinned/)
  end
end
