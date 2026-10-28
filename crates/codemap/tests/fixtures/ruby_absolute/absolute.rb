module Feature
  def root_marker
    1
  end
end
module AbsoluteSpace; end
class Base
  def base_marker
    0
  end
end
class Outer
  class Top
    def decoy_marker
      0
    end
  end
  class Base
    def base_marker
      2
    end
  end
  module Feature
    def lexical_marker
      3
    end
  end
  class ::Top < Base
    module OwnFeature
      def own_marker
        4
      end
    end
    include Feature
    include ::Feature
    include OwnFeature
    def run
      root_marker
      lexical_marker
      own_marker
      base_marker
      ::AbsoluteSpace::OtherTop.ping
    end
  end
  class ::Top::Inner
    include ::Feature
    def run_inner
      root_marker
    end
  end
  class ::AbsoluteSpace::OtherTop
    include ::Feature
    def self.ping
      5
    end
  end
  module ::GlobalModule
    include Feature
    def run_module
      lexical_marker
    end
  end
end
