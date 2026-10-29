class Widget
  def initialize(value); @value = value; end
  def perform; @value; end
end
class Child < Widget; end
class InstanceNew
  def new; 0; end
  def perform; 1; end
end
module Feature
  def act; 1; end
end
class Mixed
  include Feature
end
class Factory
  def self.new; Widget.new(1); end
  def perform; 0; end
end
class InheritedFactory < Factory; end
module FactoryMethods
  def new; Widget.new(1); end
end
class ExtendedFactory
  extend FactoryMethods
  def perform; 0; end
end
module OuterFactory
  include FactoryMethods
end
class NestedExtensionFactory
  extend OuterFactory
  def perform; 0; end
end
class SingletonFactory
  class << self
    def new; Widget.new(1); end
  end
  def perform; 0; end
end
class ExternalChild < ExternalBase
  def perform; 0; end
end
class ExternalExtension
  extend ExternalFeature
  def perform; 0; end
end
class DynamicExtension
  extend feature_provider
  def perform; 0; end
end
module ConstructorConcern
  extend ExternalConcern
  included do
    def self.new; Widget.new(1); end
  end
end
class IncludedConstructor
  include ConstructorConcern
  def perform; 0; end
end
class ExternalInclusion
  include ExternalFeature
  def perform; 0; end
end
class ExplicitFactory
  def perform; 0; end
end
def ExplicitFactory.new; Widget.new(1); end
class SingletonReceiverFactory
  def perform; 0; end
end
class << SingletonReceiverFactory
  def new; Widget.new(1); end
end
module NotAClass
  def self.new; Widget.new(1); end
  def perform; 0; end
end
class Reopened
  def perform; 1; end
end
class ReflectiveFactory
  define_singleton_method(:new) { Widget.new(1) }
  def perform; 0; end
end
class AliasFactory
  def perform; 0; end
  class << self
    def build; Widget.new(1); end
    alias_method :new, :build
  end
end
class AliasKeywordFactory
  def perform; 0; end
  class << self
    def build; Widget.new(1); end
    alias new build
  end
end
class EvalFactory
  class_eval "def self.new; Widget.new(1); end"
  def perform; 0; end
end
class InstanceReflection
  def initialize
    define_singleton_method(:perform) { 1 }
  end
  def perform; 0; end
end
class InstanceDefinition
  def initialize
    def self.perform; 1; end
  end
  def perform; 0; end
end
class ForwardedFactory
  send(:define_singleton_method, :new) { Widget.new(1) }
  def perform; 0; end
end
class Driver
  def direct; Widget.new(1).perform; end
  def inherited; Child.new(1).perform; end
  def instance_named_new; InstanceNew.new.perform; end
  def mixed; Mixed.new.act; end
  def factory; Factory.new.perform; end
  def inherited_factory; InheritedFactory.new.perform; end
  def extended_factory; ExtendedFactory.new.perform; end
  def nested_extension_factory; NestedExtensionFactory.new.perform; end
  def singleton_factory; SingletonFactory.new.perform; end
  def external; ExternalChild.new.perform; end
  def external_extension; ExternalExtension.new.perform; end
  def dynamic_extension; DynamicExtension.new.perform; end
  def included_constructor; IncludedConstructor.new.perform; end
  def external_inclusion; ExternalInclusion.new.perform; end
  def explicit_factory; ExplicitFactory.new.perform; end
  def singleton_receiver_factory; SingletonReceiverFactory.new.perform; end
  def module_factory; NotAClass.new.perform; end
  def reopening; Reopened.new.perform; end
  def assigned; worker = Widget.new(1); worker.perform; end
  def reflective_factory; ReflectiveFactory.new.perform; end
  def alias_factory; AliasFactory.new.perform; end
  def alias_keyword_factory; AliasKeywordFactory.new.perform; end
  def eval_factory; EvalFactory.new.perform; end
  def instance_reflection; InstanceReflection.new.perform; end
  def instance_definition; InstanceDefinition.new.perform; end
  def forwarded_factory; ForwardedFactory.new.perform; end
end
