class Other
  def receiver_method(value)
    value * 2
  end
end

class Sub < Worker
  def inherited_call(value)
    receiver_method(value)
  end
end

class Parent
  def self.make
    1
  end
end

class Child < Parent
end

class Caller
  def constant_inherited
    Child.make
  end
end
