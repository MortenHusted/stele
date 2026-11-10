# frozen_string_literal: true

require 'json'

# Verifies a Ruby snapshot and the policy relations `stele topology` derives
# from it against a reviewer-declared template oracle: frozen source hashes,
# a census, the exact call sites of selected templates, relations that must
# and must not exist, and declarations that must relate nothing.
class TemplateOracle
  CONTRACT = 'stele-ruby-templates/v1'

  def self.verify(snapshot, topology, oracle)
    new(snapshot, topology, oracle).verify
  end

  def initialize(snapshot, topology, oracle)
    @snapshot = snapshot
    @topology = topology
    @oracle = oracle
  end

  def verify
    refuse 'template oracle contract or basis differs' unless @oracle.fetch('contract') == CONTRACT && @oracle.fetch('basis') == 'reviewer_declared'
    refuse 'template oracle names another commit' unless @oracle.fetch('commit') == @snapshot.fetch('identity').fetch('commit_sha')
    sources
    census
    sites = call_sites
    relations = policy_relations
    {
      'selected_templates' => @oracle.fetch('templates').length,
      'selected_sites' => sites,
      'required_relations' => relations.fetch('required'),
      'forbidden_relations' => relations.fetch('forbidden'),
      'unrelated_declarations' => relations.fetch('unrelated')
    }
  end

  private

  def refuse(message)
    raise ArgumentError, message
  end

  def template?(path)
    File.extname(path).casecmp?('.erb') && File.basename(path) != File.extname(path)
  end

  # Every file a label rests on is the file the labels were written against.
  def sources
    recorded = @snapshot.fetch('files').to_h { |file| [file.fetch('rel_path'), file.fetch('sha256')] }
    @oracle.fetch('files').each do |path, sha256|
      refuse "source pin differs: #{path}" unless recorded[path] == sha256
    end
  end

  def census
    observed = {
      'templates' => @snapshot.fetch('files').count { |file| template?(file.fetch('rel_path')) },
      'template_parse_errors' => @snapshot.fetch('report').fetch('observations', {}).fetch('template_parse_errors', 0),
      'policies' => @topology.fetch('codemap').fetch('policies', []).to_h do |policy|
        [policy.fetch('policy'), {'relations' => policy.fetch('relations'), 'unresolved' => policy.fetch('unresolved')}]
      end
    }
    refuse "template census differs: #{observed.to_json}" unless observed == @oracle.fetch('census')
  end

  # The selected templates have exactly the labelled call sites, and in a
  # semantic snapshot no resolver read any of them.
  def call_sites
    semantic = @snapshot.fetch('edges').any? { |edge| edge['kind'] == 'Calls' }
    by_path = Hash.new { |hash, path| hash[path] = [] }
    @snapshot.fetch('call_sites').each { |site| by_path[site.fetch('rel_path')] << site }
    @oracle.fetch('templates').sum do |template|
      path = template.fetch('path')
      refuse "selected template is not pinned: #{path}" unless @oracle.fetch('files').key?(path)
      observed = by_path[path]
      refuse "a resolver read #{path}" if semantic && observed.any? { |site| site.fetch('category') != 'Unsupported' }
      expected = template.fetch('sites').map { |site| [site.fetch('line'), site.fetch('name')] }.sort
      actual = observed.map { |site| [site.fetch('span').fetch('start_line'), site.fetch('raw_name')] }.sort
      unless actual == expected
        missing = difference(expected, actual)
        unexpected = difference(actual, expected)
        refuse "call sites differ in #{path}: missing #{missing.inspect}, unexpected #{unexpected.inspect}"
      end
      expected.length
    end
  end

  # Multiset difference: what `left` has more of than `right`.
  def difference(left, right)
    remaining = right.tally
    left.reject { |item| remaining.fetch(item, 0).positive? && (remaining[item] -= 1) }
  end

  # Where the map draws a snapshot node: a file as its group, a class or
  # module as its container, a method under its own id.
  def drawn(node_id)
    case node_id
    when /\A[a-z_]+:file:(.+)\z/ then "@file:#{Regexp.last_match(1)}"
    when /\A[a-z_]+:(?:type|module):/ then "@container:#{node_id}"
    else node_id
    end
  end

  def policy_relations
    relations = @topology.fetch('codemap').fetch('policy_relations', [])
    known = relations.to_h { |relation| [[relation.fetch('policy'), relation.fetch('source'), relation.fetch('target')], relation] }
    @oracle.fetch('required').each do |label|
      relation = known[[label.fetch('policy'), drawn(label.fetch('from')), drawn(label.fetch('to'))]]
      refuse "required relation is missing: #{label.to_json}" unless relation
      refuse "required relation rests on another declaration: #{label.to_json}" unless relations.any? do |candidate|
        candidate.fetch('policy') == label.fetch('policy') && candidate.fetch('source') == drawn(label.fetch('from')) &&
          candidate.fetch('target') == drawn(label.fetch('to')) && candidate.fetch('declaration') == label.fetch('declaration')
      end
    end
    @oracle.fetch('forbidden').each do |label|
      refuse "forbidden relation exists: #{label.to_json}" if known.key?([label.fetch('policy'), drawn(label.fetch('from')), drawn(label.fetch('to'))])
    end
    @oracle.fetch('unrelated').each do |label|
      refuse "unrelated declaration is not pinned: #{label.to_json}" unless @oracle.fetch('files').key?(label.fetch('path'))
      resting = relations.any? do |relation|
        relation.fetch('rel_path') == label.fetch('path') && (!label.key?('line') || relation.fetch('start_line') == label.fetch('line'))
      end
      refuse "a relation rests on a declaration that must relate nothing: #{label.to_json}" if resting
    end
    {'required' => @oracle.fetch('required').length, 'forbidden' => @oracle.fetch('forbidden').length, 'unrelated' => @oracle.fetch('unrelated').length}
  end
end
