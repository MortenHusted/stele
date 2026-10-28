# frozen_string_literal: true
require 'digest'
require 'json'

class JsTsBenchmarkScore
  attr_reader :commit

  def initialize(oracle:, oracle_sha256:, anchors:, anchors_sha256:)
    @oracle = pinned_json(oracle, oracle_sha256)
    source = pinned_json(anchors, anchors_sha256)
    @oracle_pin, @anchor_pin = oracle_sha256, anchors_sha256
    @commit = @oracle.fetch('commit')
    raise ArgumentError, 'source anchor commit differs from oracle' unless source.fetch('commit') == @commit
    raise ArgumentError, 'unsupported source anchor contract' unless source.fetch('contract') == 'codemap-js-ts-source-anchors/v1'
    @files = unique_index(source.fetch('files'), 'path')
    @sites = @oracle.fetch('sites')
    labels = unique_index(@sites, 'id')
    raise ArgumentError, 'at least 60 labels or an explicit census required' unless @sites.length >= 60 || @oracle['census'] == true
    raise ArgumentError, 'duplicate labelled location' unless @sites.map { |s| [s.fetch('path'), s.fetch('byte_offset')] }.uniq.length == @sites.length
    @oracle.fetch('files').each do |path, pin|
      raise ArgumentError, 'source anchor pin differs from original oracle' unless @files.fetch(path).fetch('sha256') == pin.fetch('sha256')
    end
    @targets = @oracle.fetch('targets')
    @target_bodies = @targets.transform_values do |target|
      matches = @files.fetch(target.fetch('path')).fetch('bodies').select do |body|
        body['name_span'] && body['name_span'].fetch('start_line') == target.fetch('definition_line') && body.fetch('normalized_name') == target.fetch('symbol')
      end
      raise ArgumentError, 'target requires one independently labelled source body' unless matches.length == 1
      matches.first
    end
    @sites.each do |site|
      raise ArgumentError, 'unknown oracle classification' unless %w[known_in_repository external unknown].include?(site.fetch('classification'))
      raise ArgumentError, 'empty regime' unless site.fetch('regime').is_a?(String) && !site['regime'].empty?
      @targets.fetch(site.fetch('target')) if site['classification'] == 'known_in_repository'
      raise ArgumentError, 'supported label requires known target' if site['regime'] == 'supported' && site['classification'] != 'known_in_repository'
    end
    queries = @oracle.fetch('cross_module_queries')
    raise ArgumentError, 'at least 10 distinct caller/target queries required' unless queries.map { |q| q.fetch('site_id') }.uniq.length >= 10
    queries.each do |query|
      site = labels.fetch(query.fetch('site_id'))
      raise ArgumentError, 'query differs from independent label' unless site['path'] == query['caller_path'] && site['caller'] == query['caller'] && site['target'] == query['target']
      raise ArgumentError, 'cross-module query requires a different target file' unless site['path'] != @targets.fetch(site.fetch('target')).fetch('path')
    end
    @decoys = @oracle.fetch('non_call_decoys').map do |decoy|
      spelling = decoy.fetch('spelling')[/\A<([^\s>]+)/, 1]
      names = @files.fetch(decoy.fetch('path')).fetch('jsx_names').select { |name| name.fetch('span').fetch('start_line') == decoy.fetch('line') && name.fetch('name') == spelling }
      raise ArgumentError, 'decoy requires one exact parsed JSX name' unless names.length == 1
      [decoy.fetch('path'), names.first.fetch('span')]
    end
    @calls = @sites.to_h { |site| [site.fetch('id'), source_call(site)] }
    locations = @sites.map { |site| location('rel_path' => site.fetch('path'), 'span' => @calls.fetch(site.fetch('id')).fetch('span')) }
    raise ArgumentError, 'distinct labels reuse one source call' unless locations.uniq.length == locations.length
    @calls.each do |id, call|
      site = labels.fetch(id)
      next unless site['regime'] == 'supported'
      raise ArgumentError, 'supported call lacks exact source caller' unless source_caller(site, call)
    end
    pairs = queries.map do |query|
      site = labels.fetch(query.fetch('site_id'))
      caller = source_caller(site, @calls.fetch(site.fetch('id')))
      raise ArgumentError, 'query lacks an exact source caller' unless caller
      target = site.fetch('target')
      [site.fetch('path'), caller.fetch('span'), @targets.fetch(target).fetch('path'), @target_bodies.fetch(target).fetch('span')]
    end
    raise ArgumentError, 'at least 10 distinct source caller/target pairs required' unless pairs.uniq.length >= 10
  end

  def score(snapshot)
    identity = snapshot.fetch('identity')
    raise ArgumentError, 'require clean semantic JS/TS snapshot at oracle commit' unless identity['language'] == 'js_ts' && identity['js_ts_semantic'].is_a?(Hash) && identity['dirty'] == false && identity['commit_sha'] == @commit
    nodes = unique_index(snapshot.fetch('nodes'), 'id')
    files = unique_index(snapshot.fetch('files'), 'rel_path')
    @files.each { |path, file| raise ArgumentError, 'snapshot source differs from independent anchors' unless files.fetch(path).fetch('sha256') == file.fetch('sha256') }
    calls = snapshot.fetch('edges').select { |edge| edge['kind'] == 'Calls' }
    edges = calls.group_by { |edge| location(edge.fetch('provenance')) }
    source_locations = @files.flat_map { |path, file| file.fetch('calls').map { |call| location('rel_path' => path, 'span' => call.fetch('span')) } }.to_h { |key| [key, true] }
    unanchored_edges = calls.select { |edge| @files.key?(edge.fetch('provenance').fetch('rel_path')) && !source_locations.key?(location(edge.fetch('provenance'))) }.map { |edge| edge.fetch('id') }
    categories = snapshot.fetch('call_sites').to_h { |site| [location(site), site.fetch('category')] }
    results = @sites.map do |site|
      anchor = @calls.fetch(site.fetch('id'))
      key = location('rel_path' => site['path'], 'span' => anchor['span'])
      observed = (edges.fetch(key, []) + calls.select do |edge|
        provenance = edge.fetch('provenance')
        span, name = provenance.fetch('span'), anchor.fetch('name_span')
        !source_locations.key?(location(provenance)) && provenance['rel_path'] == site['path'] && span.fetch('start_byte') < name.fetch('end_byte') && name.fetch('start_byte') < span.fetch('end_byte')
      end).uniq
      target = @targets[site['target']]
      owner = source_caller(site, anchor)
      wrong = observed.reject do |edge|
        location(edge.fetch('provenance')) == key && target && owner && site['classification'] == 'known_in_repository' &&
          node_matches?(nodes.fetch(edge.fetch('from')), site['path'], owner) &&
          node_matches?(nodes.fetch(edge.fetch('to')), target.fetch('path'), @target_bodies.fetch(site['target']))
      end
      {'id' => site['id'], 'known' => site['classification'] == 'known_in_repository',
       'supported' => site['regime'] == 'supported', 'exact_edge' => observed.length == 1 && wrong.empty?,
       'wrong_edges' => wrong.map { |edge| edge.fetch('id') }, 'admitted_edges' => observed.length,
       'category' => categories[key]}
    end
    decoy_edges = decoy_admissions(calls)
    supported, known = results.select { |s| s['supported'] }, results.select { |s| s['known'] }
    raise ArgumentError, 'recall denominators must be nonempty' if supported.empty? || known.empty?
    supported_hits, known_hits = supported.count { |s| s['exact_edge'] }, known.count { |s| s['exact_edge'] }
    false_positives = results.count { |s| !s['wrong_edges'].empty? || s['admitted_edges'] > 1 }
    {'contract' => 'codemap-js-ts-labelled-score/v1', 'oracle_sha256' => @oracle_pin, 'anchors_sha256' => @anchor_pin,
     'snapshot_id' => identity.fetch('snapshot_id'), 'passed' => false_positives.zero? && decoy_edges.empty? && unanchored_edges.empty? && supported_hits * 10 >= supported.length * 9 && known_hits * 2 >= known.length,
     'unanchored_edges' => unanchored_edges,
     'false_positive_sites' => false_positives, 'decoy_edges' => decoy_edges, 'supported_denominator' => supported.length,
     'known_denominator' => known.length, 'supported_recall' => supported_hits.fdiv(supported.length),
     'overall_known_target_recall' => known_hits.fdiv(known.length),
     'classification_counts' => @sites.group_by { |s| s.fetch('classification') }.transform_values(&:length),
     'unprofiled_observations' => snapshot.fetch('call_sites').count { |site| site['category'] == 'Unprofiled' },
     'sites' => results}
  end

  private

  def pinned_json(path, pin)
    raise ArgumentError, 'pin must be lowercase SHA-256' unless /\A[0-9a-f]{64}\z/.match?(pin)
    bytes = File.binread(path)
    raise ArgumentError, 'input digest mismatch' unless Digest::SHA256.hexdigest(bytes) == pin
    JSON.parse(bytes)
  end

  def unique_index(rows, field)
    index = rows.to_h { |row| [row.fetch(field), row] }
    raise ArgumentError, "duplicate #{field}" unless index.length == rows.length
    index
  end

  def source_call(site)
    spelling = site.fetch('spelling')
    field = spelling.include?('.') && !spelling.start_with?('.') ? 'callee_span' : 'name_span'
    byte = site.fetch('byte_offset') + (spelling.start_with?('.') ? 1 : 0)
    calls = @files.fetch(site.fetch('path')).fetch('calls').select { |call| call.fetch(field).fetch('start_byte') == byte }
    raise ArgumentError, 'label requires one exact source call' unless calls.length == 1
    calls.first
  end

  def source_caller(site, call)
    return unless call['caller_body']
    bodies = @files.fetch(site.fetch('path')).fetch('bodies').select { |body| body.fetch('body_span') == call['caller_body'] && caller_label?(body, site.fetch('caller')) }
    raise ArgumentError, 'source call has ambiguous labelled caller' if bodies.length > 1
    bodies.first
  end

  def caller_label?(body, label)
    if /\Aanonymous@[0-9]+\z/.match?(label)
      body['name_span'].nil? && body.fetch('normalized_name').start_with?('anonymous@') && body.fetch('callable_span').fetch('start_line') == label.split('@').last.to_i
    else
      body.fetch('normalized_name') == label
    end
  end

  def node_matches?(node, path, body)
    provenance = node.fetch('provenance')
    node.fetch('kind') == body.fetch('kind') && provenance['rel_path'] == path && provenance['span'] == body.fetch('span') &&
      node.fetch('symbol_key') == "js_ts:#{path}##{(body.fetch('scope') + [body.fetch('normalized_name')]).join('::')}"
  end

  def decoy_admissions(edges)
    @decoys.flat_map do |path, name|
      edges.select do |edge|
        source = edge.fetch('provenance')
        span = source.fetch('span')
        source['rel_path'] == path && span.fetch('start_byte') < name.fetch('end_byte') && name.fetch('start_byte') < span.fetch('end_byte')
      end.map { |edge| edge.fetch('id') }
    end.uniq
  end

  def location(row)
    span = row.fetch('span')
    [row.fetch('rel_path'), span.fetch('start_byte'), span.fetch('end_byte')]
  end
end
