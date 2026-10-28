use lemmaspec::{mutate_artifact, MutationStatus, MutationTarget};

const MUTATION_EXAMPLE: &str = r#"
spec mutation_example {
  relation source { args: [symbol] }
  relation result { args: [symbol] }
  relation violation { args: [symbol] }
  relation detected { args: [symbol] }
  relation unexpected { args: [symbol] }

  fact source_release { relation: source args: [release] }
  fact violation_bad { relation: violation args: [bad] }
  fact detected_bad { relation: detected args: [bad] }

  rule derive_result {
    derive: "result(Item)"
    when: ["source(Item)"]
  }
  rule detect_unexpected {
    derive: "unexpected(Item)"
    when: ["detected(Item)", "!violation(Item)"]
  }

  expect result_is_derived { query: "result(release)" count: 1 }
  expect manifest_has_one_entry { query: "violation(Item)" count: 1 }
  expect nothing_unexpected { query: "unexpected(Item)" count: 0 }

  mutation rule_coverage {
    operator: drop_rule
    except: [detect_unexpected]
  }
  mutation condition_coverage {
    operator: drop_condition
    except: [detect_unexpected]
  }
  mutation manifest_guard {
    operator: drop_fact
    relation: violation
    must_fail: nothing_unexpected
  }
}
"#;

const REJECTED_ONLY_POLICY: &str = r#"
spec rejected_only_policy {
  relation input { args: [symbol] }
  relation output { args: [symbol] }

  fact input_exists { relation: input args: [value] }

  rule output_follows_input {
    derive: "output(Value)"
    when: ["input(Value)"]
  }

  expect output_exists { query: "output(value)" count: 1 }

  mutation conditions_are_observable {
    operator: drop_condition
  }
}
"#;

const NAMED_CONDITION_EXAMPLE: &str = r#"
spec named_condition_example {
  relation source { args: [symbol] }
  relation allowed { args: [symbol] }
  relation result { args: [symbol] }

  fact source_release { relation: source args: [release] }
  fact source_draft { relation: source args: [draft] }
  fact allowed_release { relation: allowed args: [release] }

  rule derive_result {
    derive: "result(Item)"
    when: {
      source_exists: "source(Item)"
      item_is_allowed: "allowed(Item)"
    }
  }

  expect exactly_one_result { query: "result(Item)" count: 1 }

  mutation condition_coverage {
    operator: drop_condition
    except: ["derive_result.source_exists"]
    must_fail: exactly_one_result
  }
}
"#;

#[test]
fn classifies_isolated_mutations_with_exact_oracles() {
    let report = mutate_artifact(MUTATION_EXAMPLE).expect("mutate example");

    assert_eq!(report.status, "vacuous");
    assert_eq!(report.baseline_status, "clean");
    assert_eq!(report.summary.total, 6);
    assert_eq!(report.summary.executed, 2);
    assert_eq!(report.summary.killed, 2);
    assert_eq!(report.summary.survived, 0);
    assert_eq!(report.summary.rejected, 1);
    assert_eq!(report.summary.excluded, 3);

    let manifest = report
        .mutations
        .iter()
        .find(|mutation| mutation.id == "manifest_guard:drop_fact:violation_bad")
        .expect("manifest mutant");
    assert_eq!(manifest.status, MutationStatus::Killed);
    assert_eq!(
        manifest
            .failed_expectations
            .iter()
            .map(|expectation| expectation.id.as_str())
            .collect::<Vec<_>>(),
        ["manifest_has_one_entry", "nothing_unexpected"]
    );
    assert!(matches!(
        manifest.target,
        MutationTarget::Fact { ref fact, ref relation }
            if fact == "violation_bad" && relation == "violation"
    ));

    let condition = report
        .mutations
        .iter()
        .find(|mutation| mutation.id == "condition_coverage:drop_condition:derive_result:1")
        .expect("condition mutant");
    assert_eq!(condition.status, MutationStatus::Rejected);
    assert!(condition
        .diagnostic
        .as_deref()
        .is_some_and(|diagnostic| diagnostic.contains("must contain at least one condition")));

    let condition_policy = report
        .policies
        .iter()
        .find(|policy| policy.id == "condition_coverage")
        .expect("condition policy summary");
    assert_eq!(condition_policy.status, "vacuous");
    assert_eq!(condition_policy.summary.executed, 0);
    assert_eq!(condition_policy.summary.rejected, 1);
    assert_eq!(condition_policy.summary.excluded, 2);
}

