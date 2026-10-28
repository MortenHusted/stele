# frozen_string_literal: true

require 'digest'
require 'find'
require 'json'
require 'pathname'
require 'set'
require 'stringio'
require 'tty-command'
require_relative 'git_context'

class IdentifierScrub
  TOKEN = /[A-Za-z0-9_]+/n

  def initialize(repo:, deny_list:, artifacts: [], report: nil)
    @runner = TTY::Command.new(printer: :null)
    @repo = File.realpath(repo)
    @repo = File.realpath(git('rev-parse', '--show-toplevel').strip)
    @artifacts = artifacts.map { |path| File.realpath(path) }
    raise ArgumentError, 'artifacts must be unpacked directories' unless @artifacts.all? { |path| File.directory?(path) }
    @list = File.realpath(deny_list)
    private_path!(@list)
    bytes = File.binread(@list)
    entries = bytes.lines.map(&:strip).reject(&:empty?)
    raise ArgumentError, 'deny-list must contain one ASCII token per line' unless entries.all? { |entry| /\A[A-Za-z0-9_]+\z/n.match?(entry) }
    @denied = entries.to_set
    raise ArgumentError, 'deny-list must not be empty' if @denied.empty?
    @report = report && File.join(File.realpath(File.dirname(report)), File.basename(report))
    if @report
      private_path!(@report)
      raise ArgumentError, 'report must be a new local file' if File.exist?(@report) || File.symlink?(@report)
    end
    @result = {
      'version' => 'identifier-scrub/v1',
      'head' => git('rev-parse', 'HEAD').strip,
      'deny_list_sha256' => Digest::SHA256.hexdigest(bytes),
      'entries' => @denied.size,
      'sources' => %w[tree history artifacts].to_h { |source| [source, {'scanned' => 0, 'matches' => 0}] },
      'matches' => []
    }
  end

  def run
    raise ArgumentError, 'full history is required; fetch the shallow repository first' unless git('rev-parse', '--is-shallow-repository').strip == 'false'
    paths = git('ls-files', '--cached', '--others', '--exclude-standard', '-z').split("\0").map do |path|
      path.force_encoding(Encoding::UTF_8)
      raise ArgumentError, 'source paths must be valid UTF-8 for review' unless path.valid_encoding?
      path
    end.uniq.sort
    paths.each { |path| scan_file('tree', File.join(@repo, path), path) }
    scan_history
    @artifacts.each_with_index do |root, index|
      Find.find(root) do |path|
        next if File.directory?(path) && !File.symlink?(path)
        relative = Pathname.new(path).relative_path_from(Pathname.new(root)).to_s
        scan_file('artifacts', path, relative, location: "artifact[#{index}]/#{relative}")
      end
    end
    @result['matched_entries'] = @result['matches'].map { |match| match['entry'] }.uniq.sort
    if @report
      File.open(@report, File::WRONLY | File::CREAT | File::EXCL, 0o600) { |file| file.write(JSON.pretty_generate(@result) + "\n") }
    end
    @result
  end

  def self.summary(result)
    result.reject { |key, _| %w[matches matched_entries].include?(key) }
  end

  private

  def private_path!(path)
    roots = [@repo, *@artifacts]
    raise ArgumentError, 'deny-list and report must stay outside scanned inputs' if roots.any? { |root| path == root || path.start_with?("#{root}/") }
    probe = git_at(File.dirname(path), 'rev-parse', '--show-toplevel', check: false)
    raise ArgumentError, 'deny-list and report must stay outside Git repositories' if probe.success?
  end

  def git(*args)
    git_at(@repo, *args).out.b
  rescue TTY::Command::ExitError
    raise ArgumentError, 'cannot read repository history or inventory'
  end

  def git_at(repo, *args, check: true)
    @runner.public_send(check ? :run : :run!, *ScrubGitContext.command, 'git', '-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false', '-C', repo, *args, binmode: true)
  end

  def scan_file(source, path, label, location: label)
    bytes = File.symlink?(path) ? File.readlink(path).b : File.binread(path)
    reject_archive!(bytes)
    scan_bytes(source, location, label.b + "\n" + bytes)
  end

  def scan_history
    objects = StringIO.new(git('cat-file', '--batch-all-objects', '--batch'))
    while (header = objects.gets)
      oid, type, size = header.split
      raise ArgumentError, 'incomplete Git object stream' unless size && /\A\d+\z/.match?(size)
      length = Integer(size)
      bytes = objects.read(length)
      raise ArgumentError, 'incomplete Git object stream' unless bytes&.bytesize == length && objects.read(1) == "\n"
      reject_archive!(bytes) if type == 'blob'
      scan_bytes('history', "git:#{oid}:#{type}", bytes)
    end
  end

  def reject_archive!(bytes)
    magic = ["PK\x03\x04", "PK\x05\x06", "PK\x07\x08", "\x1f\x8b", 'BZh', "\xfd7zXZ\x00", "7z\xbc\xaf\x27\x1c", "\x28\xb5\x2f\xfd"]
    if magic.any? { |prefix| bytes.start_with?(prefix.b) } || bytes.byteslice(257, 5) == 'ustar'
      raise ArgumentError, 'compressed archives require a separately unpacked export before scanning'
    end
  end

  def scan_bytes(source, location, bytes)
    found = Set.new
    bytes.scan(TOKEN) { |token| found.add(token) if @denied.include?(token) }
    @result['sources'][source]['scanned'] += 1
    @result['sources'][source]['matches'] += found.size
    found.sort.each { |entry| @result['matches'] << {'source' => source, 'location' => location, 'entry' => entry} }
  end
end
