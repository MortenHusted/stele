# frozen_string_literal: true
require 'minitest/autorun'
require 'tmpdir'
require 'fileutils'
require 'open3'
require_relative '../preparation'

class CppBenchmarkPreparationTest < Minitest::Test
  def test_inventory_preserves_in_root_aliases_but_refuses_external_aliases
    Dir.mktmpdir do |scratch|
      root = File.realpath(scratch)
      source, headers = %w[source headers].map { |name| File.join(root, name).tap { |path| Dir.mkdir(path) } }
      runner = TTY::Command.new(printer: :null)
      runner.run('git', 'init', '--quiet', source)
      File.write(source + '/unit.cc', 'int f() { return 1; }')
      runner.run('git', '-C', source, 'add', 'unit.cc')
      File.write(headers + '/header.h', 'header')
      File.symlink('header.h', headers + '/alias.h')
      File.symlink('.', headers + '/cycle')
      preparation = CppBenchmarkPreparation.new
      result = preparation.send(:inventory, source, {'headers' => headers})
      assert_equal result.fetch('{headers}/header.h'), result.fetch('{headers}/alias.h')
      assert result.key?('{source}/unit.cc')
      File.write(root + '/outside', 'outside')
      File.symlink(root + '/outside', headers + '/external.h')
      assert_raises(ArgumentError) { preparation.send(:inventory, source, {'headers' => headers}) }
    end
  end

  def test_new_output_refuses_existing_files_aliases_and_git_roots
    Dir.mktmpdir do |scratch|
      root = File.realpath(scratch)
      runner = TTY::Command.new(printer: :null)
      File.write(root + '/existing', 'preserve')
      File.symlink('absent', root + '/alias')
      %w[existing alias].each { |name| assert_raises(ArgumentError) { CppBenchmarkPreparation.new_output(root + '/' + name, runner) } }
      runner.run('git', 'init', '--quiet', root)
      assert_raises(ArgumentError) { CppBenchmarkPreparation.new_output(root + '/output', runner) }
      refute File.exist?(root + '/output')
      assert_equal 'preserve', File.read(root + '/existing')
    end
  end

  def test_public_source_refuses_other_commit_before_preparation
    Dir.mktmpdir do |root|
      runner = TTY::Command.new(printer: :null)
      runner.run('git', 'init', '--quiet', root)
      File.write(root + '/a', 'source')
      runner.run('git', '-C', root, 'add', 'a')
      runner.run('git', '-C', root, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--quiet', '-m', 'fixture')
      assert_raises(ArgumentError) { CppBenchmarkPreparation.new.verify_source(root) }
    end
  end

  def test_sdk_and_operator_pins_refuse_drift
    preparation = CppBenchmarkPreparation.new
    Dir.mktmpdir do |root|
      File.write(root + '/SDKSettings.json', JSON.generate('Version' => '27.0', 'CanonicalName' => 'macosx27.0'))
      preparation.verify_sdk(root)
      File.write(root + '/SDKSettings.json', JSON.generate('Version' => '26.0', 'CanonicalName' => 'macosx26.0'))
      assert_raises(ArgumentError) { preparation.verify_sdk(root) }
      assert_raises(ArgumentError) { preparation.verify_operator('files' => {'clangd' => {'path' => root + '/missing', 'sha256' => '0' * 64}}) }
    end
  end

  def test_operator_dependencies_cannot_drift_behind_matching_llvm_images
    preparation = CppBenchmarkPreparation.new
    pins = preparation.contract.fetch('operator').fetch('image_pins')
    names = pins.keys | %w[library/libz3.5.3.2.0.dylib library/libzstd.1.5.7.dylib]
    Dir.mktmpdir do |root|
      files = names.to_h do |name|
        path = File.join(root, name)
        FileUtils.mkdir_p(File.dirname(path))
        bytes = 'synthetic ' + name
        File.write(path, bytes)
        sha = Digest::SHA256.hexdigest(bytes)
        pins[name] = sha if pins.key?(name)
        [name, {'path' => path, 'sha256' => sha}]
      end
      preparation.verify_operator('files' => files)
      %w[library/libz3.5.3.2.0.dylib library/libzstd.1.5.7.dylib].each do |name|
        path = files.fetch(name).fetch('path')
        bytes = File.read(path)
        File.write(path, 'changed dependency')
        assert_raises(ArgumentError) { preparation.verify_operator('files' => files) }
        File.write(path, bytes)
      end
    end
  end

  def test_commands_reject_unknown_arguments_and_have_noninteractive_help
    %w[prepare-cpp-benchmark cpp-public-benchmark].each do |name|
      command = File.expand_path('../../' + name, __dir__)
      out, err, status = Open3.capture3(RbConfig.ruby, command, '--unknown', '--help')
      assert_equal 2, status.exitstatus
      assert_empty out
      assert_includes err, 'Usage:'
      out, _err, status = Open3.capture3(RbConfig.ruby, command, '--help')
      assert status.success?
      assert_includes out, '--repo'
      _out, err, status = Open3.capture3(RbConfig.ruby, command)
      assert_equal 2, status.exitstatus
      assert_includes err, 'Usage:'
    end
  end
end
