# frozen_string_literal: true
require 'tty-command'
require 'tempfile'

class PythonBenchmarkRunner < TTY::Command
  def initialize(diagnostic_directory)
    super(printer: :null)
    @diagnostic_directory = diagnostic_directory
  end

  def record_failure(error)
    file = Tempfile.create(['python-benchmark-failure-', '.log'], @diagnostic_directory)
    file.write(error.full_message)
    file.close
    warn "Benchmark diagnostics: #{file.path}"
  end

  def run(*args)
    super
  rescue TTY::Command::ExitError => error
    record_failure(error)
    raise
  end
end
