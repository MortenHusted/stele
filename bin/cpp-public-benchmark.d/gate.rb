# frozen_string_literal: true
require 'fileutils'
require 'json'
require 'digest'
require 'rbconfig'
require 'tty-command'
require_relative '../prepare-cpp-benchmark.d/preparation'
require_relative '../public-corpus-benchmark.d/gate'

class CppPublicGate
  def initialize
    @root = File.expand_path('../..', __dir__)
    @preparation = CppBenchmarkPreparation.new
    @contract = @preparation.contract
    @runner = TTY::Command.new(printer: :null)
  end

  def run(repo:, profile:, cargo:, lemmaspec:, out:)
    repo = @preparation.verify_source(repo)
    profile = JSON.parse(File.read(profile))
    raise ArgumentError, 'profile source or translation units differ from public pin' unless File.realpath(profile.fetch('roots').fetch('source')) == repo && profile.fetch('translation_units') == @contract.fetch('translation_units')
    cargo, lemmaspec = [cargo, lemmaspec].map { |path| File.expand_path(path) }
    raise ArgumentError, 'explicit Cargo and LemmaSpec binaries must be executable' unless [cargo, lemmaspec].all? { |path| File.executable?(path) }
    @preparation.verify_sdk(profile.fetch('roots').fetch('sysroot'))
    @preparation.verify_operator(profile.fetch('runtime'))
    consumer = @contract.fetch('consumer')
    CppBenchmarkPreparation.verify_pin(lemmaspec, consumer.fetch('binary_sha256'))
    raise ArgumentError, 'LemmaSpec version differs from pinned consumer' unless @runner.run(lemmaspec, '--version').out.strip == consumer.fetch('version_output')
    @out = CppBenchmarkPreparation.new_output(out, @runner)
    runtime_path = File.join(@out, 'runtime.json')
    write('runtime.json', profile.fetch('runtime'))
    @env = {'CODEMAP_CPP_RUNTIME_MANIFEST' => runtime_path, 'LEMMASPEC_BIN' => lemmaspec}
    command('build', cargo, 'build', '--locked', '--release', '-p', 'stele')
    binary = File.join(@root, 'target/release/stele')
    @env['CODEMAP_CPP_CLI'] = binary
    write('source-manifest.json', @preparation.source_manifest(repo))
    anchors = File.join(@out, 'anchors.json')
    @env.merge!('CODEMAP_CPP_SOURCE_MANIFEST' => File.join(@out, 'source-manifest.json'), 'CODEMAP_CPP_ANCHOR_REPORT' => anchors)
    native_test(cargo, 'anchors', @contract.fetch('source_anchors').fetch('export_test'))
    CppBenchmarkPreparation.verify_pin(anchors, @contract.fetch('source_anchors').fetch('sha256'))
    %w[production_session_matches_the_independent_labelled_calls production_session_rejects_original_input_drift production_session_fails_when_the_owned_tool_exits production_guard_denies_network_execution_and_outside_writes].each do |name|
      native_test(cargo, name, 'cpp_lsp::tests::' + name)
    end
    @env['CODEMAP_CPP_CANONICAL_SNAPSHOT_OUT'] = File.join(@out, 'fixture.json')
    native_test(cargo, 'canonical-fixture', 'cpp_lsp::tests::canonical_extraction_matches_the_independent_labelled_calls')
    @env['CODEMAP_CPP_CANONICAL_SNAPSHOT'] = @env.fetch('CODEMAP_CPP_CANONICAL_SNAPSHOT_OUT')
    name = 'guarded_cpp_fixture_projects_the_independent_oracle_calls'
    observed = command('positive-consumer', cargo, 'test', '--locked', '--release', '-p', 'codemap-lemmaspec', '--test', 'positive_path', '--', '--ignored', '--color', 'never', '--exact', name)
    PublicCorpusGate.verify_oracle(observed.out, name)
    snapshots = []
    %w[a b].each do |label|
      checkout = File.join(@out, 'checkout-' + label)
      command('clone-' + label, 'git', 'clone', '--quiet', '--no-local', '--no-hardlinks', '--depth', '1', '--single-branch', '--no-tags', '--no-checkout', repo, checkout)
      command('checkout-' + label, 'git', '-C', checkout, 'checkout', '--quiet', '--detach', @contract.fetch('commit'))
      command('origin-' + label, 'git', '-C', checkout, 'remote', 'set-url', 'origin', @contract.fetch('repository'))
      @preparation.verify_source(checkout)
      @contract.fetch('query_lanes').each do |lanes|
        config = rebound_profile(profile, repo, checkout, label, lanes)
        name = "#{label}-#{lanes}"
        write('profile-' + name + '.json', config)
        pin = @contract.fetch('application_oracle')
        command('score-' + name, RbConfig.ruby, File.join(@root, 'bin/cpp-benchmark'), '--repo', checkout, '--profile', File.join(@out, 'profile-' + name + '.json'), '--stele', binary, '--oracle', File.join(@root, pin.fetch('path')), '--oracle-sha256', pin.fetch('sha256'), '--anchors', anchors, '--anchors-sha256', @contract.fetch('source_anchors').fetch('sha256'), '--out-dir', File.join(@out, name))
        snapshots << File.binread(File.join(@out, name, 'snapshot.json'))
        @preparation.verify_source(checkout)
      end
    end
    raise ArgumentError, 'independent checkout or query-lane snapshots differ' unless snapshots.uniq.length == 1
    result = {'contract' => 'stele-cpp-public-corpus-result/v1', 'passed' => true, 'snapshot_sha256' => Digest::SHA256.hexdigest(snapshots.first), 'stele_sha256' => Digest::SHA256.file(binary).hexdigest, 'scope' => 'original guarded fixture, drift/exit and OS-policy controls, positive consumer, frozen selected public callers and complete byte equality across independent checkouts and query lanes'}
    write('result.json', result)
    result
  end

  private

  def rebound_profile(profile, original, checkout, label, lanes)
    config = Marshal.load(Marshal.dump(profile))
    commands = JSON.parse(File.read(config.fetch('compilation_database')))
    commands.each do |entry|
      %w[directory file].each { |key| entry[key] = rebase_path(entry.fetch(key), original, checkout) }
      entry['arguments'] = entry.fetch('arguments').map { |arg| rebase_path(arg, original, checkout) }
    end
    database = "commands-#{label}.json"
    write(database, commands) unless File.exist?(File.join(@out, database))
    config['compilation_database'] = File.join(@out, database)
    config['roots']['source'] = checkout
    config['query_lanes'] = lanes
    config
  end

  def rebase_path(path, original, checkout)
    return checkout if path == original
    path.start_with?(original + '/') ? checkout + path.delete_prefix(original) : path
  end

  def native_test(cargo, label, name)
    observed = command(label, cargo, 'test', '--locked', '--release', '-p', 'codemap', '--lib', '--', '--ignored', '--color', 'never', '--exact', name)
    PublicCorpusGate.verify_oracle(observed.out, name)
  end

  def write(name, value)
    CppBenchmarkPreparation.write(@out, name, value)
  end

  def command(label, *args)
    warn "cpp-public-benchmark: #{label}"
    result = @runner.run!(*args, chdir: @root, env: @env) { |_stdout, stderr| $stderr.write(stderr.to_s) }
    log = File.join(@out, label + '.log')
    File.open(log, 'w', 0o600) { |file| file.write(result.out + result.err) }
    raise ArgumentError, "#{label} failed; see #{log}" unless result.success?
    result
  end
end
