require "json"
require "minitest"

module SteleEvidence
  class Reporter < Minitest::AbstractReporter
    def initialize
      super
      @results = []
    end

    def record(result)
      failures = result.failures.map do |failure|
        case failure
        when Minitest::UnexpectedError then "error"
        when Minitest::Skip then "skip"
        when Minitest::Assertion then "assertion"
        else "error"
        end
      end
      @results << {class: result.class_name, method: result.name,
                   location: result.source_location, failures: failures}
    end

    def report
      receipt = {protocol: "minitest/1", completed: true,
                 ruby_engine: RUBY_ENGINE, ruby_version: RUBY_VERSION,
                 ruby_platform: RUBY_PLATFORM, minitest: Minitest::VERSION,
                 minitest_gem: Gem.loaded_specs.fetch("minitest").version.to_s,
                 results: @results}
      File.write(ENV.fetch("STELE_MINITEST_RECEIPT"), JSON.generate(receipt))
    end
  end

  def self.minitest_plugin_init(_options)
    expected_class = ENV.fetch("STELE_MINITEST_CLASS")
    expected_method = ENV.fetch("STELE_MINITEST_METHOD")
    klass = expected_class.split("::").inject(Object) { |scope, name| scope.const_get(name, false) }
    raise "unsupported runtime inheritance" unless klass.superclass == Minitest::Test
    location = klass.instance_method(expected_method).source_location
    expected_file = File.realpath(ENV.fetch("STELE_MINITEST_FILE"))
    expected_line = Integer(ENV.fetch("STELE_MINITEST_LINE"))
    unless location && File.realpath(location[0]) == expected_file && location[1] == expected_line
      raise "Minitest method source_location changed since discovery"
    end
    Minitest.reporter << Reporter.new
  end
end

Minitest.register_plugin(SteleEvidence)
