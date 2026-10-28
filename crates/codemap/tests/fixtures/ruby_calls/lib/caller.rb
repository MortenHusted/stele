require_relative "callee"
require_relative "inherit"
require "json"
require "prism"

def run
  worker = Worker.new
  a = worker.receiver_method(1)
  b = Greeter.create
  c = cross_file_free(2)
  d = JSON.parse("{}")
  e = worker.receiver_method(3)
  f = Prism.dump("1")
  a + b
end

def run2
  other = Other.new
  f = other.receiver_method(4)
  g = receiver_method(5)
  h = Sub.new.receiver_method(6)
  i = run
  i
end
