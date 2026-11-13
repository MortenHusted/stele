# frozen_string_literal: true

require 'digest'
require 'json'
require 'tmpdir'
require 'tty-command'
require_relative 'selected_caller_oracle'
require_relative 'template_oracle'
require_relative 'laravel_oracle'

class PublicCorpusGate
  attr_reader :contract

  def initialize(language:, corpus: nil)
    raise ArgumentError, 'language must be php, go, rust or ruby' unless %w[php go rust ruby].include?(language)
    @root = File.expand_path('../..', __dir__)
    descriptor = {'rust' => 'rust/ripgrep.json', 'ruby' => 'ruby/mastodon.json'}.fetch(language, "#{language}.json")
    if corpus
      # A language may declare further corpora beside its default one.
      raise ArgumentError, 'corpus must be a declared name' unless corpus.match?(/\A[a-z0-9_]+\z/)
      descriptor = "#{language}/#{corpus}.json"
      raise ArgumentError, "#{language} declares no corpus named #{corpus}" unless File.file?(File.join(@root, 'benchmarks', descriptor))
    end
    @contract = JSON.parse(File.read(File.join(@root, 'benchmarks', descriptor)))
    raise ArgumentError, 'declared corpus is for another language' unless @contract.fetch('language') == language
    [@contract.fetch('oracle'), @contract.fetch('tool_lock'), @contract['application_oracle'], @contract['template_oracle'], @contract['framework_profile'], @contract['reader_scope']].compact.each do |pin|
      raise ArgumentError, 'declared oracle or tool lock has drifted' unless Digest::SHA256.file(File.join(@root, pin.fetch('path'))).hexdigest == pin.fetch('sha256')
    end
    @runner = TTY::Command.new(printer: :null)
  end

  def self.verify_snapshot(snapshot, contract, semantic: true)
    identity = snapshot.fetch('identity')
    raise ArgumentError, 'snapshot language or commit differs from the public pin' unless identity.fetch('language') == contract.fetch('language') && identity.fetch('commit_sha') == contract.fetch('commit')
    raise ArgumentError, 'snapshot is dirty' unless identity.fetch('dirty') == false
    raise ArgumentError, 'snapshot is below the accepted corpus size' unless snapshot.fetch('report').fetch('files_scanned') > contract.fetch('minimum_files_exclusive')
    calls = snapshot.fetch('edges').count { |edge| edge['kind'] == 'Calls' }
    raise ArgumentError, 'snapshot call census differs from the requested mode' unless calls.positive? == semantic
    {'files_scanned' => snapshot.fetch('report').fetch('files_scanned'), 'calls' => calls}
  end

  def self.verify_oracle(output, name)
    named_pass = output.lines.any? { |line| line.strip == "test #{name} ... ok" }
    one_test = output.include?('test result: ok. 1 passed; 0 failed; 0 ignored;')
    raise ArgumentError, 'the declared labelled oracle did not execute and pass exactly once' unless named_pass && one_test
  end

  def run(repo:, out:, bundle: nil, cargo_home: nil)
    repo = File.realpath(repo)
    @out = File.join(File.realpath(File.dirname(out)), File.basename(out))
    raise ArgumentError, 'output must be a new directory outside Git repositories' if File.exist?(@out) || File.symlink?(@out) || @runner.run!('git', '-C', File.dirname(@out), 'rev-parse', '--show-toplevel').success?
    head = @runner.run('git', '-C', repo, 'rev-parse', 'HEAD').out.strip
    clean = @runner.run('git', '-C', repo, 'status', '--porcelain').out.empty?
    raise ArgumentError, 'source must be clean at the exact public corpus commit' unless head == @contract.fetch('commit') && clean
    return run_framework(repo, head, bundle, cargo_home) if @contract['mode'] == 'framework-syntax'

    lock = @contract.fetch('tool_lock')
    if @contract.fetch('language') == 'rust'
      raise ArgumentError, '--cargo-home is required for Rust; --bundle is not supported' unless cargo_home && !bundle
      cargo_home = File.realpath(cargo_home)
      raise ArgumentError, 'Rust Cargo home must be named .cargo for dependency classification' unless File.basename(cargo_home) == '.cargo'
      env = {'CARGO_HOME' => cargo_home, 'RUSTUP_TOOLCHAIN' => @contract.fetch('toolchain'), 'CARGO_NET_OFFLINE' => 'true', 'RUSTUP_NO_UPDATE_CHECK' => '1'}
    else
      raise ArgumentError, '--cargo-home is only supported for Rust' if cargo_home
      bundle = File.realpath(bundle || File.join(@root, File.dirname(lock.fetch('path'))))
      raise ArgumentError, 'operator bundle lock differs from the public contract' unless Digest::SHA256.file(File.join(bundle, File.basename(lock.fetch('path')))).hexdigest == lock.fetch('sha256')
      entrypoint = @contract['bundle_entrypoint']
      env = {@contract.fetch('bundle_environment') => entrypoint ? File.join(bundle, entrypoint) : bundle}
    end
    Dir.mkdir(@out, 0o700)
    command('build', 'cargo', 'build', '--locked', '--release', '-p', 'stele', chdir: @root, env: env)
    oracle = @contract.fetch('oracle')
    observed = command('oracle', 'cargo', 'test', '--locked', '-p', 'codemap', '--test', oracle.fetch('test_target'), '--', '--ignored', '--color', 'never', '--exact', oracle.fetch('test_name'), chdir: @root, env: env)
    self.class.verify_oracle(observed.out, oracle.fetch('test_name'))
    @contract.fetch('runtime_tests', []).each do |policy_test|
      policy_output = command(policy_test.fetch('test_name'), 'cargo', 'test', '--locked', '-p', policy_test.fetch('package', 'codemap'), '--test', policy_test.fetch('test_target'), '--', '--ignored', '--color', 'never', '--exact', policy_test.fetch('test_name'), chdir: @root, env: env)
      self.class.verify_oracle(policy_output.out, policy_test.fetch('test_name'))
    end
    binary = File.join(@root, 'target/release/stele')
    templates = nil
    snapshots = Dir.mktmpdir('stele-public-corpus') do |scratch|
      %w[a b].each_with_index.map do |name, index|
        checkout = File.join(scratch, name)
        command("clone-#{name}", 'git', 'clone', '--quiet', '--no-local', '--depth', '1', '--single-branch', '--no-tags', '--no-checkout', repo, checkout)
        command("checkout-#{name}", 'git', '-C', checkout, 'checkout', '--quiet', '--detach', head)
        command("origin-#{name}", 'git', '-C', checkout, 'remote', 'set-url', 'origin', @contract.fetch('repository'))
        if name == 'a'
          syntax = File.join(@out, 'syntax.json')
          command('syntax', binary, 'code', 'extract', '--path', checkout, '--language', @contract.fetch('language'), '--out', syntax)
          command('validate-syntax', binary, 'code', 'validate', '--file', syntax)
          self.class.verify_snapshot(JSON.parse(File.read(syntax)), @contract, semantic: false)
        end
        workers = @contract['definition_workers']
        extraction_env = workers ? env.merge('CODEMAP_DEFINITION_WORKERS' => workers.fetch(index).to_s) : env
        snapshot = File.join(@out, "semantic-#{name}.json")
        command("extract-#{name}", binary, 'code', 'extract', '--path', checkout, '--language', @contract.fetch('language'), '--calls=required', '--progress', '--out', snapshot, env: extraction_env)
        command("validate-#{name}", binary, 'code', 'validate', '--file', snapshot)
        bytes = File.binread(snapshot)
        self.class.verify_snapshot(JSON.parse(bytes), @contract)
        verify_application_oracle(JSON.parse(bytes)) if @contract['application_oracle']
        templates = verify_template_oracle(name, snapshot, JSON.parse(bytes), binary) if @contract['template_oracle']
        raise ArgumentError, 'extractor changed its input checkout' unless @runner.run('git', '-C', checkout, 'status', '--porcelain').out.empty?
        bytes
      end
    end
    raise ArgumentError, 'independent corpus checkouts produced different bytes' unless snapshots.uniq.length == 1
    counts = self.class.verify_snapshot(JSON.parse(snapshots.first), @contract)
    result = {
      'contract' => 'stele-public-corpus-result/v1', 'passed' => true,
      'language' => @contract.fetch('language'), 'repository' => @contract.fetch('repository'),
      'commit' => head, 'oracle_sha256' => oracle.fetch('sha256'),
      'tool_lock_sha256' => lock.fetch('sha256'), 'stele_sha256' => Digest::SHA256.file(binary).hexdigest,
      'snapshot_sha256' => Digest::SHA256.hexdigest(snapshots.first),
      'files_scanned' => counts.fetch('files_scanned'), 'calls' => counts.fetch('calls'),
      'scope' => 'synthetic labelled precision and forbidden edges; application identity, scale and two-checkout determinism'
    }
    if @contract['application_oracle']
      result['application_oracle_sha256'] = @contract.fetch('application_oracle').fetch('sha256')
      result['application_oracle'] = verify_application_oracle(JSON.parse(snapshots.first))
      result['scope'] = 'exact synthetic fixture; frozen selected production callers with explicit forbidden targets; explicit coverage exclusions; no application-wide precision or recall claim'
    end
    if @contract['template_oracle']
      result['template_oracle_sha256'] = @contract.fetch('template_oracle').fetch('sha256')
      result['template_oracle'] = templates
      result['scope'] = 'exact synthetic fixture; exact call sites of selected templates; selected required and forbidden policy relations; frozen template and policy census; no application-wide precision or recall claim'
    end
    File.write(File.join(@out, 'result.json'), JSON.pretty_generate(result) + "\n")
    result
  end

  private

  def run_framework(repo, head, bundle, cargo_home)
    raise ArgumentError, 'the syntax framework gate uses no --bundle or --cargo-home' if bundle || cargo_home
    oracle = JSON.parse(File.read(File.join(@root, @contract.fetch('oracle').fetch('path'))))
    framework_sources(repo, oracle)
    env = {'CARGO_NET_OFFLINE' => 'true', 'RUSTUP_TOOLCHAIN' => @contract.fetch('toolchain'), 'RUSTUP_NO_UPDATE_CHECK' => '1'}
    Dir.mkdir(@out, 0o700)
    command('build', 'cargo', 'build', '--locked', '--release', '-p', 'stele', chdir: @root, env: env)
    @contract.fetch('synthetic_tests').each do |test|
      observed = command(test.fetch('test_name'), 'cargo', 'test', '--locked', '-p', test.fetch('package'), '--test', test.fetch('test_target'), '--', '--exact', test.fetch('test_name'), '--color', 'never', chdir: @root, env: env)
      self.class.verify_oracle(observed.out, test.fetch('test_name'))
    end
    metadata = command('metadata', 'cargo', 'metadata', '--locked', '--offline', '--no-deps', '--format-version', '1', chdir: @root, env: env)
    binary = File.join(JSON.parse(metadata.out).fetch('target_directory'), 'release/stele')
    scored = []
    snapshots = []
    topologies = Dir.mktmpdir('stele-laravel-corpus') do |scratch|
      %w[a b].map do |name|
        checkout = File.join(scratch, name)
        command("clone-#{name}", 'git', 'clone', '--quiet', '--no-local', '--no-checkout', repo, checkout)
        command("checkout-#{name}", 'git', '-C', checkout, 'checkout', '--quiet', '--detach', head)
        command("origin-#{name}", 'git', '-C', checkout, 'remote', 'set-url', 'origin', @contract.fetch('repository'))
        framework_sources(checkout, oracle)
        snapshot_path = File.join(@out, "syntax-#{name}.json")
        command("extract-#{name}", binary, 'code', 'extract', '--path', checkout, '--language', 'php', '--out', snapshot_path, env: env)
        command("validate-#{name}", binary, 'code', 'validate', '--file', snapshot_path, env: env)
        bytes = File.binread(snapshot_path)
        snapshot = JSON.parse(bytes)
        self.class.verify_snapshot(snapshot, @contract, semantic: false)
        @contract.fetch('tool_versions', {}).each do |tool, version|
          raise ArgumentError, "Laravel #{tool} identity differs" unless snapshot.fetch('identity').fetch('tools').fetch(tool) == version
        end
        topology_path = File.join(@out, "topology-#{name}.json")
        command("topology-#{name}", binary, 'topology', '--snapshot', snapshot_path, '--out', topology_path, env: env)
        topology_bytes = File.binread(topology_path)
        reader_scope = JSON.parse(File.read(File.join(@root, @contract.fetch('reader_scope').fetch('path'))))
        scored << LaravelOracle.verify(snapshot, JSON.parse(topology_bytes), oracle, reader_scope: reader_scope)
        raise ArgumentError, 'extractor changed its input checkout' unless @runner.run('git', '-C', checkout, 'status', '--porcelain').out.empty?
        snapshots << bytes
        topology_bytes
      end
    end
    raise ArgumentError, 'independent Laravel checkouts produced different snapshots' unless snapshots.uniq.length == 1
    raise ArgumentError, 'independent Laravel checkouts produced different policies' unless topologies.uniq.length == 1 && scored.uniq.length == 1
    result = {
      'contract' => 'stele-public-corpus-result/v1', 'passed' => true, 'language' => 'php',
      'repository' => @contract.fetch('repository'), 'commit' => head,
      'oracle_sha256' => @contract.fetch('oracle').fetch('sha256'),
      'framework_profile_sha256' => @contract.fetch('framework_profile').fetch('sha256'),
      'reader_scope_sha256' => @contract.fetch('reader_scope').fetch('sha256'),
      'tool_lock_sha256' => @contract.fetch('tool_lock').fetch('sha256'),
      'stele_sha256' => Digest::SHA256.file(binary).hexdigest,
      'snapshot_sha256' => Digest::SHA256.hexdigest(snapshots.first),
      'topology_sha256' => Digest::SHA256.hexdigest(topologies.first), 'framework_oracle' => scored.first,
      'scope' => 'frozen selected Eloquent declarations and Blade references with forbidden targets and explicit omissions; no application-wide precision or recall claim'
    }
    File.write(File.join(@out, 'result.json'), JSON.pretty_generate(result) + "\n")
    result
  end

  def framework_sources(repo, oracle)
    framework = oracle.fetch('framework')
    {'composer.json' => framework.fetch('composer_json_sha256'), 'composer.lock' => framework.fetch('composer_lock_sha256'),
     oracle.fetch('license_file').fetch('path') => oracle.fetch('license_file').fetch('sha256')}.each do |path, digest|
      raise ArgumentError, "Laravel framework source pin differs: #{path}" unless Digest::SHA256.file(File.join(repo, path)).hexdigest == digest
    end
    locked = JSON.parse(File.read(File.join(repo, 'composer.lock'))).fetch('packages').select { |package| package.fetch('name') == 'laravel/framework' }
    raise ArgumentError, 'Laravel framework package pin differs' unless locked.length == 1 && locked.first.fetch('version') == framework.fetch('version') && locked.first.fetch('source') == framework.fetch('source')
  end

  def verify_application_oracle(snapshot)
    tools = snapshot.fetch('identity').fetch('tools')
    if @contract.fetch('language') == 'rust'
      policy = {'cargo_offline' => true, 'build_scripts_enabled' => false, 'proc_macros_enabled' => false, 'background_checks_enabled' => false}
      raise ArgumentError, 'Rust analyzer execution policy differs' unless tools.fetch('rust_analyzer_policy') == policy
      %w[rust_analyzer rustc cargo].each do |tool|
        prefix = "#{tool.tr('_', '-')} #{@contract.fetch('toolchain')} "
        raise ArgumentError, "Rust #{tool} version differs from the public pin" unless tools.fetch(tool).start_with?(prefix)
      end
    else
      verify_ruby_tools(tools)
    end
    pin = @contract.fetch('application_oracle')
    SelectedCallerOracle.verify(snapshot, JSON.parse(File.read(File.join(@root, pin.fetch('path')))), fact_source: @contract.fetch('semantic_fact_source', 'RustAnalyzer'))
  end

  def verify_ruby_tools(tools)
    raise ArgumentError, 'Ruby tool-bundle identity differs' unless tools.fetch('ruby_tool_bundle_sha256') == @contract.fetch('tool_lock').fetch('sha256')
    @contract.fetch('tool_versions').each do |tool, version|
      raise ArgumentError, "Ruby #{tool} version differs from the public pin" unless tool == 'ruby' ? tools.fetch(tool).start_with?(version) : tools.fetch(tool) == version
    end
  end

  # The policy relations are what the shipped binary derives: the gate asks
  # `stele topology` for them rather than re-deriving anything itself.
  def verify_template_oracle(name, snapshot_path, snapshot, binary)
    verify_ruby_tools(snapshot.fetch('identity').fetch('tools'))
    topology = File.join(@out, "topology-#{name}.json")
    command("topology-#{name}", binary, 'topology', '--snapshot', snapshot_path, '--out', topology)
    pin = @contract.fetch('template_oracle')
    TemplateOracle.verify(snapshot, JSON.parse(File.read(topology)), JSON.parse(File.read(File.join(@root, pin.fetch('path')))))
  end

  def command(name, *args, **options)
    warn "public-corpus-benchmark: #{name}"
    result = @runner.run!(*args, **options) { |_out, err| $stderr.write(err.to_s) }
    log = File.join(@out, "#{name}.log")
    File.open(log, 'w', 0o600) { |file| file.write(result.out + result.err) }
    raise ArgumentError, "#{name} failed; see #{log}" unless result.success?
    result
  end
end
