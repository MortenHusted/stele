# frozen_string_literal: true
require 'minitest/autorun'
require 'tmpdir'
require 'fileutils'
require 'open3'
require 'stringio'
require_relative '../notices'
require_relative '../package'

class ReleaseNoticesTest < Minitest::Test
  def fixture(files)
    Dir.mktmpdir do |scratch|
      root = File.realpath(scratch)
      repo, home, stage = %w[repo cargo stage].map { |name| File.join(root, name).tap { |path| Dir.mkdir(path) } }
      ReleaseNotices::PROJECT_FILES.each do |path|
        physical = File.join(repo, path);FileUtils.mkdir_p(File.dirname(physical));File.write(physical, 'synthetic project notice')
      end
      FileUtils.mkdir_p(File.join(repo, 'publication'))
      File.write(File.join(repo, 'publication/dependency-notices.json'), JSON.generate('packages' => {}))
      cache = File.join(home, 'registry/cache/public');FileUtils.mkdir_p(cache)
      archive = File.join(cache, 'fixture-1.0.0.crate')
      Zlib::GzipWriter.open(archive) do |gzip|
        Gem::Package::TarWriter.new(gzip) do |tar|
          files.each { |path, bytes| tar.add_file_simple('fixture-1.0.0/' + path, 0o644, bytes.bytesize) { |entry| entry.write(bytes) } }
        end
      end
      checksum = Digest::SHA256.file(archive).hexdigest
      File.write(File.join(repo, 'Cargo.lock'), "version = 4\n\n[[package]]\nname = \"fixture\"\nversion = \"1.0.0\"\nsource = #{JSON.generate(ReleaseNotices::REGISTRY)}\nchecksum = #{JSON.generate(checksum)}\n")
      package = {'id' => 'fixture', 'name' => 'fixture', 'version' => '1.0.0', 'source' => ReleaseNotices::REGISTRY, 'license' => 'MIT', 'license_file' => nil}
      metadata = {'workspace_root' => repo, 'packages' => [{'id' => 'root', 'name' => 'stele', 'source' => nil}, package, {'id' => 'dev', 'name' => 'unshipped-dev', 'source' => ReleaseNotices::REGISTRY}], 'resolve' => {'nodes' => [{'id' => 'root', 'deps' => [{'pkg' => 'fixture', 'dep_kinds' => [{'kind' => nil}]}, {'pkg' => 'dev', 'dep_kinds' => [{'kind' => 'dev'}]}]}, {'id' => 'fixture', 'deps' => []}]}}
      yield repo, home, stage, archive, metadata, checksum
    end
  end

  def test_collects_normal_dependency_and_nested_notices_without_dev_packages
    fixture('LICENSE' => 'synthetic MIT grant', 'src/unicode/LICENSE-UNICODE' => 'synthetic Unicode grant') do |repo, home, stage, _archive, metadata, checksum|
      report = ReleaseNotices.new(repo: repo, cargo_home: home).write(stage, metadata)
      assert_equal ['fixture'], report.fetch('packages').map { |package| package.fetch('name') }
      assert_equal checksum, report.fetch('packages').first.fetch('archive_sha256')
      assert_equal 'synthetic Unicode grant', File.read(File.join(stage, 'licenses/fixture-1.0.0/src/unicode/LICENSE-UNICODE'))
      assert File.file?(File.join(stage, 'licenses/project/crates/codemap-topology/NOTICE'))
      refute File.read(File.join(stage, 'THIRD_PARTY_INVENTORY.json')).include?(home)
    end
  end

  def test_standard_library_notices_are_retained_beside_dependency_notices
    fixture('LICENSE' => 'synthetic MIT grant') do |repo, home, stage, _archive, metadata, _checksum|
      sysroot = File.join(File.dirname(repo), 'rust');docs = File.join(sysroot, 'share/doc/rust')
      FileUtils.mkdir_p(File.join(docs, 'licenses'))
      File.write(File.join(docs, 'COPYRIGHT-library.html'), 'synthetic library attribution')
      File.write(File.join(docs, 'licenses/MIT.txt'), 'synthetic library grant')
      report = ReleaseNotices.new(repo: repo, cargo_home: home).write(stage, metadata, sysroot: sysroot, compiler: {'release' => 'fixture'})
      assert_equal 'synthetic library grant', File.read(File.join(stage, 'licenses/rust/licenses/MIT.txt'))
      assert_equal 'fixture', report.fetch('rust_standard_library').fetch('compiler').fetch('release')
    end
  end

  def test_changed_archive_refuses_before_copying_package_data
    fixture('LICENSE' => 'synthetic MIT grant') do |repo, home, stage, archive, metadata, _checksum|
      File.open(archive, 'ab') { |file| file.write('tampered') }
      assert_raises(ArgumentError) { ReleaseNotices.new(repo: repo, cargo_home: home).write(stage, metadata) }
      refute File.exist?(File.join(stage, 'licenses/fixture-1.0.0'))
    end
  end

  def test_nested_unicode_notice_does_not_replace_root_mit_and_supplement_binds_vcs
    commit = 'a' * 40
    fixture('src/unicode/LICENSE' => 'synthetic ICU grant', '.cargo_vcs_info.json' => JSON.generate('git' => {'sha1' => commit})) do |repo, home, stage, _archive, metadata, checksum|
      assert_raises(ArgumentError) { ReleaseNotices.new(repo: repo, cargo_home: home).write(stage, metadata) }
      supplement = 'synthetic root MIT grant';path = 'publication/fixture.LICENSE';File.write(File.join(repo, path), supplement)
      pins = {'packages' => {'fixture' => {'version' => '1.0.0', 'crate_sha256' => checksum, 'commit' => commit, 'path' => path, 'sha256' => Digest::SHA256.hexdigest(supplement), 'url' => 'https://example.invalid/fixture/LICENSE'}}}
      File.write(File.join(repo, 'publication/dependency-notices.json'), JSON.generate(pins))
      second = File.join(File.dirname(stage), 'second');Dir.mkdir(second)
      ReleaseNotices.new(repo: repo, cargo_home: home).write(second, metadata)
      assert_equal supplement, File.read(File.join(second, 'licenses/fixture-1.0.0/LICENSE.upstream'))
      pins['packages']['fixture']['commit'] = 'b' * 40;File.write(File.join(repo, 'publication/dependency-notices.json'), JSON.generate(pins))
      third = File.join(File.dirname(stage), 'third');Dir.mkdir(third)
      assert_raises(ArgumentError) { ReleaseNotices.new(repo: repo, cargo_home: home).write(third, metadata) }
    end
  end

  def test_archive_notice_path_cannot_escape_stage
    fixture('../LICENSE' => 'escape') do |repo, home, stage, _archive, metadata, _checksum|
      assert_raises(ArgumentError) { ReleaseNotices.new(repo: repo, cargo_home: home).write(stage, metadata) }
      refute File.exist?(File.join(stage, 'licenses/LICENSE'))
    end
  end

  def test_archive_headers_are_private_deterministic_and_have_no_dot_entry
    Dir.mktmpdir do |stage|
      File.write(File.join(stage, 'stele'), 'synthetic executable')
      FileUtils.mkdir_p(File.join(stage, 'licenses'))
      File.write(File.join(stage, 'licenses/NOTICE'), 'synthetic notice')
      first = StringIO.new(''.b)
      ReleaseArchive.write(first, stage: stage, root: 'stele-fixture-target', epoch: 123456)
      File.utime(Time.now, Time.now, File.join(stage, 'stele'))
      second = StringIO.new(''.b)
      ReleaseArchive.write(second, stage: stage, root: 'stele-fixture-target', epoch: 123456)
      assert_equal first.string, second.string
      assert_equal 0, first.string.byteslice(4, 4).unpack1('V')
      names = []
      Zlib::GzipReader.wrap(StringIO.new(first.string)) do |gzip|
        Gem::Package::TarReader.new(gzip) do |tar|
          tar.each do |entry|
            names << entry.full_name
            assert_equal 0, entry.header.uid
            assert_equal 0, entry.header.gid
            assert_empty entry.header.uname
            assert_empty entry.header.gname
            assert_equal 123456, entry.header.mtime
            assert_includes %w[0 5], entry.header.typeflag
            expected = entry.directory? || entry.full_name.end_with?('/stele') ? 0o755 : 0o644
            assert_equal expected, entry.header.mode
            assert entry.full_name == 'stele-fixture-target' || entry.full_name.start_with?('stele-fixture-target/')
          end
        end
      end
      assert_equal names.sort, names
      refute_includes names, '.'
      refute names.any? { |name| name.include?('/._') }
    end
  end

  def test_invalid_release_arguments_fail_before_build
    assert_raises(ArgumentError) { ReleasePackage.new.run(version: '0.0.0', target: 'unsupported') }
    assert_raises(ArgumentError) { ReleasePackage.new.run(version: '0.0.0', target: 'aarch64-apple-darwin') }
    command = File.expand_path('../../package-release', __dir__)
    out, _err, status = Open3.capture3(RbConfig.ruby, command, '--help')
    assert status.success?
    assert_includes out, '--dist-dir'
    _out, _err, status = Open3.capture3(RbConfig.ruby, command, '--unknown', '--help')
    assert_equal 2, status.exitstatus
  end

  def test_symlinked_cargo_home_keeps_both_spellings_in_build_flags
    fixture('LICENSE' => 'synthetic grant') do |repo, home, stage, _archive, metadata, _checksum|
      File.write(File.join(repo, 'Cargo.toml'), "[workspace.package]\nversion = \"1.0.0\"\n")
      link = File.join(File.dirname(home), 'cargo-alias');File.symlink(home, link)
      metadata['target_directory'] = stage
      package = ReleasePackage.new;package.instance_variable_set(:@root, repo)
      captured = nil
      fake = lambda do |*args, **options|
        if args.include?('build')
          captured = options.fetch(:env)
          raise ArgumentError, 'stop before owned build'
        end
        value = if args.include?('metadata') then JSON.generate(metadata)
                elsif args.include?('sysroot') then home
                elsif args.include?('-vV') then 'release: synthetic'
                else '123456'
                end
        Struct.new(:out).new(value)
      end
      previous = ENV['CARGO_HOME'];ENV['CARGO_HOME'] = link
      begin
        package.define_singleton_method(:clean_head) { 'synthetic-head' }
        package.define_singleton_method(:command, &fake)
        assert_match(/stop before owned build/, assert_raises(ArgumentError) { package.run(version: '1.0.0', target: 'aarch64-apple-darwin', dist_dir: File.join(File.dirname(stage), 'dist')) }.message)
      ensure
        ENV['CARGO_HOME'] = previous
      end
      [link, File.realpath(home)].each do |path|
        assert_includes captured.fetch('CARGO_ENCODED_RUSTFLAGS').split("\x1f"), '--remap-path-prefix=' + path + '=/stele/dependencies'
        assert_includes Shellwords.split(captured.fetch('CFLAGS')), '-ffile-prefix-map=' + path + '=/stele/dependencies'
      end
      bad = (link + '/registry/src/index.crates.io-synthetic/pkg/file.rs').b
      assert_raises(ArgumentError) { package.send(:check_paths!, bad, []) }
      good = '/stele/dependencies/registry/src/index.crates.io-synthetic/pkg/file.rs'.b
      package.send(:check_paths!, good, [link, File.realpath(home)])
      package.send(:check_paths!, ("\xff".b + good).force_encoding(Encoding::UTF_8), [])
      package.send(:check_paths!, '/stele/builder/toolchain/lib/rustlib/src/rust/library/core.rs'.b, [])
      assert_raises(ArgumentError) { package.send(:check_paths!, '/unexpected/toolchain/lib/rustlib/src/rust/library/core.rs'.b, []) }
    end
  end
end
