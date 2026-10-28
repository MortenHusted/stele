require_relative "../models/user"

module Controllers
  class UsersController
    def index
      User.find(1)
      render()
    end
  end
end
