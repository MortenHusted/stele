# frozen_string_literal: true
require 'digest'
require 'json'

class CppBenchmarkScore
  attr_reader :commit

  def initialize(oracle:, oracle_sha256:, anchors:, anchors_sha256:)
    raise ArgumentError, 'oracle pin must be lowercase SHA-256' unless /\A[0-9a-f]{64}\z/.match?(oracle_sha256)
    bytes = File.binread(oracle)
    raise ArgumentError, 'oracle digest mismatch' unless Digest::SHA256.hexdigest(bytes) == oracle_sha256
    @oracle = JSON.parse(bytes)
    @oracle_sha256 = oracle_sha256
    raise ArgumentError, 'anchor pin must be lowercase SHA-256' unless /\A[0-9a-f]{64}\z/.match?(anchors_sha256)
    @anchors_bytes = File.binread(anchors)
    raise ArgumentError, 'anchor digest mismatch' unless Digest::SHA256.hexdigest(@anchors_bytes) == anchors_sha256
    @files = unique_index(JSON.parse(@anchors_bytes).fetch('files'), 'path')
    @commit = @oracle.fetch('commit')
    @sites = @oracle.fetch('sites')
    @targets = @oracle.fetch('targets')
    sites = unique_index(@sites, 'id')
    locations = @sites.map { |site| [site.fetch('path'), site.fetch('byte_offset')] }
    raise ArgumentError, 'duplicate labelled source site' unless locations.uniq.length == locations.length
    raise ArgumentError, 'oracle needs 60 sites or an explicit census declaration' unless @sites.length >= 60 || @oracle['census'] == true
    queries = @oracle.fetch('cross_module_queries')
    raise ArgumentError, 'oracle needs at least 10 distinct cross-module queries' unless queries.map { |q| q.fetch('site_id') }.uniq.length >= 10
    queries.each do |query|
      site = sites.fetch(query.fetch('site_id'))
      target = @targets.fetch(query.fetch('target'))
      raise ArgumentError, 'invalid cross-module query label' unless site['path'] == query['caller_path'] && site['caller'] == query['caller'] && site['target'] == query['target'] && site['path'] != target['path']
    end
    @sites.each do |site|
      raise ArgumentError, 'invalid oracle classification' unless %w[known_in_repository external unknown].include?(site.fetch('classification'))
      raise ArgumentError, 'oracle regime must be explicit' unless site.fetch('regime').is_a?(String) && !site['regime'].empty?
      @targets.fetch(site.fetch('target')) if site['classification'] == 'known_in_repository'
      raise ArgumentError, 'supported labels must have a known target' if site['regime'] == 'supported' && site['classification'] != 'known_in_repository'
    end
    @target_anchors = @targets.transform_values do |target|
      name, parameters = target.fetch('symbol').split('(', 2)
      unqualified = name.split('::').last + (parameters ? "(#{parameters}" : '')
      bodies = @files.fetch(target.fetch('path')).fetch('bodies').select do |body|
        body.fetch('name_span').fetch('start_line') == target.fetch('definition_line') && matches_label?(body.fetch('normalized_name'), unqualified)
      end
      raise ArgumentError, 'ambiguous independently labelled target body' if bodies.length > 1
      bodies.first
    end
  end

  def score(snapshot)
    identity = snapshot.fetch('identity')
    raise ArgumentError, 'require clean semantic C++ snapshot at oracle commit' unless identity['language'] == 'cpp' && identity['cpp_semantic'].is_a?(Hash) && identity['dirty'] == false && identity['commit_sha'] == @commit
    nodes = unique_index(snapshot.fetch('nodes'), 'id')
    files = unique_index(snapshot.fetch('files'), 'rel_path')
    @files.each { |path, file| raise ArgumentError, 'anchor source digest mismatch' unless files.fetch(path).fetch('sha256') == file.fetch('sha256') }
    edges = snapshot.fetch('edges').select { |edge| edge['kind'] == 'Calls' }.group_by { |edge| location(edge.fetch('provenance')) }
    categories = snapshot.fetch('call_sites').to_h { |site| [location(site), site.fetch('category')] }
    labelled_spans = []
    results = @sites.map do |site|
      selected = @files.fetch(site.fetch('path')).fetch('calls').select { |call| call.fetch('name_span').fetch('start_byte') == site.fetch('byte_offset') }
      raise ArgumentError, 'ambiguous source call anchor' if selected.length > 1
      raise ArgumentError, 'supported site must have an exact call and target anchor' if site['regime'] == 'supported' && (selected.empty? || !@target_anchors[site['target']])
      key = selected.empty? ? nil : location('rel_path' => site['path'], 'span' => selected.first.fetch('span'))
      raise ArgumentError, 'call anchor reused by distinct labels' if key && labelled_spans.include?(key)
      labelled_spans << key if key
      observed = if key
                   edges.fetch(key, [])
                 else
                   edges.select { |(path, first, last), _| path == site['path'] && first <= site['byte_offset'] && site['byte_offset'] < last }.values.flatten
                 end
      expected = @targets[site['target']]
      wrong = observed.reject do |edge|
        caller = body(nodes.fetch(edge.fetch('from')))
        target = body(nodes.fetch(edge.fetch('to')))
        key && expected && @target_anchors[site['target']] && site['classification'] == 'known_in_repository' &&
          caller['path'] == site['path'] && matches_label?(caller['symbol'], site['caller']) &&
          target['path'] == expected.fetch('path') && target['span'] == @target_anchors[site['target']].fetch('span') && matches_label?(target['symbol'], expected.fetch('symbol'))
      end
      {'id' => site.fetch('id'), 'known' => site['classification'] == 'known_in_repository',
       'supported' => site['regime'] == 'supported', 'exact_edge' => observed.length == 1 && wrong.empty?,
       'wrong_edges' => wrong.map { |edge| edge.fetch('id') }, 'admitted_edges' => observed.length,
       'category' => categories[key], 'call_anchor_present' => !selected.empty?}
    end
    supported = results.select { |site| site['supported'] }
    known = results.select { |site| site['known'] }
    raise ArgumentError, 'recall denominators must be nonempty' if supported.empty? || known.empty?
    supported_hits = supported.count { |site| site['exact_edge'] }
    known_hits = known.count { |site| site['exact_edge'] }
    false_positives = results.count { |site| !site['wrong_edges'].empty? || site['admitted_edges'] > 1 }
    {'contract' => 'codemap-cpp-labelled-score/v1', 'oracle_sha256' => @oracle_sha256,
     'anchors_sha256' => Digest::SHA256.hexdigest(@anchors_bytes), 'snapshot_id' => identity.fetch('snapshot_id'),
     'passed' => false_positives.zero? && supported_hits * 10 >= supported.length * 9 && known_hits * 2 >= known.length,
     'false_positive_sites' => false_positives, 'supported_denominator' => supported.length,
     'known_denominator' => known.length, 'supported_recall' => supported_hits.fdiv(supported.length),
     'overall_known_target_recall' => known_hits.fdiv(known.length),
     'classification_counts' => @sites.group_by { |site| site.fetch('classification') }.transform_values(&:length),
     'sites' => results}
  end

  private

  def unique_index(rows, field)
    index = rows.to_h { |row| [row.fetch(field), row] }
    raise ArgumentError, "duplicate #{field}" unless index.length == rows.length
    index
  end

  def location(row)
    span = row.fetch('span')
    [row.fetch('rel_path'), span.fetch('start_byte'), span.fetch('end_byte')]
  end

  def body(node)
    provenance = node.fetch('provenance')
    file = @files.fetch(provenance.fetch('rel_path'))
    matches = file.fetch('bodies').select { |body| body.fetch('span') == provenance.fetch('span') }
    raise ArgumentError, 'body must have one exact source anchor' unless matches.length == 1
    {'path' => provenance['rel_path'], 'span' => provenance.fetch('span'),
     'symbol' => node.fetch('symbol_key').split('#', 2).fetch(1)}
  end

  def matches_label?(actual, expected)
    actual = actual.split('(', 2).first unless expected.include?('(')
    actual = canonical_label(actual)
    expected = canonical_label(expected)
    actual == expected || actual.end_with?("::#{expected}")
  end

  def canonical_label(label)
    return label.gsub(/\s+/, '') unless label.include?('(')
    name, rest = label.split('(', 2)
    parameters, _, suffix = rest.rpartition(')')
    normalized = parameters.split(',').map do |parameter|
      parameter = parameter.strip
      parameter = "#{parameter[6...-1]} const&" if parameter.start_with?('const ') && parameter.end_with?('&') && parameter.count('&') == 1
      parameter.gsub(/\s+/, '')
    end
    "#{name.gsub(/\s+/, '')}(#{normalized.join(',')})#{suffix.gsub(/\s+/, '')}"
  end
end
