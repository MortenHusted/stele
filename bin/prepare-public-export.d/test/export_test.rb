# frozen_string_literal: true
require 'minitest/autorun'
require 'tmpdir'
require 'fileutils'
require 'open3'
require_relative '../export'

class CuratedExportTest < Minitest::Test
  def git(repo, *args, check: true)
    TTY::Command.new(printer: :null).public_send(check ? :run : :run!, *ScrubGitContext.command, 'git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false', '-C', repo, *args)
  end

  def fixture
    Dir.mktmpdir do |scratch|
      root = File.realpath(scratch);repo = File.join(root, 'source');Dir.mkdir(repo)
      git(repo, 'init', '--quiet', '--template=')
      FileUtils.mkdir_p(File.join(repo, 'terms'));FileUtils.mkdir_p(File.join(repo, 'publication'));FileUtils.mkdir_p(File.join(repo, 'bin'))
      {'README.md' => 'public synthetic content', 'THIRD_PARTY_NOTICES.md' => 'synthetic attribution', 'terms/license.txt' => 'SYNTHETIC TEST LICENSE; NOT A GRANT', 'terms/notice.txt' => 'synthetic notice', 'private.txt' => 'synthetic_private_source_marker', 'bin/run' => 'synthetic executable'}.each { |path, bytes| File.write(File.join(repo, path), bytes) }
      File.chmod(0o755, File.join(repo, 'bin/run'));File.symlink('README.md', File.join(repo, 'README.link'))
      files = %w[README.md README.link THIRD_PARTY_NOTICES.md bin/run]
      File.write(File.join(repo, 'publication/files.json'), JSON.generate('files' => files))
      git(repo, 'add', '.');git(repo, 'commit', '--quiet', '-m', 'PRIVATE_SOURCE_HISTORY')
      File.write(File.join(root, 'deny.txt'), "synthetic_private_source_marker\n")
      options = {repo: repo, source_commit: git(repo, 'rev-parse', 'HEAD').out.strip, include_list: 'publication/files.json', license: 'terms/license.txt', notice: 'terms/notice.txt', deny_list: File.join(root, 'deny.txt'), scanner_archive: ENV.fetch('STELE_GITLEAKS_ARCHIVE', File.join(root, 'missing-scanner')), out: File.join(root, 'export')}
      yield root, repo, options, files
    end
  end

  def commit_list(repo, options, files)
    File.write(File.join(repo, 'publication/files.json'), JSON.generate('files' => files))
    git(repo, 'add', 'publication/files.json');git(repo, 'commit', '--quiet', '-m', 'select fixture files')
    options[:source_commit] = git(repo, 'rev-parse', 'HEAD').out.strip
  end

  def native_scanner
    skip 'set STELE_GITLEAKS_ARCHIVE to the pinned public host archive' unless ENV['STELE_GITLEAKS_ARCHIVE']
  end

  def test_repeatable_root_uses_only_pinned_selected_blobs_and_explicit_terms
    native_scanner
    fixture do |root, repo, options, _files|
      File.write(File.join(repo, 'README.md'), 'WORKTREE_ONLY')
      first = CuratedExport.new.run(**options)
      options[:out] = File.join(root, 'repeat');second = CuratedExport.new.run(**options)
      assert first.fetch('scans_clean')
      assert_equal first.fetch('export_commit'), second.fetch('export_commit')
      assert_equal first.fetch('export_tree'), second.fetch('export_tree')
      destination = File.join(root, 'export/repo')
      assert_equal '1', git(destination, 'rev-list', '--all', '--count').out.strip
      assert_equal 'public synthetic content', File.read(File.join(destination, 'README.md'))
      assert_equal 'SYNTHETIC TEST LICENSE; NOT A GRANT', File.read(File.join(destination, 'LICENSE'))
      refute File.exist?(File.join(destination, 'private.txt'))
      assert File.symlink?(File.join(destination, 'README.link'))
      assert File.executable?(File.join(destination, 'bin/run'))
      assert_empty git(destination, 'status', '--porcelain').out
      assert_equal ['refs/heads/main'], git(destination, 'for-each-ref', '--format=%(refname)').out.lines.map(&:strip)
      refute git(destination, 'cat-file', '-e', options[:source_commit] + '^{commit}', check: false).success?
      refute File.exist?(File.join(destination, '.git/objects/info/alternates'))
      refute File.exist?(File.join(destination, '.git/logs'))
      assert_includes git(destination, 'cat-file', '-p', 'HEAD').out, 'Stele curated export <export@example.invalid>'
      mapping = JSON.parse(File.read(File.join(root, 'export/mapping.json')))
      assert_equal options[:source_commit], mapping.fetch('source_commit')
      assert_equal first.fetch('export_tree'), mapping.fetch('export_tree')
      refute File.exist?(File.join(destination, 'mapping.json'))
      assert_equal 'pending_owner_review', first.fetch('publication_status')
    end
  end

  def test_untracked_source_file_and_untracked_license_are_refused
    fixture do |root, repo, options, files|
      File.write(File.join(repo, 'local-only'), 'untracked')
      commit_list(repo, options, files + ['local-only'])
      assert_raises(ArgumentError) { CuratedExport.new.run(**options) }
      refute File.exist?(options[:out])
      options[:license] = 'local-only'
      commit_list(repo, options, files)
      assert_raises(ArgumentError) { CuratedExport.new.run(**options) }
      refute File.exist?(options[:out])
      options[:include_list] = File.join(root, 'untracked-list')
      assert_raises(ArgumentError) { CuratedExport.new.run(**options) }
      File.write(File.join(repo, 'local-list.json'), JSON.generate('files' => files))
      assert_raises(ArgumentError) { CuratedExport.new.run(**options.merge(include_list: 'local-list.json')) }
      assert_raises(ArgumentError) { CuratedExport.new.run(**options.merge(include_list: 'publication/files.json', license: 'README.link')) }
    end
  end

  def test_traversal_git_metadata_submodules_and_escaping_links_are_refused
    fixture do |_root, repo, options, files|
      commit_list(repo, options, files + ['../README.md'])
      assert_raises(ArgumentError) { CuratedExport.new.run(**options) }
      commit_list(repo, options, files + ['.GIT/config'])
      assert_raises(ArgumentError) { CuratedExport.new.run(**options) }
      git(repo, 'update-index', '--add', '--cacheinfo', '160000,' + options[:source_commit] + ',submodule')
      commit_list(repo, options, files + ['submodule'])
      assert_raises(ArgumentError) { CuratedExport.new.run(**options) }
      File.symlink('../outside', File.join(repo, 'escape'));git(repo, 'add', 'escape')
      commit_list(repo, options, files + ['escape'])
      assert_raises(ArgumentError) { CuratedExport.new.run(**options) }
    end
  end

  def test_ref_names_conflicting_terms_and_existing_or_in_repo_outputs_refuse
    fixture do |_root, repo, options, files|
      assert_raises(ArgumentError) { CuratedExport.new.run(**options.merge(source_commit: 'HEAD')) }
      File.write(options[:out], 'preserve')
      assert_raises(ArgumentError) { CuratedExport.new.run(**options) }
      assert_equal 'preserve', File.read(options[:out])
      assert_raises(ArgumentError) { CuratedExport.new.run(**options.merge(out: File.join(repo, 'export'))) }
      File.write(File.join(repo, 'LICENSE'), 'old synthetic terms');git(repo, 'add', 'LICENSE')
      commit_list(repo, options, files + ['LICENSE'])
      assert_raises(ArgumentError) { CuratedExport.new.run(**options.merge(out: File.join(File.dirname(repo), 'conflicting-terms'))) }
    end
  end

  def test_artifact_findings_prevent_a_clean_result
    native_scanner
    fixture do |root, _repo, options, _files|
      payload = File.join(root, 'payload');Dir.mkdir(payload)
      File.write(File.join(payload, 'finding'), 'synthetic_private_source_marker')
      command = File.expand_path('../../prepare-public-export', __dir__)
      arguments = options.flat_map { |key, value| ['--' + key.to_s.tr('_', '-'), value] }
      output, error, status = Open3.capture3(RbConfig.ruby, command, *arguments, payload)
      assert_equal 1, status.exitstatus, error
      result = JSON.parse(output)
      refute result.fetch('scans_clean')
      assert_equal 1, JSON.parse(File.read(File.join(options[:out], 'review/result.json'))).fetch('identifier_findings')
      assert_equal 'pending_owner_review', result.fetch('publication_status')
    end
  end

  def test_inherited_git_identity_and_repository_do_not_change_the_export
    native_scanner
    fixture do |root, repo, options, _files|
      baseline = CuratedExport.new.run(**options)
      overrides = {'GIT_DIR' => File.join(repo, '.git'), 'GIT_WORK_TREE' => repo, 'GIT_AUTHOR_NAME' => 'PRIVATE_IDENTITY', 'GIT_AUTHOR_EMAIL' => 'private@example.invalid', 'GIT_COMMITTER_NAME' => 'PRIVATE_IDENTITY', 'GIT_CONFIG_COUNT' => '1', 'GIT_CONFIG_KEY_0' => 'init.templateDir', 'GIT_CONFIG_VALUE_0' => repo}
      previous = overrides.to_h { |key, _| [key, ENV[key]] }
      begin
        overrides.each { |key, value| ENV[key] = value }
        result = CuratedExport.new.run(**options.merge(out: File.join(root, 'isolated')))
        assert_equal baseline.fetch('export_commit'), result.fetch('export_commit')
      ensure
        previous.each { |key, value| ENV[key] = value }
      end
    end
  end

  def test_replace_refs_cannot_substitute_the_pinned_tree
    native_scanner
    fixture do |root, repo, options, _files|
      baseline = CuratedExport.new.run(**options)
      File.write(File.join(repo, 'README.md'), 'substituted tree')
      git(repo, 'add', 'README.md');git(repo, 'commit', '--quiet', '-m', 'replacement')
      replacement = git(repo, 'rev-parse', 'HEAD').out.strip
      git(repo, 'replace', options[:source_commit], replacement)
      result = CuratedExport.new.run(**options.merge(out: File.join(root, 'no-replacement')))
      assert_equal baseline.fetch('export_commit'), result.fetch('export_commit')
    end
  end

  def test_cli_help_unknown_arguments_and_noninteractive_empty_invocation
    command = File.expand_path('../../prepare-public-export', __dir__)
    out, _err, status = Open3.capture3(RbConfig.ruby, command, '--help')
    assert status.success?
    assert_includes out, '--source-commit'
    assert_includes out, '--include-list'
    _out, _err, status = Open3.capture3(RbConfig.ruby, command, '--unknown', '--help')
    assert_equal 2, status.exitstatus
    _out, _err, status = Open3.capture3(RbConfig.ruby, command)
    assert_equal 2, status.exitstatus
  end

  def test_git_metadata_aliases_are_rejected_before_payload_materialization
    fixture do |root, repo, options, files|
      git(repo, 'config', 'core.protectHFS', 'false')
      git(repo, 'config', 'core.protectNTFS', 'false')
      path = "zzz/.git\u200c/config"
      oid = git(repo, 'rev-parse', 'HEAD:README.md').out.strip
      git(repo, 'update-index', '--add', '--cacheinfo', '100644,' + oid + ',' + path)
      commit_list(repo, options, files + [path])
      assert_raises(StandardError) { CuratedExport.new.run(**options) }
      refute File.exist?(File.join(root, 'export/repo', path))
      refute File.exist?(File.join(root, 'export/repo/README.md'))
    end
  end

  def test_selected_unicode_file_names_keep_their_content
    native_scanner
    fixture do |root, repo, options, files|
      path = "caf\u00e9.md"
      File.write(File.join(repo, path), 'synthetic_unicode_path_marker')
      git(repo, 'add', path);commit_list(repo, options, files + [path])
      assert CuratedExport.new.run(**options).fetch('scans_clean')
      assert_equal 'synthetic_unicode_path_marker', File.read(File.join(root, 'export/repo', path))
      File.write(options.fetch(:deny_list), "synthetic_unicode_path_marker\n")
      out = File.join(root, 'unicode-finding')
      refute CuratedExport.new.run(**options.merge(out: out)).fetch('scans_clean')
      identifiers = JSON.parse(File.read(File.join(out, 'review/identifiers.json')))
      assert identifiers.fetch('matches').any? { |match| match['source'] == 'tree' && match['location'] == path }
    end
  end

  def test_case_colliding_names_cannot_be_silently_merged
    native_scanner
    fixture do |root, repo, options, files|
      collides = File.exist?(File.join(repo, 'Readme.md'))
      git(repo, 'config', 'core.ignorecase', 'false')
      oid = git(repo, 'rev-parse', 'HEAD:README.md').out.strip
      git(repo, 'update-index', '--add', '--cacheinfo', '100644,' + oid + ',Readme.md')
      commit_list(repo, options, files + ['Readme.md'])
      if collides
        assert_raises(StandardError) { CuratedExport.new.run(**options) }
        refute File.exist?(File.join(root, 'export/result.json'))
      else
        result = CuratedExport.new.run(**options)
        assert result.fetch('scans_clean')
        assert_equal File.read(File.join(root, 'export/repo/README.md')), File.read(File.join(root, 'export/repo/Readme.md'))
      end
    end
  end
end
