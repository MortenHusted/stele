class Worker
  def receiver_method(value)
    value
  end

  def associated
    1
  end
end

module Greeter
  def self.create
    Worker.new
  end
end

def cross_file_free(x)
  x
end

class WithAttr
  attr_accessor :label

  def uses_attr
    self.label
  end

  def sibling_target
    uses_attr
  end
end

module Mixable
  def mixed_method
    1
  end
end

class Host
  include Mixable

  def calls_mixin
    mixed_method
  end
end

module Billing
  class Report
    def self.compile
      1
    end
  end
end

module Admin
  class Report
    def self.compile
      2
    end
  end
end

module Billing
  class Invoice
    def build
      Report.compile
    end
  end
end

# A method that calls itself is a verified implicit-self call like any other;
# the edge from a definition to itself must not be dropped as noise.
class Recursive
  def descend
    descend
  end
end
