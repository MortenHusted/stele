# frozen_string_literal: true

class LaravelOracle
  CONTRACT = 'stele-laravel-source-oracle/v1'
  ASSOCIATION = 'policy:laravel-association-v1/php'
  TEMPLATE = 'policy:laravel-template-render-v1/php'

  def self.verify(snapshot, topology, oracle, reader_scope: nil)
    new(snapshot, topology, oracle, reader_scope: reader_scope).verify
  end

  def initialize(snapshot, topology, oracle, reader_scope: nil)
    @snapshot, @topology, @oracle, @reader_scope = snapshot, topology, oracle, reader_scope
    @relations = topology.fetch('codemap').fetch('policy_relations', [])
  end

  def verify
    identity
    sources
    template_reader_scope
    templates_have_no_canonical_facts
    associations
    templates
    omissions
    {
      'required_eloquent_declarations' => @oracle.fetch('required_eloquent').length,
      'required_blade_references' => @oracle.fetch('required_blade').length,
      'omission_controls' => @oracle.fetch('must_remain_unresolved_or_explicitly_out_of_scope').length,
      'refused_templates' => @snapshot.fetch('php', {}).fetch('blade', []).count { |reference| reference.fetch('kind') == 'parse_error' },
      'policies' => @topology.fetch('codemap').fetch('policies', [])
    }
  end

  private

  def refuse(message)
    raise ArgumentError, message
  end

  def identity
    refuse 'Laravel oracle contract or basis differs' unless @oracle.fetch('contract') == CONTRACT && @oracle.fetch('basis').start_with?('reviewer_declared')
    identity = @snapshot.fetch('identity')
    refuse 'Laravel snapshot language or commit differs' unless identity.fetch('language') == 'php' && identity.fetch('commit_sha') == @oracle.fetch('commit')
    refuse 'Laravel snapshot is dirty' unless identity.fetch('dirty') == false
    refuse 'Laravel topology names another snapshot' unless @topology.fetch('codemap').fetch('identity') == identity
    report = @snapshot.fetch('report')
    refuse 'Laravel source has parser errors' unless report.fetch('parse_errors').zero?
    refuse 'Laravel syntax snapshot contains canonical calls' if @snapshot.fetch('edges').any? { |edge| edge.fetch('kind') == 'Calls' }
  end

  def template_reader_scope
    references = @snapshot.fetch('php', {}).fetch('blade', [])
    errors = references.select { |reference| reference.fetch('kind') == 'parse_error' }
    reported = @snapshot.fetch('report').fetch('observations', {}).fetch('blade_parse_errors', 0)
    unless @reader_scope
      refuse 'Laravel templates have parser errors' if reported.positive? || errors.any?
      return
    end
    refuse 'Laravel reader scope identity differs' unless @reader_scope.fetch('contract') == 'stele-laravel-blade-reader-scope/v1' && @reader_scope.fetch('commit') == @oracle.fetch('commit')
    reviewed = @reader_scope.fetch('reviewed_files')
    files = @snapshot.fetch('files').to_h { |file| [file.fetch('rel_path'), file.fetch('sha256')] }
    reviewed.each do |file|
      refuse "Laravel reader scope source pin differs: #{file.fetch('path')}" unless files[file.fetch('path')] == file.fetch('sha256')
    end
    refused_paths = errors.map { |reference| reference.fetch('provenance').fetch('rel_path') }
    refuse 'Laravel template refusals differ from reviewed source scope' unless refused_paths.sort == reviewed.map { |file| file.fetch('path') }.sort && reported == errors.length
    selected = @oracle.fetch('scope').fetch('blade_selected_files')
    refuse 'a selected required template was refused' unless (refused_paths & selected).empty?
    refuse 'a refused template contributed a usable reference' if references.any? { |reference| reference.fetch('kind') != 'parse_error' && refused_paths.include?(reference.fetch('provenance').fetch('rel_path')) }
    refuse 'a refused template contributed a policy relation' if @relations.any? { |relation| relation.fetch('policy') == TEMPLATE && refused_paths.include?(relation.fetch('rel_path')) }
  end

  def source_pins(value, pins = {})
    case value
    when Hash
      if value.key?('path') && value.key?('sha256') && value.fetch('path').end_with?('.php')
        path, digest = value.values_at('path', 'sha256')
        refuse "conflicting Laravel source labels: #{path}" if pins.key?(path) && pins[path] != digest
        pins[path] = digest
      end
      value.each_value { |item| source_pins(item, pins) }
    when Array
      value.each { |item| source_pins(item, pins) }
    end
    pins
  end

  def sources
    files = @snapshot.fetch('files').to_h { |file| [file.fetch('rel_path'), file.fetch('sha256')] }
    source_pins(@oracle).each do |path, digest|
      refuse "Laravel source pin differs: #{path}" unless files[path] == digest
    end
  end

  def templates_have_no_canonical_facts
    nodes = @snapshot.fetch('nodes')
    template_nodes = nodes.select { |node| node.fetch('provenance').fetch('rel_path').end_with?('.blade.php') }
    refuse 'a Blade template contributed a definition' unless template_nodes.all? { |node| node.fetch('kind').fetch('kind') == 'File' }
    ids = template_nodes.map { |node| node.fetch('id') }
    refuse 'a Blade template contributed a canonical edge' if @snapshot.fetch('edges').any? { |edge| ids.include?(edge.fetch('from')) || ids.include?(edge.fetch('to')) }
    refuse 'a Blade template contributed a measure' if @snapshot.fetch('file_measures', []).any? { |measure| ids.include?(measure.fetch('file_id')) }
    refuse 'a resolver read a Blade template' if @snapshot.fetch('call_sites', []).any? { |site| site.fetch('rel_path').end_with?('.blade.php') && site.fetch('category') == 'Resolved' }
  end

  def class_id(label)
    namespace, separator, name = label.fetch('qualified_name').rpartition('\\')
    name = label.fetch('qualified_name') if separator.empty?
    symbol = namespace.empty? ? name : namespace + '::' + name
    matches = @snapshot.fetch('nodes').select do |node|
      node.fetch('kind') == {'kind' => 'type', 'subtype' => 'Class'} &&
        node.fetch('provenance').fetch('rel_path') == label.fetch('path') &&
        node.fetch('symbol_key') == "php:#{label.fetch('path')}##{symbol}"
    end
    refuse "Laravel class anchor is not unique: #{label.fetch('qualified_name')}" unless matches.length == 1
    '@container:' + matches.first.fetch('id')
  end

  def at_span?(relation, path, span)
    relation.fetch('rel_path') == path && relation.fetch('start_line') >= span.fetch('start_line') && relation.fetch('end_line') <= span.fetch('end_line')
  end

  def line_pair(relation)
    relation.values_at('start_line', 'end_line')
  end

  def associations
    @oracle.fetch('required_eloquent').each do |label|
      declaration = label.fetch('declaration')
      observed = @relations.select { |relation| relation.fetch('policy') == ASSOCIATION && at_span?(relation, label.fetch('source').fetch('path'), declaration.fetch('span')) }
      refuse "Laravel association count differs: #{label.fetch('id')}" unless observed.length == 1
      relation = observed.first
      expected = [class_id(label.fetch('owner')), class_id(label.fetch('required_target'))]
      refuse "Laravel association target differs: #{label.fetch('id')}" unless relation.values_at('source', 'target') == expected
      span = label.fetch('call_span')
      refuse "Laravel association rests on another line: #{label.fetch('id')}" unless line_pair(relation) == span.values_at('start_line', 'end_line')
      written = "#{declaration.fetch('name')}: #{label.fetch('factory')}(#{label.fetch('required_target').fetch('qualified_name')})"
      refuse "Laravel association rests on another declaration: #{label.fetch('id')}" unless relation.fetch('declaration') == written
    end
  end

  def templates
    expected = @oracle.fetch('required_blade').map do |label|
      ['@file:' + label.fetch('source').fetch('path'), '@file:' + label.fetch('required_target').fetch('path'),
       label.fetch('source').fetch('path'), *label.fetch('span').values_at('start_line', 'end_line')]
    end
    selected = @oracle.fetch('scope').fetch('blade_selected_files')
    actual = @relations.select { |relation| relation.fetch('policy') == TEMPLATE && selected.include?(relation.fetch('rel_path')) }.map do |relation|
      relation.values_at('source', 'target', 'rel_path', 'start_line', 'end_line')
    end
    refuse 'Laravel selected Blade relations differ from exact labels' unless actual.sort == expected.sort
  end

  def omissions
    @oracle.fetch('must_remain_unresolved_or_explicitly_out_of_scope').each do |label|
      span = label.key?('declaration') ? label.fetch('declaration').fetch('span') : label.fetch('span')
      policy = label.key?('declaration') ? ASSOCIATION : TEMPLATE
      refuse "Laravel excluded declaration acquired a relation: #{label.fetch('id')}" if @relations.any? do |relation|
        relation.fetch('policy') == policy && at_span?(relation, label.fetch('source').fetch('path'), span)
      end
    end
  end
end
