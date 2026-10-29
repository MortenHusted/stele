module Redirected
  def self.class_methods(&body)
    const_set(:Commands, Module.new).module_eval(&body)
  end

  class_methods do
    def routed
      lexical_helper
    end
  end

  def lexical_helper
    :lexical
  end
end

class Host
  include Redirected

  def rejected
    routed
  end
end

module Arbitrary
  def self.install(&body)
    Module.new.module_eval(&body)
  end

  install do
    def misplaced
      :elsewhere
    end
  end
end

class ArbitraryHost
  include Arbitrary

  def rejected
    misplaced
  end
end

module Plain
  def helper
    :plain
  end

  def ordinary
    helper
  end
end

class PlainHost
  include Plain

  def accepted
    ordinary
  end
end

class Utility
  def self.value
    :value
  end
end

module Yielding
  def self.class_methods
    yield
  end

  class_methods do
    def unverified
      Utility.value
    end

    def self.unverified_singleton
      :singleton
    end

    class Explicit
      def helper
        :explicit
      end

      def accepted
        helper
      end

      def self.value
        :explicit_value
      end
    end
  end

  def rejected
    unverified
  end

  def rejected_singleton
    Yielding.unverified_singleton
  end
end

class Driver
  def accepted
    Yielding::Explicit.value
  end

  def rejected_block_self
    Object.new.instance_eval do
      helper
    end
  end

  def helper
    :driver
  end

  def rejected_explicit_block_self
    Object.new.instance_eval do
      self.helper
    end
  end

  def accepted_after_block
    Object.new.instance_eval { :unrelated }
    helper
  end

  def accepted_yielding_block
    [1].any? { helper }
  end

  def accepted_yielded_response
    YieldSource.perform { self.helper }
  end

  def rejected_instance_exec
    Object.new.instance_exec { helper }
  end

  def rejected_class_eval
    Module.new.class_eval { helper }
  end

  def rejected_class_exec
    Module.new.class_exec { helper }
  end

  def rejected_module_eval
    Module.new.module_eval { self.helper }
  end

  def rejected_module_exec
    Module.new.module_exec { self.helper }
  end
end

class YieldSource
  def self.perform
    yield
  end
end

Object.new.instance_eval do
  class EvalDeclared
    def helper
      :explicit_context
    end

    def accepted
      helper
    end
  end
end
