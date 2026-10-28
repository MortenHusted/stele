class Widget
  def initialize(value); @value = value; end
  def perform; @value; end
end
module ConstructorHook
  def self.included(base)
    base.define_singleton_method(:new) { Widget.new(1) }
  end
end
class IncludedHook
  include ConstructorHook
  def perform; 0; end
end
class Driver
  def direct; Widget.new(1).perform; end
  def included_hook; IncludedHook.new.perform; end
end
