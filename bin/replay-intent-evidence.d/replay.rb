# frozen_string_literal: true
require 'digest'
require 'fileutils'
require 'json'
require 'rbconfig'
require 'tempfile'
require 'rubygems/package'
require 'zlib'
require 'tty-command'
require_relative '../prepare-public-export.d/export'

class IntentReplay
  FIXTURE = 'publication/intent-mutations.json'
  LEDGER = 'intent/runs.stele'

  def self.cases(bytes)
    bundle = JSON.parse(bytes)
    raise ArgumentError, 'expected a nonempty intent mutation fixture' unless bundle.is_a?(Hash) && bundle.keys.sort == %w[cases contract] && bundle['contract'] == 'stele-intent-mutations/v1' && bundle['cases'].is_a?(Array) && !bundle['cases'].empty?
    cases = bundle.fetch('cases').map do |entry|
      raise ArgumentError, 'mutation fields or identifiers differ' unless entry.is_a?(Hash) && entry.keys.sort == %w[channel example patch] && %w[example channel].all? { |key| entry[key].is_a?(String) && /\A[a-z][a-z0-9_]*\z/.match?(entry[key]) } && entry['patch'].is_a?(String)
      patch = entry.fetch('patch')
      raise ArgumentError, 'mutation includes historical metadata or non-text operations' if patch.match?(/^(?:index |new file mode |deleted file mode |old mode |new mode |rename |copy |GIT binary patch|Binary files)/)
      paths = patch.lines.grep(/^diff --git /).map do |line|
        match = line.match(%r{\Adiff --git a/(crates/[a-z0-9_-]+/src/[a-z0-9_./-]+\.rs) b/\1\n?\z})
        raise ArgumentError, 'mutations may change only existing owned Rust source, never tests or metadata' unless match && match[1].split('/').none? { |part| %w[. .. .git].include?(part) }
        path = match[1]
        raise ArgumentError, 'mutation file headers differ' unless patch.include?("--- a/#{path}\n+++ b/#{path}\n")
        path
      end
      raise ArgumentError, 'mutation needs source hunks' if paths.empty? || paths.uniq != paths || !patch.match?(/^@@ /)
      raise ArgumentError, 'mutation has unreported file headers' unless patch.lines.grep(/^--- /).map(&:strip) == paths.map { |path| '--- a/' + path } && patch.lines.grep(/^\+\+\+ /).map(&:strip) == paths.map { |path| '+++ b/' + path }
      entry.merge('paths' => paths)
    end
    raise ArgumentError, 'duplicate mutation example/channel' unless cases.map { |entry| entry.values_at('example', 'channel') }.uniq.length == cases.length
    cases
  rescue JSON::ParserError
    raise ArgumentError, 'invalid mutation fixture JSON'
  end

  def self.runs(text)
    blocks = text.scan(/^run \{\n(.*?)^\}\n?/m).flatten
    raise ArgumentError, 'unparsed run ledger content' unless text.gsub(/^run \{\n(.*?)^\}\n?/m, '').strip.empty?
    blocks.map do |body|
      fields = {}
      body.lines.each do |line|
        match = line.match(/\A  ([a-z_]+): (.*?)\n?\z/)
        raise ArgumentError, 'invalid or repeated run field' unless match && !fields.key?(match[1])
        key, value = match.captures
        fields[key] = if %w[example channel outcome].include?(key) then value
                      elsif key == 'digest_version' then Integer(value, 10)
                      else JSON.parse(value)
                      end
      end
      expected = %w[example digest digest_version channel source base outcome]
      expected << 'diff' if fields['outcome'] == 'fail'
      raise ArgumentError, 'run fields differ from recorder contract' unless fields.keys.sort == expected.sort && %w[pass fail error skip].include?(fields['outcome'])
      fields
    end
  rescue JSON::ParserError
    raise ArgumentError, 'invalid recorder ledger field'
  end

  def initialize
    @runner = TTY::Command.new(printer: :null)
    @rust_env = {}
  end

  def run(repo:, out:, deny_list:, scanner_archive:, artifacts: [], single_commit: false)
    @repo = File.realpath(repo)
    root = git('rev-parse', 'HEAD').out.strip
    raise ArgumentError, 'replay requires a clean fresh single-root main candidate' unless File.realpath(git('rev-parse', '--show-toplevel').out.strip) == @repo && git('status', '--porcelain', '--untracked-files=all').out.empty? && git('rev-list', '--all', '--parents').out.strip == root && git('for-each-ref', '--format=%(refname)').out.strip == 'refs/heads/main' && git('symbolic-ref', '--short', 'HEAD').out.strip == 'main'
    raise ArgumentError, 'inherited run evidence must be omitted from the source root' if File.exist?(File.join(@repo, LEDGER)) || git('cat-file', '-e', root + ':' + LEDGER, check: false).success?
    @out = File.join(File.realpath(File.dirname(out)), File.basename(out))
    raise ArgumentError, 'receipt must be new and outside Git and candidate inputs' if File.exist?(@out) || File.symlink?(@out) || git_at(File.dirname(@out), 'rev-parse', '--show-toplevel', check: false).success? || [@repo, *artifacts.map { |path| File.realpath(path) }].any? { |path| @out == path || @out.start_with?(path + '/') || path.start_with?(@out + '/') }
    fixture = git('show', root + ':' + FIXTURE).out.b
    cases = self.class.cases(fixture)
    cases.each do |entry|
      entry.fetch('paths').each do |path|
        raise ArgumentError, 'mutation target must be a tracked regular source file' unless git('ls-tree', root, '--', path).out.match?(/\A100(?:644|755) blob /)
      end
      Tempfile.create('stele-mutation-check') do |file|
        file.write(entry.fetch('patch'));file.flush;git('apply', '--check', '--', file.path)
      end
    end
    Dir.mkdir(@out, 0o700)
    channel = File.read(File.join(@repo, 'rust-toolchain.toml'))[/^channel\s*=\s*"([^"]+)"$/, 1]
    raise ArgumentError, 'prepare the declared Rust toolchain' unless channel
    @rust_env = {'RUSTUP_TOOLCHAIN' => channel, 'CARGO_NET_OFFLINE' => 'true'}
    recorder, build = build_recorder(root)
    baseline = capture('root-intent-report.txt', recorder, 'intent', 'report').out
    source = source_identity(baseline, root)
    recording = single_commit ? ['--source-baseline', source] : []
    base = single_commit ? 'source:' + source : root
    test_env = {'CARGO_TARGET_DIR' => File.join(@out, 'test-build')}
    cases.each do |entry|
      label = entry.fetch('example') + '-' + entry.fetch('channel')
      patch = save(label + '.patch', entry.fetch('patch'))
      begin
        git('apply', '--check', '--', patch);git('apply', '--', patch)
        observed = capture(label + '.txt', recorder, 'intent', 'record', *recording, '--example', entry.fetch('example'), '--channel', entry.fetch('channel'), env: test_env)
        raise ArgumentError, 'a deliberate break did not produce its own test failure' unless observed.out.strip == entry.fetch('example') + ' on ' + entry.fetch('channel') + ': fail'
      ensure
        git('restore', '--source', root, '--worktree', '--', *entry.fetch('paths'))
      end
      raise ArgumentError, 'source changed outside the selected mutation' unless git('diff', '--name-only', root, '--', '.', ':(exclude)' + LEDGER).out.empty?
    end
    green = capture('passing-runs.txt', recorder, 'intent', 'record', *recording, env: test_env)
    expected = cases.map { |entry| entry.fetch('example') + ' on ' + entry.fetch('channel') + ': pass' }.sort
    raise ArgumentError, 'passing runs do not cover exactly the mutation examples' unless green.out.lines.map(&:strip).sort == expected
    recorded = self.class.runs(File.read(File.join(@repo, LEDGER)))
    raise ArgumentError, 'unexpected run count or non-public base' unless recorded.length == 2 * cases.length && recorded.all? { |run| run['base'] == base && run['digest_version'] == 1 }
    cases.each do |entry|
      pair = recorded.select { |run| run.values_at('example', 'channel') == entry.values_at('example', 'channel') }
      red = pair.find { |run| run['outcome'] == 'fail' };pass = pair.find { |run| run['outcome'] == 'pass' }
      raise ArgumentError, 'missing matching real fail/pass evidence at the clean source' unless pair.length == 2 && red && pass && red['digest'] == pass['digest'] && pass['source'] == source && red['source'] != source
      Tempfile.create('stele-recorded-mutation') do |file|
        file.write(red.fetch('diff'));file.flush;git('apply', '--check', '--', file.path)
      end
      diff_paths = red.fetch('diff').scan(/^diff --git a\/(\S+) b\/\1$/).flatten
      raise ArgumentError, 'recorded diff changes other paths' unless diff_paths.sort == entry.fetch('paths').sort
      red.fetch('diff').scan(/^diff --git a\/(\S+) b\/\1\nindex ([0-9a-f]+)\.\.[0-9a-f]+/).each do |path, before|
        raise ArgumentError, 'recorded index metadata does not belong to the public root' unless git('rev-parse', root + ':' + path).out.strip.start_with?(before)
      end
      verify_observed_source(root, entry, red, recorder) if single_commit
    end
    commit = commit_ledger(root, single_commit: single_commit)
    final_report = capture('intent-report.txt', recorder, 'intent', 'report').out
    raise ArgumentError, 'ledger commit changed the source identity' unless source_identity(final_report, commit) == source
    standings = final_report.lines.grep(/^### `/)
    raise ArgumentError, 'catalog is not fully verified' if standings.empty? || standings.any? { |line| !line.rstrip.end_with?(': verified') }
    capture('plan-check.txt', recorder, 'plan', 'check', 'plans/stele-plans.stele')
    payloads = [File.join(@out, 'recorder-payload')]
    if single_commit
      _release, release_build = build_recorder(commit, label: 'public')
      payloads << File.join(@out, 'public-payload')
    end
    review = ExportReview.new.run(repo: @repo, deny_list: deny_list, scanner_archive: scanner_archive, out: File.join(@out, 'review'), artifacts: [*payloads, *artifacts])
    result = {'contract' => single_commit ? 'stele-intent-replay/v2' : 'stele-intent-replay/v1', 'source_root' => root, 'evidence_commit' => commit, 'source_identity' => source, 'mutation_sha256' => Digest::SHA256.hexdigest(fixture), 'recorder' => build, 'ledger_sha256' => Digest::SHA256.file(File.join(@repo, LEDGER)).hexdigest, 'failing_runs' => cases.length, 'passing_runs' => cases.length, 'verified_statements' => standings.length, 'scans_clean' => review.fetch('scans_clean'), 'publication_status' => 'pending_owner_review'}
    result.merge!('baseline_kind' => 'source', 'release' => release_build, 'temporary_recording_commit' => root) if single_commit
    save('result.json', JSON.pretty_generate(result) + "\n")
    result
  end

  private

  def verify_observed_source(root, entry, red, recorder)
    Tempfile.create('stele-observed-source') do |file|
      file.write(red.fetch('diff'));file.flush
      begin
        git('apply', '--check', '--', file.path);git('apply', '--', file.path)
        report = capture(entry.fetch('example') + '-reproduced-source.txt', recorder, 'intent', 'report').out
        raise ArgumentError, 'observed failure patch does not reproduce its recorded source identity' unless report[/Source identity `([^`]+)`/, 1] == red.fetch('source')
      ensure
        git('restore', '--source', root, '--worktree', '--', *entry.fetch('paths'))
      end
    end
  end

  def build_recorder(root, label: 'recorder')
    os = RbConfig::CONFIG['host_os'].include?('darwin') ? 'apple-darwin' : 'unknown-linux-gnu'
    cpu = %w[arm64 aarch64].include?(RbConfig::CONFIG['host_cpu']) ? 'aarch64' : 'x86_64'
    target = cpu + '-' + os
    version = File.read(File.join(@repo, 'Cargo.toml')).split('[workspace.package]', 2).fetch(1)[/^version = "([^"]+)"$/, 1]
    build = capture(label + '-build.txt', RbConfig.ruby, File.join(@repo, 'bin/package-release'), '--version', version, '--target', target, '--dist-dir', File.join(@out, label + '-release'), env: {'CARGO_TARGET_DIR' => File.join(@out, 'recorder-build')})
    package = JSON.parse(build.out)
    archive = File.realpath(package.fetch('archive'))
    raise ArgumentError, 'recorder package leaves the receipt or differs from its checksum' unless archive.start_with?(@out + '/') && Digest::SHA256.file(archive).hexdigest == package.fetch('sha256')
    payload = File.join(@out, label + '-payload');Dir.mkdir(payload, 0o700)
    prefix = 'stele-' + version + '-' + target + '/'
    Zlib::GzipReader.open(archive) do |gzip|
      Gem::Package::TarReader.new(gzip) do |tar|
        tar.each do |entry|
          next if entry.directory?
          name = entry.full_name.delete_prefix(prefix)
          raise ArgumentError, 'recorder package member is not a regular relative file' unless entry.file? && entry.full_name.start_with?(prefix) && name.split('/', -1).none? { |part| ['', '.', '..'].include?(part) || part.downcase == '.git' } && !name.match?(/[\\\0]/)
          path = File.join(payload, name);FileUtils.mkdir_p(File.dirname(path), mode: 0o700)
          File.open(path, File::WRONLY | File::CREAT | File::EXCL, name == 'stele' ? 0o700 : 0o600) { |file| file.write(entry.read) }
        end
      end
    end
    receipt = JSON.parse(File.read(File.join(payload, 'BUILD.json')))
    bytes = File.binread(File.join(payload, 'stele'))
    raise ArgumentError, 'recorder was not built from the public root' unless receipt.fetch('source_commit') == root && receipt.fetch('binary_sha256') == Digest::SHA256.hexdigest(bytes)
    inventory = JSON.parse(File.read(File.join(payload, 'THIRD_PARTY_INVENTORY.json')))
    recorder = save('stele-' + label, bytes);File.chmod(0o700, recorder)
    raise ArgumentError, 'recorder version differs' unless command(recorder, '--version').out.strip == 'stele ' + version
    [recorder, {'source_commit' => root, 'binary_sha256' => receipt.fetch('binary_sha256'), 'archive_sha256' => package.fetch('sha256'), 'target' => target, 'rust_toolchain' => @rust_env.fetch('RUSTUP_TOOLCHAIN'), 'compiler' => inventory.fetch('rust_standard_library').fetch('compiler')}]
  end

  def commit_ledger(root, single_commit: false)
    oid = git('hash-object', '-w', '--no-filters', '--', LEDGER).out.strip
    git('update-index', '--add', '--cacheinfo', '100644,' + oid + ',' + LEDGER)
    tree = git('write-tree').out.strip
    raise ArgumentError, 'evidence commit must change only the run ledger' unless git('diff', '--name-only', '--no-renames', root, tree).out.lines.map(&:strip) == [LEDGER]
    before = git('ls-tree', '-rz', '--name-only', root).out.split("\0")
    after = git('ls-tree', '-rz', '--name-only', tree).out.split("\0")
    raise ArgumentError, 'a source-root history path disappeared' unless (before - after).empty?
    date = git('show', '-s', '--format=%ct', root).out.strip + ' +0000'
    identity = CuratedExport::IDENTITY.merge('GIT_AUTHOR_DATE' => date, 'GIT_COMMITTER_DATE' => date)
    parents = single_commit ? [] : ['-p', root]
    message = single_commit ? 'Publish reviewed source and reproducible intent evidence' : 'Record reproducible intent evidence'
    commit = git('-c', 'commit.gpgsign=false', 'commit-tree', tree, *parents, '-m', message, env: identity).out.strip
    git('-c', 'core.logAllRefUpdates=false', 'update-ref', 'refs/heads/main', commit, root)
    history = single_commit ? [commit] : [commit + ' ' + root, root]
    raise ArgumentError, 'evidence history differs from the selected cutover shape' unless git('rev-list', '--all', '--parents').out.lines.map(&:strip) == history && git('status', '--porcelain', '--untracked-files=all').out.empty?
    if single_commit
      git('reflog', 'expire', '--expire=now', '--all')
      git('gc', '--prune=now')
      all = git('cat-file', '--batch-all-objects', '--batch-check=%(objectname)').out.lines.map(&:strip).sort
      reachable = git('rev-list', '--objects', '--all').out.lines.map { |line| line.split.first }.sort
      raise ArgumentError, 'unreachable objects remain in single-commit publication storage' unless all == reachable
    end
    commit
  end

  def source_identity(report, commit)
    source = report[/Source identity `([^`]+)`/, 1]
    raise ArgumentError, 'report does not match the clean candidate commit' unless source && report.include?('matches the tree of HEAD commit `' + commit + '`')
    source
  end

  def save(name, bytes)
    path = File.join(@out, name)
    File.open(path, File::WRONLY | File::CREAT | File::EXCL, 0o600) { |file| file.write(bytes) }
    path
  end

  def capture(name, *args, env: {})
    result = command(*args, env: env, check: false)
    save(name, result.out + result.err)
    raise ArgumentError, 'replay command failed; inspect the private receipt' unless result.success?
    result
  end

  def command(*args, env: {}, check: true)
    result = @runner.run!(*ScrubGitContext.command, 'GIT_NO_REPLACE_OBJECTS=1', *@rust_env.merge(env).map { |key, value| key + '=' + value }, *args, chdir: @repo, binmode: true)
    raise ArgumentError, 'replay operation failed; no approved evidence' if check && !result.success?
    result
  end

  def git(*args, **options)
    command('git', '-C', @repo, *args, **options)
  end

  def git_at(repo, *args, **options)
    command('git', '-C', repo, *args, **options)
  end
end