#[test]
fn all_rejected_policy_is_vacuous_instead_of_clean() {
    let report = mutate_artifact(REJECTED_ONLY_POLICY).expect("mutate rejected-only policy");

    assert_eq!(report.status, "vacuous");
    assert_eq!(report.summary.total, 1);
    assert_eq!(report.summary.executed, 0);
    assert_eq!(report.summary.killed, 0);
    assert_eq!(report.summary.survived, 0);
    assert_eq!(report.summary.rejected, 1);
    assert_eq!(report.policies.len(), 1);
    assert_eq!(report.policies[0].status, "vacuous");
    assert_eq!(report.policies[0].summary.executed, 0);
}

#[test]
fn every_policy_with_a_killed_mutant_is_clean() {
    let source = MUTATION_EXAMPLE.replace(
        "  mutation condition_coverage {\n    operator: drop_condition\n    except: [detect_unexpected]\n  }\n",
        "",
    );
    let report = mutate_artifact(&source).expect("mutate non-vacuous policies");

    assert_eq!(report.status, "clean");
    assert_eq!(report.policies.len(), 2);
    assert!(report
        .policies
        .iter()
        .all(|policy| policy.status == "clean" && policy.summary.executed > 0));
}

#[test]
fn named_oracle_cannot_be_masked_by_other_failures() {
    let source = MUTATION_EXAMPLE
        .replace("must_fail: nothing_unexpected", "must_fail: result_is_derived")
        .replace(
            "  mutation rule_coverage {\n    operator: drop_rule\n    except: [detect_unexpected]\n  }\n",
            "",
        )
        .replace(
            "  mutation condition_coverage {\n    operator: drop_condition\n    except: [detect_unexpected]\n  }\n",
            "",
        );
    let report = mutate_artifact(&source).expect("mutate with wrong named oracle");

    assert_eq!(report.status, "survived");
    assert_eq!(report.summary.survived, 1);
    let mutation = &report.mutations[0];
    assert_eq!(mutation.status, MutationStatus::Survived);
    assert_eq!(
        mutation
            .failed_expectations
            .iter()
            .map(|expectation| expectation.id.as_str())
            .collect::<Vec<_>>(),
        ["manifest_has_one_entry", "nothing_unexpected"]
    );
}

#[test]
fn incomplete_baseline_stops_before_generating_mutants() {
    let source = MUTATION_EXAMPLE.replace(
        "expect result_is_derived { query: \"result(release)\" count: 1 }",
        "expect result_is_derived { query: \"result(release)\" count: 0 }",
    );
    let report = mutate_artifact(&source).expect("report incomplete baseline");

    assert_eq!(report.status, "baseline_incomplete");
    assert_eq!(report.baseline_failures.len(), 1);
    assert_eq!(report.baseline_failures[0].id, "result_is_derived");
    assert!(report.mutations.is_empty());
}

#[test]
fn mutation_report_is_byte_stable() {
    let first = serde_json::to_vec_pretty(&mutate_artifact(MUTATION_EXAMPLE).unwrap()).unwrap();
    let second = serde_json::to_vec_pretty(&mutate_artifact(MUTATION_EXAMPLE).unwrap()).unwrap();

    assert_eq!(first, second);
}

#[test]
fn parallel_report_matches_the_sequential_report() {
    let report = mutate_artifact(include_str!("fixtures/mutation/parallel_order.lemmaspec"))
        .expect("mutate parallel order fixture");

    assert!(report.policies.len() > 1);
    assert!(
        report.summary.killed > 0
            && report.summary.survived > 0
            && report.summary.rejected > 0
            && report.summary.excluded > 0
    );
    assert_eq!(
        serde_json::to_string_pretty(&report).unwrap(),
        include_str!("fixtures/mutation/parallel_order.json").trim_end()
    );
}

