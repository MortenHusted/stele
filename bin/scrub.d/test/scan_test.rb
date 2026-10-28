# frozen_string_literal: true

require 'minitest/autorun'
require 'tmpdir'
require 'json'
require 'rbconfig'
require 'tty-command'
require_relative '../scan'

class IdentifierScrubTest < Minitest::Test
  def setup
    @dir = Dir.mktmpdir('scrub-synthetic')
    @repo = File.join(@dir, 'repo')
    @runner = TTY::Command.new(printer: :null)
    @runner.run('git', 'init', '-q', @repo)
    git('config', 'user.name', 'Synthetic')
    git('config', 'user.email', 'synthetic@example.invalid')
    write('source.txt', 'ordinary content')
    commit
    @list = File.join(@dir, 'deny.txt')
    File.write(@list, "SyntheticPrivateName\n")
  end

  def teardown
    FileUtils.remove_entry(@dir)
  end

  def git(*args)
    @runner.run('git', '-C', @repo, *args).out
  end

  def write(path, bytes)
    File.write(File.join(@repo, path), bytes)
  end

  def commit
    git('add', '.')
    git('commit', '-qm', 'synthetic fixture')
  end

  def scan(**options)
    IdentifierScrub.new(repo: @repo, deny_list: @list, **options).run
  end

  def test_whole_case_sensitive_tokens_without_prefix_matches
    write('source.txt', 'SyntheticPrivateNameSuffix prefixSyntheticPrivateName syntheticprivatename')
    assert_empty scan.fetch('matches')
    write('source.txt', "SyntheticPrivateName\x00SyntheticPrivateName\xff".b)
    result = scan
    assert_equal ['SyntheticPrivateName'], result.fetch('matched_entries')
    assert_equal 1, result.dig('sources', 'tree', 'matches')
    assert_equal 0, result.dig('sources', 'history', 'matches')
  end

  def test_scans_untracked_paths_symlink_text_and_removed_history_on_other_refs
    git('checkout', '-qb', 'synthetic-other-ref')
    write('removed.txt', 'SyntheticPrivateName')
    commit
    git('checkout', '-')
    write('SyntheticPrivateName.txt', 'ordinary')
    File.symlink('SyntheticPrivateName', File.join(@repo, 'alias'))
    result = scan
    assert_equal 2, result.dig('sources', 'tree', 'matches')
    assert_operator result.dig('sources', 'history', 'matches'), :>=, 1
    assert result.fetch('matches').any? { |match| match['location'].start_with?('git:') }
  end

  def test_scans_explicit_unpacked_artifacts_and_refuses_compressed_contents
    artifact = File.join(@dir, 'unpacked')
    Dir.mkdir(artifact)
    File.binwrite(File.join(artifact, 'binary'), "\x00SyntheticPrivateName\xff".b)
    result = scan(artifacts: [artifact])
    assert_equal 1, result.dig('sources', 'artifacts', 'matches')
    File.binwrite(File.join(artifact, 'packed.gz'), "\x1f\x8bdata".b)
    assert_raises(ArgumentError) { scan(artifacts: [artifact]) }
  end

  def test_artifact_root_names_are_not_content_or_report_locations
    File.write(@list, "SyntheticPrivateName\nartifact\n")
    artifact = File.join(@dir, 'SyntheticPrivateName')
    Dir.mkdir(artifact)
    File.write(File.join(artifact, 'public.txt'), 'ordinary content')
    assert_empty scan(artifacts: [artifact]).fetch('matched_entries')
    File.write(File.join(artifact, 'public.txt'), 'SyntheticPrivateName')
    matches = scan(artifacts: [artifact]).fetch('matches')
    assert_equal ['artifact[0]/public.txt'], matches.map { |match| match['location'] }
  end

  def test_private_inputs_and_reports_cannot_live_in_git_or_scanned_artifacts
    inside = File.join(@repo, 'deny.txt')
    File.write(inside, "SyntheticPrivateName\n")
    assert_raises(ArgumentError) { IdentifierScrub.new(repo: @repo, deny_list: inside) }
    alias_path = File.join(@dir, 'list-alias')
    File.symlink(inside, alias_path)
    assert_raises(ArgumentError) { IdentifierScrub.new(repo: @repo, deny_list: alias_path) }
    assert_raises(ArgumentError) { scan(report: File.join(@repo, 'report.json')) }
    artifact = File.join(@dir, 'artifacts')
    Dir.mkdir(artifact)
    assert_raises(ArgumentError) { scan(artifacts: [artifact], report: File.join(artifact, 'report.json')) }
    assert_raises(ArgumentError) { scan(report: @list) }
    assert_equal "SyntheticPrivateName\n", File.read(@list)
  end

  def test_empty_invalid_lists_and_shallow_history_cannot_report_clean
    File.write(@list, "\n")
    assert_raises(ArgumentError) { scan }
    File.write(@list, "invalid token\n")
    assert_raises(ArgumentError) { scan }
    File.write(@list, "SyntheticPrivateName\n")
    shallow = File.join(@dir, 'shallow')
    @runner.run('git', 'clone', '--quiet', '--depth', '1', "file://#{@repo}", shallow)
    assert_raises(ArgumentError) { IdentifierScrub.new(repo: shallow, deny_list: @list).run }
  end

  def test_large_list_uses_the_same_exact_token_semantics
    File.write(@list, (0...100_000).map { |index| "SyntheticToken#{index}" }.join("\n"))
    write('source.txt', 'SyntheticToken99999 SyntheticToken99999Suffix')
    assert_equal ['SyntheticToken99999'], scan.fetch('matched_entries')
  end

  def test_cli_keeps_identifiers_off_console_and_writes_new_private_report
    write('source.txt', 'SyntheticPrivateName')
    report = File.join(@dir, 'report.json')
    command = File.expand_path('../../scrub', __dir__)
    result = @runner.run!(RbConfig.ruby, command, '--repo', @repo, '--deny-list', @list, '--report', report)
    assert_equal 1, result.exit_status
    refute_includes result.out + result.err, 'SyntheticPrivateName'
    assert_equal 1, JSON.parse(result.out).dig('sources', 'tree', 'matches')
    assert_equal ['SyntheticPrivateName'], JSON.parse(File.read(report)).fetch('matched_entries')
    assert_equal 0, File.stat(report).mode & 0o077
    assert_raises(ArgumentError) { scan(report: report) }
    help = @runner.run(RbConfig.ruby, command, '--help')
    assert_includes help.out, '[A-Za-z0-9_]+'
    clean = @runner.run!(RbConfig.ruby, command, '--repo', @repo, '--deny-list', @list)
    assert_equal 1, clean.exit_status
    write('source.txt', 'ordinary')
    assert_equal 0, @runner.run!(RbConfig.ruby, command, '--repo', @repo, '--deny-list', @list).exit_status
  end

  def test_invalid_cli_options_cannot_report_a_clean_scan
    command = File.expand_path('../../scrub', __dir__)
    result = @runner.run!(RbConfig.ruby, command, '--repo', @repo, '--deny-list', @list, '--unknown')
    assert_equal 2, result.exit_status
    assert_empty result.out
    assert_equal 2, @runner.run!(RbConfig.ruby, command).exit_status
  end
end
