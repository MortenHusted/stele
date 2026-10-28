# frozen_string_literal: true
require 'minitest/autorun'
require 'tmpdir'
require 'fileutils'
require 'open3'
require_relative '../review'

class ExportReviewTest < Minitest::Test
  def fixture
    Dir.mktmpdir do |scratch|
      root = File.realpath(scratch)
      repo = File.join(root, 'repo');Dir.mkdir(repo)
      git(repo, 'init', '--quiet')
      %w[README.md LICENSE NOTICE THIRD_PARTY_NOTICES.md].each { |name| File.write(File.join(repo, name), 'synthetic fixture notice') }
      git(repo, 'add', '.')
      git(repo, 'commit', '--quiet', '-m', 'clean fixture')
      yield root, repo
    end
  end

  def git(repo, *args)
    TTY::Command.new(printer: :null).run(*ScrubGitContext.command, 'git', '-c', 'core.hooksPath=/dev/null', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', '-C', repo, *args)
  end

  def test_inventory_covers_nested_sources_and_preserves_pending_review
    fixture do |_root, repo|
      path = File.join(repo, 'crates/lemmaspec/src/ast.rs');FileUtils.mkdir_p(File.dirname(path));File.write(path, 'fn example() {}')
      File.symlink('README.md', File.join(repo, 'CLAUDE.md'))
      git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'nested fixture')
      report = ExportReview.new.inventory(repo, [])
      assert_equal 6, report.fetch('files').length
      entry = report.fetch('files').find { |file| file.fetch('path') == 'crates/lemmaspec/src/ast.rs' }
      assert_equal 'lemmalog', entry.fetch('source')
      assert_equal 'pending_owner_review', entry.fetch('review_status')
      assert_equal '120000', report.fetch('files').find { |file| file['path'] == 'CLAUDE.md' }.fetch('mode')
    end
  end

  def test_history_ref_changes_are_part_of_the_input_identity
    fixture do |_root, repo|
      before = ExportReview.new.inventory(repo, [])
      git(repo, 'tag', 'extra-history-ref')
      after = ExportReview.new.inventory(repo, [])
      assert_equal before.fetch('head'), after.fetch('head')
      assert_equal before.fetch('files'), after.fetch('files')
      refute_equal before.fetch('refs'), after.fetch('refs')
    end
  end

  def test_dirty_input_and_missing_notice_refuse_inventory
    fixture do |_root, repo|
      File.write(File.join(repo, 'README.md'), 'changed')
      assert_raises(ArgumentError) { ExportReview.new.inventory(repo, []) }
      git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'changed')
      File.unlink(File.join(repo, 'NOTICE'));git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'missing notice')
      assert_raises(ArgumentError) { ExportReview.new.inventory(repo, []) }
    end
  end

  def test_committed_notice_drift_cannot_claim_the_upstream_pin
    fixture do |_root, repo|
      path = 'publication/third-party/tree-sitter-cpp.LICENSE'
      physical = File.join(repo, path);FileUtils.mkdir_p(File.dirname(physical))
      FileUtils.cp(File.expand_path('../../../' + path, __dir__), physical)
      git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'upstream notice')
      ExportReview.new.inventory(repo, [])
      File.write(physical, 'changed notice')
      git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'notice mutation')
      assert_raises(ArgumentError) { ExportReview.new.inventory(repo, []) }
    end
  end

  def test_capture_refuses_content_drift_after_inventory
    fixture do |root, repo|
      review = ExportReview.new
      inventory = review.inventory(repo, [])
      File.write(File.join(repo, 'README.md'), 'changed after inventory')
      assert_raises(ArgumentError) { review.send(:capture, repo, inventory.fetch('files'), File.join(root, 'capture')) }
    end
  end

  def test_source_and_artifact_links_cannot_leave_shipped_inputs
    fixture do |root, repo|
      outside = File.join(root, 'outside');File.write(outside, 'private fixture')
      File.symlink(outside, File.join(repo, 'link'))
      git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'outside link')
      assert_raises(ArgumentError) { ExportReview.new.inventory(repo, []) }
      git(repo, 'rm', '--quiet', 'link');git(repo, 'commit', '--quiet', '-m', 'remove link')
      artifact = File.join(root, 'artifact');Dir.mkdir(artifact);File.symlink(outside, File.join(artifact, 'link'))
      assert_raises(ArgumentError) { ExportReview.new.inventory(repo, [artifact]) }
    end
  end

  def test_wrong_scanner_archive_and_existing_output_refuse_without_overwrite
    fixture do |root, repo|
      archive = File.join(root, 'archive');File.write(archive, 'not the pinned release')
      deny = File.join(root, 'deny');File.write(deny, "synthetic_private_identifier\n")
      out = File.join(root, 'new-review')
      assert_raises(ArgumentError) { ExportReview.new.run(repo: repo, deny_list: deny, scanner_archive: archive, out: out) }
      refute File.exist?(out)
      File.write(out, 'preserve')
      assert_raises(ArgumentError) { ExportReview.new.run(repo: repo, deny_list: deny, scanner_archive: archive, out: out) }
      assert_equal 'preserve', File.read(out)
    end
  end

  def test_cli_help_and_unknown_arguments
    command = File.expand_path('../../export-review', __dir__)
    out, err, status = Open3.capture3(RbConfig.ruby, command, '--unknown', '--help')
    assert_equal 2, status.exitstatus
    assert_empty out
    assert_includes err, 'Usage:'
    out, _err, status = Open3.capture3(RbConfig.ruby, command, '--help')
    assert status.success?
    assert_includes out, '--scanner-archive'
  end

  def test_pinned_native_scanner_detects_deleted_history_and_artifact_findings
    archive = ENV['STELE_GITLEAKS_ARCHIVE']
    skip 'set STELE_GITLEAKS_ARCHIVE to the pinned public host archive' unless archive
    fixture do |root, repo|
      token = 'ghp_' + Digest::SHA256.hexdigest('synthetic secret scanner canary')[0, 36]
      File.write(File.join(repo, '.gitleaks.toml'), "[extend]\nuseDefault = true\n[allowlist]\npaths = ['.*']\n")
      File.write(File.join(repo, 'README.md'), token + ' # gitleaks:allow')
      git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'synthetic canary')
      File.write(File.join(repo, 'README.md'), 'clean fixture')
      git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'remove canary')
      deny = File.join(root, 'deny');File.write(deny, "synthetic_private_identifier\n")
      artifact = File.join(root, 'payload');Dir.mkdir(artifact)
      File.write(File.join(artifact, 'plain'), "#{token}\nsynthetic_private_identifier\n")
      File.binwrite(File.join(artifact, 'binary'), "\x7fELF\0#{token}".b)
      out = File.join(root, 'review')
      foreign = File.join(root, 'foreign');Dir.mkdir(foreign)
      git(foreign, 'init', '--quiet');File.write(File.join(foreign, 'a'), 'unrelated')
      git(foreign, 'add', '.');git(foreign, 'commit', '--quiet', '-m', 'unrelated repository')
      command = File.expand_path('../../export-review', __dir__)
      caller_env = {'GIT_DIR' => File.join(foreign, '.git'), 'GIT_WORK_TREE' => foreign}
      stdout, stderr, status = Open3.capture3(caller_env, RbConfig.ruby, command, '--repo', repo, '--deny-list', deny, '--scanner-archive', archive, '--out', out, artifact)
      assert_equal 1, status.exitstatus, stderr
      refute stdout.include?(token)
      refute stderr.include?('synthetic_private_identifier')
      result = JSON.parse(File.read(File.join(out, 'result.json')))
      refute result.fetch('scans_clean')
      assert_equal git(repo, 'rev-parse', 'HEAD').out.strip, JSON.parse(File.read(File.join(out, 'identifiers.json'))).fetch('head')
      assert_equal 0, result.fetch('secret_findings').fetch('tree')
      assert_operator result.fetch('secret_findings').fetch('history'), :>, 0
      assert_operator result.fetch('secret_findings').fetch('artifact-0'), :>=, 2
      assert_operator result.fetch('identifier_findings'), :>, 0
      assert_equal 'pending_owner_review', result.fetch('provenance_review')
      refute File.read(File.join(out, 'secrets-history.json')).include?(token)
    end
  end

  def test_pinned_native_clean_scan_does_not_approve_provenance
    archive = ENV['STELE_GITLEAKS_ARCHIVE']
    skip 'set STELE_GITLEAKS_ARCHIVE to the pinned public host archive' unless archive
    fixture do |root, repo|
      deny = File.join(root, 'deny');File.write(deny, "synthetic_private_identifier\n")
      out = File.join(root, 'review')
      result = ExportReview.new.run(repo: repo, deny_list: deny, scanner_archive: archive, out: out)
      assert result.fetch('scans_clean')
      assert_equal 'pending_owner_review', result.fetch('provenance_review')
      assert_equal 0o600, File.stat(File.join(out, 'inventory.json')).mode & 0o777
      assert_equal 0o700, File.stat(out).mode & 0o777
      assert File.file?(File.join(out, 'gitleaks.LICENSE'))
      assert_equal 0o600, File.stat(File.join(out, 'secrets-tree.json')).mode & 0o777
      assert_equal 0o700, File.stat(File.join(out, 'tree/content')).mode & 0o777
    end
  end
  def native_archive
    ENV['STELE_GITLEAKS_ARCHIVE'] || skip('set STELE_GITLEAKS_ARCHIVE to the pinned public host archive')
  end

  def canary
    'ghp_' + Digest::SHA256.hexdigest('synthetic secret scanner canary')[0, 36]
  end

  def native_run(root, repo, archive, name, artifacts: [])
    deny = File.join(root, 'deny');File.write(deny, "synthetic_private_identifier\n")
    out = File.join(root, name)
    [ExportReview.new.run(repo: repo, deny_list: deny, scanner_archive: archive, artifacts: artifacts, out: out), out]
  end

  def test_native_binary_attributes_cannot_silence_history
    archive = native_archive
    fixture do |root, repo|
      File.write(File.join(repo, '.gitattributes'), "*.md binary\n")
      git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'binary attributes')
      File.write(File.join(repo, 'README.md'), canary)
      git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'canary')
      File.write(File.join(repo, 'README.md'), 'removed')
      git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'remove canary')
      result, = native_run(root, repo, archive, 'attributes-review')
      assert_operator result.fetch('secret_findings').fetch('history'), :>, 0
    end
  end

  def test_native_root_ignore_files_cannot_silence_tree_history_or_artifacts
    archive = native_archive
    fixture do |root, repo|
      File.write(File.join(repo, 'README.md'), canary)
      git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'canary')
      artifact = File.join(root, 'payload');Dir.mkdir(artifact);File.write(File.join(artifact, 'plain'), canary)
      baseline, first = native_run(root, repo, archive, 'baseline', artifacts: [artifact])
      assert_operator baseline.fetch('secret_findings').fetch('tree'), :>, 0
      second = File.join(root, 'with-ignores')
      fingerprints = %w[tree history].flat_map do |section|
        JSON.parse(File.read(File.join(first, 'secrets-' + section + '.json'))).map { |finding| finding.fetch('Fingerprint').gsub(first, second) }
      end
      File.write(File.join(repo, '.gitleaksignore'), fingerprints.join("\n") + "\n")
      git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'ignore fingerprints')
      artifact_fingerprints = JSON.parse(File.read(File.join(first, 'secrets-artifact-0.json'))).map { |finding| finding.fetch('Fingerprint').gsub(first, second) }
      File.write(File.join(artifact, '.gitleaksignore'), artifact_fingerprints.join("\n") + "\n")
      result, = native_run(root, repo, archive, 'with-ignores', artifacts: [artifact])
      %w[tree history artifact-0].each { |section| assert_operator result.fetch('secret_findings').fetch(section), :>, 0 }
    end
  end

  def test_native_builtin_path_exclusions_are_visible_for_manual_review
    archive = native_archive
    fixture do |root, repo|
      artifact = File.join(root, 'payload');Dir.mkdir(artifact);File.write(File.join(artifact, 'payload.bin'), canary)
      result, out = native_run(root, repo, archive, 'allowlist-review', artifacts: [artifact])
      assert_equal 0, result.fetch('secret_findings').fetch('artifact-0')
      assert_equal 1, result.fetch('manual_secret_review_count')
      entry = JSON.parse(File.read(File.join(out, 'inventory.json'))).fetch('artifacts').first.fetch('files').first
      assert entry.fetch('default_allowlist_path_match')
      assert_equal 'pending_owner_review', result.fetch('provenance_review')
    end
  end

end
