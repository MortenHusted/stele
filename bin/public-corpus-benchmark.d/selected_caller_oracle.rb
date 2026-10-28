# frozen_string_literal: true

class SelectedCallerOracle
  def self.verify(snapshot, oracle, fact_source:)
    files = snapshot.fetch('files').to_h { |file| [file.fetch('rel_path'), file.fetch('sha256')] }
    oracle.fetch('files').each do |path, hash|
      raise ArgumentError, "Selected-caller oracle source drift: #{path}" unless files[path] == hash
    end
    nodes = snapshot.fetch('nodes')
    (oracle.fetch('callers') + oracle.fetch('targets')).each do |anchor|
      matching = nodes.select { |node| node['symbol_key'] == anchor.fetch('symbol_key') }
      raise ArgumentError, 'Selected-caller oracle callable anchor differs' unless matching.length == 1 &&
        %w[Function Method].include?(matching.first.fetch('kind').fetch('kind')) &&
        matching.first.fetch('provenance').fetch('rel_path') == anchor.fetch('path') &&
        matching.first.fetch('provenance').fetch('span') == anchor.fetch('span')
    end
    symbols = nodes.to_h { |node| [node.fetch('id'), node.fetch('symbol_key')] }
    callers = oracle.fetch('callers').map { |anchor| anchor.fetch('symbol_key') }
    raise ArgumentError, 'Selected-caller oracle has no selected callers or required edges' if callers.empty? || oracle.fetch('expected_edges').empty?
    selected = snapshot.fetch('edges').select do |edge|
      provenance = edge.fetch('provenance')
      span = provenance.fetch('span')
      in_body = oracle.fetch('callers').any? do |anchor|
        anchor.fetch('path') == provenance.fetch('rel_path') &&
          anchor.fetch('span').fetch('start_byte') <= span.fetch('start_byte') &&
          span.fetch('end_byte') <= anchor.fetch('span').fetch('end_byte')
      end
      edge['kind'] == 'Calls' && (in_body || callers.include?(symbols.fetch(edge.fetch('from'))))
    end
    raise ArgumentError, 'Selected-caller selected calls lack analyzer provenance' unless selected.all? { |edge| edge.fetch('provenance').fetch('fact_source') == fact_source }
    actual = selected.map do |edge|
      provenance = edge.fetch('provenance')
      {'caller' => symbols.fetch(edge.fetch('from')), 'callee' => symbols.fetch(edge.fetch('to')),
       'path' => provenance.fetch('rel_path'), 'span' => provenance.fetch('span')}
    end
    expected = oracle.fetch('expected_edges')
    optional = oracle.fetch('definition_only_sites').map { |site| edge_key(site) }
    required_actual = actual.reject { |edge| optional.include?(edge) }
    raise ArgumentError, 'Selected-caller selected call sites differ from the frozen oracle' unless required_actual.tally == expected.tally
    raise ArgumentError, 'Selected-caller definition-only site emitted duplicate edges' unless (actual - required_actual).tally.values.all? { |count| count == 1 }
    oracle.fetch('forbidden_edges').each do |site|
      raise ArgumentError, 'Selected-caller forbidden target was admitted' if actual.include?(edge_key(site))
    end
    oracle.fetch('noncanonical_sites').each do |site|
      raise ArgumentError, 'Selected-caller noncanonical site became a canonical call' if actual.any? { |edge| same_site?(edge, site) }
    end
    sites = snapshot.fetch('call_sites')
    labelled = expected + oracle.fetch('forbidden_edges') + oracle.fetch('noncanonical_sites') + oracle.fetch('definition_only_sites')
    labelled.each do |site|
      observations = sites.select { |observed| observed.fetch('rel_path') == site.fetch('path') && observed.fetch('span') == site.fetch('span') }
      raise ArgumentError, 'Selected-caller labelled call site is missing or duplicated' unless observations.length == 1
      admitted = actual.any? { |edge| same_site?(edge, site) }
      raise ArgumentError, 'Selected-caller observation category contradicts its admitted edges' unless (observations.first.fetch('category') == 'Resolved') == admitted
    end
    {'selected_callers' => callers.length, 'required_sites' => expected.length,
     'forbidden_targets' => oracle.fetch('forbidden_edges').length,
     'noncanonical_sites' => oracle.fetch('noncanonical_sites').length,
     'definition_only_sites' => oracle.fetch('definition_only_sites').map do |site|
       observed = sites.find { |entry| entry.fetch('rel_path') == site.fetch('path') && entry.fetch('span') == site.fetch('span') }
       {'path' => site.fetch('path'), 'span' => site.fetch('span'), 'category' => observed.fetch('category'),
        'owned_target_admitted' => actual.include?(edge_key(site))}
     end}
  end

  def self.edge_key(site)
    site.slice('caller', 'callee', 'path', 'span')
  end

  def self.same_site?(left, right)
    left.fetch('path') == right.fetch('path') && left.fetch('span') == right.fetch('span')
  end
end
