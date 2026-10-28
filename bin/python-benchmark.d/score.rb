# frozen_string_literal: true
require 'digest'
require 'json'

class PythonBenchmarkScore
  attr_reader :original_commit, :files, :labels

  def initialize(oracle:, oracle_sha256:, anchors:, anchors_sha256:)
    @oracle = pinned_json(oracle, oracle_sha256)
    source = pinned_json(anchors, anchors_sha256)
    @pins = {'oracle_sha256' => oracle_sha256, 'anchors_sha256' => anchors_sha256}
    @original_commit = @oracle.fetch('source_commit')
    raise ArgumentError, 'source anchor original commit differs from oracle' unless source.fetch('original_commit') == @original_commit
    raise ArgumentError, 'unsupported source anchor contract' unless source.fetch('contract') == 'codemap-python-benchmark-anchors/v1'
    @files = unique_index(source.fetch('files'), 'path')
    @labels = @oracle.fetch('calls')
    thresholds = @oracle.fetch('thresholds')
    expected = {'false_positives' => 0, 'minimum_areas' => 3, 'minimum_census' => 50,
                'minimum_supported_recall' => 0.9, 'minimum_supported_share' => 0.2}
    raise ArgumentError, 'acceptance thresholds differ from frozen contract' unless thresholds == expected
    supported = @labels.count { |site| site.fetch('expected_supported') == true }
    raise ArgumentError, 'fixed census is too small' unless @labels.length >= 50
    raise ArgumentError, 'supported share is below acceptance threshold' unless supported * 5 >= @labels.length
    areas = @oracle.fetch('selected_complete_functions')
    raise ArgumentError, 'source areas must be distinct and represented' unless @oracle.fetch('selected_areas').uniq.length >= 3 && areas.length >= 3 && areas.all? { |path, owners| owners.any? && @labels.any? { |site| site.fetch('path') == path && owners.include?(site.fetch('caller')) } }
    keys = @labels.map { |site| location(site) }
    raise ArgumentError, 'duplicate independent label' unless keys.uniq.length == keys.length
    areas.each do |path, owners|
      bodies = owners.map do |owner|
        matches = @files.fetch(path).fetch('definitions').select { |body| body.fetch('qualified_name') == owner }
        raise ArgumentError, 'selected complete function requires one original body' unless matches.length == 1
        matches.first.fetch('span')
      end
      selected = calls(path).select { |call| bodies.any? { |span| span.fetch('start_byte') <= call.fetch('span').fetch('start_byte') && call['span']['end_byte'] <= span['end_byte'] } }
      labelled = @labels.select { |site| site.fetch('path') == path }
      raise ArgumentError, 'frozen labels do not cover the complete selected original bodies' unless selected.map { |call| span_key(call.fetch('span')) }.sort == labelled.map { |site| span_key(site.fetch('span')) }.sort
    end
    @labels.each do |site|
      raise ArgumentError, 'support label must be boolean' unless [true, false].include?(site.fetch('expected_supported'))
      call = calls(site.fetch('path')).find { |value| same_span?(value.fetch('span'), site.fetch('span')) }
      raise ArgumentError, 'label differs from independent original-source call' unless call && %w[callee callee_kind callee_span caller evaluation_context].all? { |field| call.fetch(field) == site.fetch(field) }
      next unless site['expected_supported']
      raise ArgumentError, 'supported site requires an original body caller' unless caller_body(site)
      target_body(site.fetch('expected_target'))
    end
    @decoys = @oracle.fetch('non_call_decoys').flat_map do |decoy|
      matches = @files.fetch(decoy.fetch('path')).fetch('non_call_decoys').select { |anchor| anchor.fetch('spelling') == decoy.fetch('spelling') && anchor.fetch('role') == decoy.fetch('role') }
      raise ArgumentError, 'non-call decoy lacks independent original-source anchor' if matches.empty?
      matches.map { |anchor| [decoy.fetch('path'), anchor.fetch('span')] }
    end
  end

  def score(snapshot, bridge:)
    identity = snapshot.fetch('identity')
    raise ArgumentError, 'require clean native Python snapshot at prepared commit/tree' unless identity['language'] == 'python' && identity.dig('python', 'native').is_a?(Hash) && identity['dirty'] == false && identity['commit_sha'] == bridge.fetch('prepared_commit') && identity['tree_sha'] == bridge.fetch('prepared_tree') && identity['repository'] == bridge.fetch('repository')
    inventory = unique_index(snapshot.fetch('files'), 'rel_path')
    raise ArgumentError, 'snapshot inventory differs from complete independent census' unless inventory.keys.sort == @files.keys.sort
    @files.each { |path, file| raise ArgumentError, 'snapshot source differs from independent anchors' unless inventory.fetch(path).fetch('sha256') == file.fetch('sha256') }
    nodes = unique_index(snapshot.fetch('nodes'), 'id')
    edges = snapshot.fetch('edges').select { |edge| edge.fetch('kind') == 'Calls' }
    sites = snapshot.fetch('call_sites')
    grouped = sites.group_by { |site| location(site) }
    source_calls = @files.flat_map { |path, file| file.fetch('calls').map { |call| [[path, *span_key(call.fetch('span'))], call] } }.to_h
    raise ArgumentError, 'snapshot does not retain complete unique original-source call census' unless grouped.keys.sort == source_calls.keys.sort && grouped.values.all? { |values| values.length == 1 }
    source_calls.each do |key, call|
      raise ArgumentError, 'site spelling differs from original call' unless grouped.fetch(key).first.fetch('raw_name') == call.fetch('callee')
    end
    unanchored = edges.reject do |edge|
      provenance = edge.fetch('provenance')
      call = source_calls[location(provenance)]
      owner = call && caller_body(call.merge('path' => provenance.fetch('rel_path')))
      target = nodes.fetch(edge.fetch('to'))
      call && owner && node_matches?(nodes.fetch(edge.fetch('from')), provenance.fetch('rel_path'), owner) &&
        @files.fetch(target.fetch('provenance').fetch('rel_path')).fetch('definitions').any? { |body| node_matches?(target, target['provenance']['rel_path'], body) }
    end
    by_location = edges.group_by { |edge| location(edge.fetch('provenance')) }
    results = @labels.map do |label|
      admitted = by_location.fetch(location(label), [])
      owner = caller_body(label)
      target = label['expected_supported'] && target_body(label.fetch('expected_target'))
      exact = admitted.length == 1 && label['expected_supported'] && owner && target &&
              node_matches?(nodes.fetch(admitted.first.fetch('from')), label.fetch('path'), owner) &&
              node_matches?(nodes.fetch(admitted.first.fetch('to')), label.fetch('expected_target').fetch('path'), target)
      {'path' => label['path'], 'span' => label['span'], 'supported' => label['expected_supported'],
       'exact_edge' => !!exact, 'false_edges' => exact ? [] : admitted.map { |edge| edge.fetch('id') },
       'category' => grouped.fetch(location(label)).first.fetch('category')}
    end
    decoy_edges = edges.select do |edge|
      value = edge.fetch('provenance')
      call = source_calls[location(value)]
      @decoys.any? { |path, span| value['rel_path'] == path && overlap?(call ? call.fetch('callee_span') : value.fetch('span'), span) }
    end
    supported = results.select { |site| site['supported'] }
    hits = supported.count { |site| site['exact_edge'] }
    false_sites = results.count { |site| !site['false_edges'].empty? }
    {'contract' => 'codemap-python-labelled-score/v1', **@pins, 'original_commit' => @original_commit,
     'prepared_commit' => bridge.fetch('prepared_commit'), 'prepared_tree' => bridge.fetch('prepared_tree'),
     'snapshot_id' => identity.fetch('snapshot_id'),
     'passed' => false_sites.zero? && unanchored.empty? && decoy_edges.empty? && hits * 10 >= supported.length * 9,
     'census_denominator' => results.length, 'supported_denominator' => supported.length,
     'supported_share' => supported.length.fdiv(results.length), 'supported_recall' => hits.fdiv(supported.length),
     'false_positive_sites' => false_sites, 'unanchored_edges' => unanchored.map { |edge| edge.fetch('id') },
     'decoy_edges' => decoy_edges.map { |edge| edge.fetch('id') },
     'complete_call_census' => sites.length, 'complete_call_categories' => sites.group_by { |site| site.fetch('category') }.transform_values(&:length),
     'sites' => results}
  end

  private

  def pinned_json(path, pin)
    raise ArgumentError, 'pin must be lowercase SHA-256' unless /\A[0-9a-f]{64}\z/.match?(pin)
    bytes = File.binread(path)
    raise ArgumentError, 'input digest mismatch' unless Digest::SHA256.hexdigest(bytes) == pin
    JSON.parse(bytes)
  end

  def unique_index(rows, key)
    values = rows.to_h { |row| [row.fetch(key), row] }
    raise ArgumentError, "duplicate #{key}" unless values.length == rows.length
    values
  end

  def calls(path)
    @files.fetch(path).fetch('calls')
  end

  def span_key(span)
    %w[start_byte end_byte start_line end_line].map { |key| span.fetch(key) }
  end

  def same_span?(left, right)
    span_key(left) == span_key(right)
  end

  def location(value)
    [value.fetch(value.key?('rel_path') ? 'rel_path' : 'path'), *span_key(value.fetch('span'))]
  end

  def caller_body(site)
    return unless site['caller'] && site['evaluation_context'] == 'body'
    candidates = @files.fetch(site.fetch('path')).fetch('definitions').select do |body|
      body.fetch('qualified_name') == site['caller'] && !body.fetch('declaration_only') && %w[FunctionDef AsyncFunctionDef].include?(body.fetch('kind')) &&
        body.fetch('span').fetch('start_byte') <= site.fetch('span').fetch('start_byte') && site['span']['end_byte'] <= body['span']['end_byte']
    end
    raise ArgumentError, 'ambiguous original caller body' if candidates.length > 1
    candidates.first
  end

  def target_body(target)
    matches = @files.fetch(target.fetch('path')).fetch('definitions').select do |body|
      body.fetch('qualified_name') == target.fetch('qualified_name') && same_span?(body.fetch('span'), target.fetch('span')) &&
        body.fetch('kind') == 'FunctionDef' && !body.fetch('declaration_only') && body.fetch('decorators').empty?
    end
    raise ArgumentError, 'target requires one undecorated original implementation body' unless matches.length == 1
    matches.first
  end

  def node_matches?(node, path, body)
    value = node.fetch('provenance')
    parent_name = body.fetch('qualified_name').rpartition('.').first
    parent_class = @files.fetch(path).fetch('definitions').any? do |parent|
      parent.fetch('kind') == 'ClassDef' && parent.fetch('qualified_name') == parent_name &&
        parent.fetch('span').fetch('start_byte') <= body.fetch('span').fetch('start_byte') && body['span']['end_byte'] <= parent['span']['end_byte']
    end
    node.fetch('kind') == {'kind' => parent_class ? 'Method' : 'Function'} &&
      %w[FunctionDef AsyncFunctionDef].include?(body.fetch('kind')) && !body.fetch('declaration_only') &&
      value.fetch('rel_path') == path && same_span?(value.fetch('span'), body.fetch('span')) &&
      node.fetch('symbol_key') == "python:#{path}##{body.fetch('qualified_name').split('.').join('::')}"
  end

  def overlap?(left, right)
    left.fetch('start_byte') < right.fetch('end_byte') && right.fetch('start_byte') < left.fetch('end_byte')
  end
end
