# frozen_string_literal: true
require 'minitest/autorun'
require 'tmpdir'
require_relative '../preparation'
require 'open3'

class BlackOperatorPreparationTest < Minitest::Test
  def test_runtime_selection_omits_generated_and_site_installed_content
    assert_equal 'bin/python3.14', BlackOperatorPreparation.runtime_path('python/bin/python3.14')
    assert_equal 'lib/python3.14/os.py', BlackOperatorPreparation.runtime_path('python/lib/python3.14/os.py')
    %w[python/bin/pip python/lib/python3.14/site-packages/a.py python/lib/python3.14/__pycache__/os.cpython-314.pyc python/lib/python3.14/pkg/a.pyc].each do |name|
      assert_nil BlackOperatorPreparation.runtime_path(name)
    end
    assert_raises(ArgumentError) { BlackOperatorPreparation.runtime_path('python/lib/python3.14/../../outside') }
  end

  def test_safe_paths_reject_archive_escapes_and_ambiguous_names
    ['/a', '../a', 'a/../b', 'a/./b', 'a\\b', "a\0b"].each do |name|
      assert_raises(ArgumentError) { BlackOperatorPreparation.relative(name) }
    end
  end

  def test_wrong_archive_digest_refuses_before_extraction
    Dir.mktmpdir do |root|
      path = File.join(root, 'archive');File.write(path, 'untrusted')
      assert_raises(ArgumentError) { BlackOperatorPreparation.verify_pin(path, '0' * 64) }
    end
  end

  def test_tar_links_are_refused
    Dir.mktmpdir do |root|
      archive = File.join(root, 'runtime.tar.gz')
      Zlib::GzipWriter.open(archive) do |gzip|
        Gem::Package::TarWriter.new(gzip) { |tar| tar.add_symlink('python/lib/python3.14/alias.py', '/outside', 0o777) }
      end
      dest = File.join(root, 'out');Dir.mkdir(dest)
      assert_raises(ArgumentError) { BlackOperatorPreparation.unpack_runtime(archive, dest) }
    end
  end

  def test_wheel_paths_cannot_escape_the_dependency_root
    Dir.mktmpdir do |root|
      archive = File.join(root, 'a.whl')
      Zip::File.open(archive, create: true) { |zip| zip.get_output_stream('../outside') { |file| file.write('bad') } }
      dest = File.join(root, 'out');Dir.mkdir(dest)
      assert_raises(ArgumentError) { BlackOperatorPreparation.unpack_wheel(archive, dest) }
      refute File.exist?(File.join(root, 'outside'))
    end
  end

  def test_runtime_archive_copies_source_and_omits_caches
    Dir.mktmpdir do |root|
      archive = File.join(root, 'runtime.tar.gz')
      Zlib::GzipWriter.open(archive) do |gzip|
        Gem::Package::TarWriter.new(gzip) do |tar|
          tar.add_file_simple('python/lib/python3.14/os.py', 0o644, 6) { |file| file.write('source') }
          tar.add_file_simple('python/lib/python3.14/empty.py', 0o644, 0) { |_file| }
          tar.add_file_simple("python/lib/python3.14/nul.py\0unused", 0o644, 1) { |file| file.write('x') }
          tar.add_file_simple('python/lib/python3.14/__pycache__/os.cpython-314.pyc', 0o644, 5) { |file| file.write('cache') }
        end
      end
      dest = File.join(root, 'out');Dir.mkdir(dest)
      BlackOperatorPreparation.unpack_runtime(archive, dest)
      assert_equal 'source', File.read(File.join(dest, 'lib/python3.14/os.py'))
      assert_equal '', File.read(File.join(dest, 'lib/python3.14/empty.py'))
      assert_equal 'x', File.read(File.join(dest, 'lib/python3.14/nul.py'))
      refute Dir.exist?(File.join(dest, 'lib/python3.14/__pycache__'))
    end
  end

  def test_runtime_pins_cover_only_the_declared_family_and_selected_tools
    Dir.mktmpdir do |scratch|
      root = File.realpath(scratch);Dir.mkdir(root + '/bin');File.write(root + '/bin/python3.14', 'python')
      cache = root + '/cache';Dir.mkdir(cache)
      File.write(cache + '/dyld.01', 'a');File.write(cache + '/dyld.02', 'b');File.write(cache + '/other', 'unrelated')
      File.write(root + '/tool', 'tool')
      tools = {'inspector/test' => {'path' => root + '/tool', 'role' => 'inspector'}}
      result = BlackOperatorPreparation.runtime_profile(root, cache, 'dyld.', tools)
      assert_equal %w[cache/dyld.01 cache/dyld.02 inspector/test python], result['files'].keys
      assert_equal Digest::SHA256.hexdigest('a'), result['files']['cache/dyld.01']['sha256']
      assert_raises(ArgumentError) { BlackOperatorPreparation.runtime_profile(root, cache, '', tools) }
      assert_raises(ArgumentError) { BlackOperatorPreparation.runtime_profile(root, cache, 'missing', tools) }
    end
  end
  def test_command_refuses_unknown_arguments_before_preparation
    command = File.expand_path('../../prepare-black-operator', __dir__)
    out, err, status = Open3.capture3(RbConfig.ruby, command, '--unknown', '--help')
    assert_equal 2, status.exitstatus
    assert_empty out
    assert_includes err, 'invalid arguments'
  end
end
