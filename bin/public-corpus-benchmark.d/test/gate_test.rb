# frozen_string_literal: true

require 'minitest/autorun'
require_relative '../gate'

class PublicCorpusGateTest < Minitest::Test
  def setup
    @contract = {'language' => 'go', 'commit' => 'a' * 40, 'minimum_files_exclusive' => 900}
    @snapshot = {
      'identity' => {'language' => 'go', 'commit_sha' => 'a' * 40, 'dirty' => false},
      'report' => {'files_scanned' => 901}, 'edges' => [{'kind' => 'Calls'}]
    }
  end

  def test_accepts_the_declared_corpus_and_rejects_wrong_identity
    assert_equal({'files_scanned' => 901, 'calls' => 1}, PublicCorpusGate.verify_snapshot(@snapshot, @contract))
    [{'language' => 'php'}, {'commit_sha' => 'b' * 40}, {'dirty' => true}].each do |change|
      changed = @snapshot.merge('identity' => @snapshot['identity'].merge(change))
      assert_raises(ArgumentError) { PublicCorpusGate.verify_snapshot(changed, @contract) }
    end
  end

  def test_identical_empty_or_undersized_snapshots_do_not_pass
    assert_raises(ArgumentError) { PublicCorpusGate.verify_snapshot(@snapshot.merge('edges' => []), @contract) }
    assert_raises(ArgumentError) do
      PublicCorpusGate.verify_snapshot(@snapshot.merge('report' => {'files_scanned' => 900}), @contract)
    end
  end

  def test_syntax_snapshots_must_not_admit_calls
    PublicCorpusGate.verify_snapshot(@snapshot.merge('edges' => []), @contract, semantic: false)
    assert_raises(ArgumentError) { PublicCorpusGate.verify_snapshot(@snapshot, @contract, semantic: false) }
  end

  def test_a_missing_or_differently_named_test_is_not_oracle_evidence
    name = 'named_oracle'
    PublicCorpusGate.verify_oracle("test named_oracle ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;", name)
    assert_raises(ArgumentError) { PublicCorpusGate.verify_oracle('test result: ok. 0 passed; 0 failed; 0 ignored;', name) }
    assert_raises(ArgumentError) do
      PublicCorpusGate.verify_oracle("test another_test ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;", name)
    end
  end

  def test_every_declared_language_has_matching_oracle_and_tool_pins
    %w[php go rust ruby].each do |language|
      gate = PublicCorpusGate.new(language: language)
      assert_equal language, gate.contract['language']
      assert_equal 40, gate.contract['commit'].length
      assert_operator gate.contract['minimum_files_exclusive'], :>, 0
    end
  end
end
