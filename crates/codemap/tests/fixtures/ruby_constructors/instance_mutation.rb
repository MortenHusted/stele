def configure(receiver)
  receiver.define_singleton_method(:perform) { 1 }
end
class Widget
  def initialize
    configure(self)
  end
  def perform; 0; end
  def act; 0; end
end
class Driver
  def replaced; Widget.new.perform; end
  def unaffected; Widget.new.act; end
end