#[test]
fn validates_mutation_configuration() {
    let without_policy = MUTATION_EXAMPLE
        .split("  mutation rule_coverage")
        .next()
        .unwrap();
    let without_policy = format!("{without_policy}}}\n");
    let error = mutate_artifact(&without_policy).expect_err("mutate requires a policy");
    assert!(error
        .to_string()
        .contains("artifact declares no mutation policies"));

    for (source, expected) in [
        (
            MUTATION_EXAMPLE.replace("relation: violation", "relation: missing"),
            "references unknown relation `missing`",
        ),
        (
            MUTATION_EXAMPLE.replace("must_fail: nothing_unexpected", "must_fail: missing"),
            "requires unknown expectation `missing` to fail",
        ),
        (
            MUTATION_EXAMPLE.replacen("except: [detect_unexpected]", "except: [missing]", 1),
            "excludes unknown rule `missing`",
        ),
    ] {
        let error = mutate_artifact(&source).expect_err("invalid mutation policy");
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn rejects_duplicate_mutation_policies() {
    let source = MUTATION_EXAMPLE.replace(
        "  mutation rule_coverage {\n    operator: drop_rule\n    except: [detect_unexpected]\n  }\n",
        "  mutation rule_coverage {\n    operator: drop_rule\n    except: [derive_result, detect_unexpected]\n  }\n  mutation duplicate_rule_coverage {\n    operator: drop_rule\n    except: [detect_unexpected, derive_result]\n  }\n",
    );

    let error = mutate_artifact(&source).expect_err("duplicate policies must be rejected");
    assert!(
        error.to_string().contains(
            "mutations `duplicate_rule_coverage` and `rule_coverage` declare identical policies"
        ),
        "{error}"
    );
}

#[test]
fn rejects_condition_policies_with_equivalent_exclusions() {
    let source = NAMED_CONDITION_EXAMPLE.replace(
        "  mutation condition_coverage {\n    operator: drop_condition\n    except: [\"derive_result.source_exists\"]\n    must_fail: exactly_one_result\n  }\n",
        "  mutation exclude_whole_rule {\n    operator: drop_condition\n    except: [derive_result]\n    must_fail: exactly_one_result\n  }\n  mutation exclude_each_condition {\n    operator: drop_condition\n    except: [\"derive_result.source_exists\", \"derive_result.item_is_allowed\"]\n    must_fail: exactly_one_result\n  }\n",
    );

    let error = mutate_artifact(&source).expect_err("equivalent policies must be rejected");
    assert!(
        error.to_string().contains("declare identical policies"),
        "{error}"
    );
}

#[test]
fn excludes_one_named_condition_without_excluding_its_rule() {
    let report = mutate_artifact(NAMED_CONDITION_EXAMPLE).expect("mutate named conditions");

    assert_eq!(report.status, "clean");
    assert_eq!(report.summary.total, 2);
    assert_eq!(report.summary.executed, 1);
    assert_eq!(report.summary.killed, 1);
    assert_eq!(report.summary.excluded, 1);

    let excluded = report
        .mutations
        .iter()
        .find(|mutation| mutation.id.ends_with(":source_exists"))
        .expect("excluded named condition");
    assert_eq!(excluded.status, MutationStatus::Excluded);

    let killed = report
        .mutations
        .iter()
        .find(|mutation| mutation.id.ends_with(":item_is_allowed"))
        .expect("executed named condition");
    assert_eq!(killed.status, MutationStatus::Killed);
    let target = serde_json::to_value(&killed.target).expect("serialize target");
    assert_eq!(target["condition"], "item_is_allowed");
    assert_eq!(target["index"], 2);
}

#[test]
fn named_condition_ids_survive_reordering() {
    let reordered = NAMED_CONDITION_EXAMPLE.replace(
        "      source_exists: \"source(Item)\"\n      item_is_allowed: \"allowed(Item)\"",
        "      item_is_allowed: \"allowed(Item)\"\n      source_exists: \"source(Item)\"",
    );

    let original = mutate_artifact(NAMED_CONDITION_EXAMPLE).expect("mutate original");
    let reordered = mutate_artifact(&reordered).expect("mutate reordered");
    let conditions = |report: &lemmaspec::MutationReport| {
        report
            .mutations
            .iter()
            .filter_map(|mutation| match &mutation.target {
                MutationTarget::Condition { expression, .. } => {
                    Some((mutation.id.clone(), (expression.clone(), mutation.status)))
                }
                _ => None,
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };

    assert_eq!(conditions(&original), conditions(&reordered));
}

#[test]
fn rejects_unknown_named_condition_exclusions() {
    let source = NAMED_CONDITION_EXAMPLE.replace(
        "derive_result.source_exists",
        "derive_result.missing_condition",
    );

    let error = mutate_artifact(&source).expect_err("unknown named condition must be rejected");
    assert!(
        error
            .to_string()
            .contains("excludes unknown rule or named condition `derive_result.missing_condition`"),
        "{error}"
    );
}

#[test]
fn rejects_duplicate_named_condition_ids() {
    let source = NAMED_CONDITION_EXAMPLE.replace("item_is_allowed:", "source_exists:");

    let error = mutate_artifact(&source).expect_err("condition ids must be unique within a rule");
    assert!(
        error
            .to_string()
            .contains("duplicate map entry `source_exists`"),
        "{error}"
    );
}
