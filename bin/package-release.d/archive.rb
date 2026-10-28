# frozen_string_literal: true
require 'rubygems/package'
require 'zlib'

module ReleaseArchive
  def self.write(io, stage:, root:, epoch:)
    raise ArgumentError, 'invalid archive root' unless /\A[A-Za-z0-9][A-Za-z0-9.+_-]*\z/.match?(root)
    paths = Dir.glob('**/*', File::FNM_DOTMATCH, base: stage).reject { |path| %w[. ..].include?(path) }.sort
    raise ArgumentError, 'archive payload must contain only regular files and directories' unless paths.all? { |path| physical = File.join(stage, path);!File.symlink?(physical) && (File.file?(physical) || File.directory?(physical)) }
    gzip = Zlib::GzipWriter.new(io)
    gzip.mtime = 0
    begin
      [nil, *paths].each do |path|
        name = path ? root + '/' + path : root
        directory = path.nil? || File.directory?(File.join(stage, path))
        bytes = directory ? ''.b : File.binread(File.join(stage, path))
        mode = directory || path == 'stele' ? 0o755 : 0o644
        prefix = ''
        if name.bytesize > 100
          split = name.b.byteslice(0, 156).rindex('/')
          raise ArgumentError, 'archive path exceeds ustar limits' unless split
          prefix, name = name.byteslice(0, split), name.byteslice(split + 1..)
        end
        raise ArgumentError, 'archive path exceeds ustar limits' if prefix.bytesize > 155 || name.bytesize > 100
        header = Gem::Package::TarHeader.new(name: name, prefix: prefix, mode: mode, size: bytes.bytesize,
                                            typeflag: directory ? '5' : '0', uid: 0, gid: 0,
                                            uname: '', gname: '', mtime: epoch)
        gzip.write(header.to_s)
        gzip.write(bytes)
        gzip.write("\0" * ((512 - bytes.bytesize % 512) % 512))
      end
      gzip.write("\0" * 1024)
    ensure
      gzip.finish
    end
  end
end
