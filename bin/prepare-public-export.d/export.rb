# frozen_string_literal: true
require 'digest'
require 'fileutils'
require 'json'
require 'tty-command'
require_relative '../export-review.d/review'

class CuratedExport
  IDENTITY = {'GIT_AUTHOR_NAME' => 'Stele curated export', 'GIT_AUTHOR_EMAIL' => 'export@example.invalid',
              'GIT_COMMITTER_NAME' => 'Stele curated export', 'GIT_COMMITTER_EMAIL' => 'export@example.invalid'}.freeze

  def initialize
    @runner = TTY::Command.new(printer: :null)
  end

  def run(repo:, source_commit:, include_list:, license:, notice:, out:, deny_list:, scanner_archive:, artifacts: [])
    repo = File.realpath(repo)
    raise ArgumentError, 'source must be a full pinned commit ID' unless /\A(?:[0-9a-f]{40}|[0-9a-f]{64})\z/.match?(source_commit)
    raise ArgumentError, 'source must be a Git worktree root' unless File.realpath(git(repo, 'rev-parse', '--show-toplevel').out.strip) == repo
    raise ArgumentError, 'source pin is not the named commit' unless git(repo, 'rev-parse', source_commit + '^{commit}').out.strip == source_commit
    entries = git(repo, 'ls-tree', '-rz', '--full-tree', source_commit).out.split("\0").to_h do |line|
      metadata, path = line.split("\t", 2)
      mode, type, oid = metadata.split
      [path.force_encoding(Encoding::UTF_8), {'mode' => mode, 'type' => type, 'oid' => oid}]
    end
    list_bytes = blob(repo, entries, relative(include_list), regular: true)
    policy = JSON.parse(list_bytes)
    raise ArgumentError, 'include list must contain only a nonempty files array' unless policy.is_a?(Hash) && policy.keys == ['files'] && policy['files'].is_a?(Array) && !policy['files'].empty?
    files = policy.fetch('files').map { |path| relative(path) }
    raise ArgumentError, 'include list contains duplicate paths' unless files.uniq == files
    selected = files.to_h { |path| [path, path] }
    {'LICENSE' => license, 'NOTICE' => notice}.each do |destination, input|
      input = relative(input)
      raise ArgumentError, 'include list conflicts with explicit terms' if selected.key?(destination) && selected[destination] != input
      selected[destination] = input
    end
    payload = selected.sort.map do |destination, input|
      bytes = blob(repo, entries, input, regular: %w[LICENSE NOTICE].include?(destination))
      entry = entries.fetch(input)
      {'path' => destination, 'input' => input, 'mode' => entry.fetch('mode'), 'sha256' => Digest::SHA256.hexdigest(bytes), 'bytes' => bytes}
    end
    payload.each do |entry|
      next unless entry.fetch('mode') == '120000'
      target = entry.fetch('bytes').dup.force_encoding(Encoding::UTF_8)
      raise ArgumentError, 'source link must be a relative tracked regular file' unless target.valid_encoding? && !target.start_with?('/') && !target.include?("\0")
      resolved = File.expand_path(target, File.join('/export', File.dirname(entry.fetch('path'))))
      linked = payload.find { |candidate| '/export/' + candidate.fetch('path') == resolved }
      raise ArgumentError, 'source link leaves selected regular files' unless linked && %w[100644 100755].include?(linked.fetch('mode'))
    end
    out = File.join(File.realpath(File.dirname(out)), File.basename(out))
    inputs = [repo, *artifacts.map { |path| File.realpath(path) }]
    raise ArgumentError, 'output must be new and outside Git and scanned inputs' if File.exist?(out) || File.symlink?(out) || git(File.dirname(out), 'rev-parse', '--show-toplevel', check: false).success? || inputs.any? { |input| out == input || out.start_with?(input + '/') || input.start_with?(out + '/') }
    epoch = git(repo, 'show', '-s', '--format=%ct', source_commit).out.strip
    Dir.mkdir(out, 0o700)
    destination = File.join(out, 'repo');Dir.mkdir(destination, 0o700)
    git(destination, 'init', '--quiet', '--template=', '--object-format=sha1', '--initial-branch=main')
    payload.each do |entry|
      bytes = entry.fetch('bytes')
      oid = Digest::SHA1.hexdigest("blob #{bytes.bytesize}\0".b + bytes)
      entry['blob_oid'] = oid
      git(destination, '-c', 'core.protectHFS=true', '-c', 'core.protectNTFS=true', 'update-index', '--add', '--cacheinfo', entry.fetch('mode') + ',' + oid + ',' + entry.fetch('path'))
    end
    payload.each do |entry|
      path = File.join(destination, entry.fetch('path'))
      FileUtils.mkdir_p(File.dirname(path))
      File.open(path, File::WRONLY | File::CREAT | File::EXCL, entry.fetch('mode') == '100755' ? 0o755 : 0o644) { |file| file.write(entry.fetch('bytes')) }
      git(destination, 'hash-object', '-w', '--no-filters', '--', path)
      if entry.fetch('mode') == '120000'
        File.unlink(path);File.symlink(entry.fetch('bytes'), path)
      end
    end
    tree = git(destination, 'write-tree').out.strip
    expected_tree = payload.map { |entry| (entry.fetch('mode') + ' blob ' + entry.fetch('blob_oid') + "\t" + entry.fetch('path') + "\0").b }.join
    raise ArgumentError, 'export tree differs from selected paths, modes or bytes' unless git(destination, 'ls-tree', '-rz', '--full-tree', tree).out.b == expected_tree
    identity = IDENTITY.merge('GIT_AUTHOR_DATE' => epoch + ' +0000', 'GIT_COMMITTER_DATE' => epoch + ' +0000')
    commit = git(destination, '-c', 'commit.gpgsign=false', 'commit-tree', tree, '-m', 'Initial curated source export', env: identity).out.strip
    git(destination, '-c', 'core.logAllRefUpdates=false', 'update-ref', 'refs/heads/main', commit)
    raise ArgumentError, 'export must have one clean root and one ref' unless git(destination, 'rev-list', '--all', '--parents').out.strip == commit && git(destination, 'for-each-ref', '--format=%(refname)').out.strip == 'refs/heads/main' && git(destination, 'status', '--porcelain', '--untracked-files=all').out.empty?
    mapping = {'contract' => 'stele-curated-export-map/v1', 'source_commit' => source_commit, 'include_list' => include_list, 'include_list_sha256' => Digest::SHA256.hexdigest(list_bytes), 'export_commit' => commit, 'export_tree' => tree, 'files' => payload.map { |entry| entry.reject { |key, _| key == 'bytes' } }, 'publication_status' => 'pending_owner_review'}
    write(out, 'mapping.json', mapping)
    review = ExportReview.new.run(repo: destination, deny_list: deny_list, scanner_archive: scanner_archive, out: File.join(out, 'review'), artifacts: artifacts)
    result = {'contract' => 'stele-curated-export/v1', 'export_commit' => commit, 'export_tree' => tree, 'scans_clean' => review.fetch('scans_clean'), 'manual_secret_review_count' => review.fetch('manual_secret_review_count'), 'mapping_sha256' => Digest::SHA256.file(File.join(out, 'mapping.json')).hexdigest, 'review_sha256' => Digest::SHA256.file(File.join(out, 'review/result.json')).hexdigest, 'publication_status' => 'pending_owner_review'}
    write(out, 'result.json', result)
    result
  end

  private

  def relative(path)
    raise ArgumentError, 'source inputs must be tracked relative paths without Git metadata or traversal' unless path.is_a?(String) && path.valid_encoding? && !path.match?(/[\x00-\x1f\x7f\\]/) && path.split('/', -1).none? { |part| ['', '.', '..'].include?(part) || part.downcase == '.git' }
    path
  end

  def blob(repo, entries, path, regular: false)
    entry = entries[path]
    modes = regular ? %w[100644 100755] : %w[100644 100755 120000]
    raise ArgumentError, 'input must be a tracked regular file or selected source link at the pin' unless entry && entry.fetch('type') == 'blob' && modes.include?(entry.fetch('mode'))
    git(repo, 'cat-file', 'blob', entry.fetch('oid')).out.b
  end

  def write(out, name, value)
    File.open(File.join(out, name), File::WRONLY | File::CREAT | File::EXCL, 0o600) { |file| file.write(JSON.pretty_generate(value) + "\n") }
  end

  def git(repo, *args, check: true, env: {})
    result = @runner.run!(*ScrubGitContext.command, 'GIT_NO_REPLACE_OBJECTS=1', *env.map { |key, value| key + '=' + value }, 'git', '-C', repo, *args, binmode: true)
    raise ArgumentError, 'Git refused the requested object or path; no clean export' if check && !result.success?
    result
  end
end
