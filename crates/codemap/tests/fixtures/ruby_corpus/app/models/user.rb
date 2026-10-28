require "json"
require_relative "record"
require_relative "auditable"

class User < Record
  include Auditable
  extend Auditable

  def save
    persist()
    audit_log
    touch
  end

  def self.find(id)
    new()
  end
end
