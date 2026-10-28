# frozen_string_literal: true

module ScrubGitContext
  def self.command(attributes: nil)
    unset = (ENV.keys.grep(/\AGIT_/) + %w[GITLEAKS_CONFIG GITLEAKS_CONFIG_TOML]).uniq
    args = ['env', *unset.flat_map { |key| ['-u', key] },
            'GIT_CONFIG_NOSYSTEM=1', 'GIT_CONFIG_GLOBAL=' + File::NULL,
            'GIT_NO_LAZY_FETCH=1', 'GIT_PAGER=', 'GIT_CONFIG_COUNT=2',
            'GIT_CONFIG_KEY_0=core.hooksPath', 'GIT_CONFIG_VALUE_0=' + File::NULL,
            'GIT_CONFIG_KEY_1=core.fsmonitor', 'GIT_CONFIG_VALUE_1=false']
    args << 'GIT_ATTR_SOURCE=' + attributes if attributes
    args
  end
end
