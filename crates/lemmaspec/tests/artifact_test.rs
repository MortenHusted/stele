use lemmaspec::{parse_artifact, walk_artifact};

const EXAMPLE: &str = include_str!("../examples/release_readiness.stele");

#[test]
fn parses_typed_self_contained_artifact() {
    let artifact = parse_artifact(EXAMPLE).expect("parse example");

    assert_eq!(artifact.name, "release_readiness");
    assert_eq!(artifact.relations.len(), 3);
    assert_eq!(artifact.facts.len(), 2);
    assert_eq!(artifact.rules.len(), 1);
    assert_eq!(artifact.expectations.len(), 1);
}

#[test]
fn walk_derives_expected_fact_with_proof() {
    let report = walk_artifact(EXAMPLE).expect("walk example");

    assert_eq!(report.status, "clean");
    assert!(report
        .expectations
        .iter()
        .all(|expectation| expectation.satisfied));
    let blocked = report
        .facts
        .iter()
        .find(|fact| fact.relation == "blocked" && fact.args == ["release"])
        .expect("derived blocked fact");
    assert_eq!(blocked.origin, "derived");
    assert!(blocked.why.contains("blocked_by_incomplete_dependency"));
    assert!(blocked.why.contains("release_needs_tests"));
    assert!(blocked.why.contains("tests_are_incomplete"));
    assert!(blocked.why.contains("plan:test-gate"));
}

#[test]
fn rejects_unknown_relation_and_wrong_arity() {
    let unknown = EXAMPLE.replace("relation: incomplete", "relation: missing");
    let error = walk_artifact(&unknown).expect_err("unknown relation must fail");
    assert!(
        error.to_string().contains("unknown relation `missing`"),
        "{error}"
    );

    let wrong_arity = EXAMPLE.replace("args: [release, tests]", "args: [release]");
    let error = walk_artifact(&wrong_arity).expect_err("wrong arity must fail");
    assert!(
        error.to_string().contains("expects 2 arguments, got 1"),
        "{error}"
    );
}

#[test]
fn rejects_rule_type_mismatch() {
    let source = EXAMPLE
        .replace("args: [symbol, symbol]", "args: [symbol, integer]")
        .replace("args: [release, tests]", "args: [release, 1]");
    let error = walk_artifact(&source).expect_err("rule variable type mismatch must fail");
    assert!(
        error
            .to_string()
            .contains("variable `Dependency` has incompatible types"),
        "{error}"
    );
}

#[test]
fn repeated_walk_is_byte_stable() {
    let first = serde_json::to_vec_pretty(&walk_artifact(EXAMPLE).unwrap()).unwrap();
    let second = serde_json::to_vec_pretty(&walk_artifact(EXAMPLE).unwrap()).unwrap();

    assert_eq!(first, second);
}

#[test]
fn validates_aggregate_input_types() {
    let source = r#"
spec aggregate_types {
  relation dependency { args: [symbol, symbol] }
  relation aggregate_result { args: [symbol, integer] }

  fact dependency_on_tests {
    relation: dependency
    args: [release, tests]
  }

  rule aggregate_dependencies {
    derive: "aggregate_result(Item, AGGREGATE(Dependency))"
    when: ["dependency(Item, Dependency)"]
  }
}
"#;

    for aggregate in ["min", "max"] {
        let source = source.replace("AGGREGATE", aggregate);
        let error = walk_artifact(&source).expect_err("numeric aggregate over symbol must fail");
        assert!(
            error
                .to_string()
                .contains(&format!("aggregate `{aggregate}` requires integer input")),
            "{error}"
        );
    }

    let sum = source.replace("AGGREGATE", "sum");
    let error = walk_artifact(&sum).expect_err("sum requires checked integer evaluation");
    assert!(
        error.to_string().contains(
            "aggregate `sum` is unavailable until checked integer evaluation is implemented"
        ),
        "{error}"
    );

    let count = source.replace("AGGREGATE", "count");
    let report = walk_artifact(&count).expect("count may aggregate symbols");
    assert!(report
        .facts
        .iter()
        .any(|fact| fact.relation == "aggregate_result" && fact.args == ["release", "1"]));
}

#[test]
fn validates_comparison_operand_types() {
    let symbol_ordering = r#"
spec symbol_ordering {
  relation named { args: [symbol] }
  relation selected { args: [symbol] }
  fact release_name { relation: named args: [release] }
  rule choose { derive: "selected(Name)" when: ["named(Name)", "Name < z"] }
}
"#;
    let error = walk_artifact(symbol_ordering).expect_err("symbol ordering must fail");
    assert!(
        error
            .to_string()
            .contains("ordering comparison requires integer operands"),
        "{error}"
    );

    let symbol_arithmetic = r#"
spec symbol_arithmetic {
  relation named { args: [symbol] }
  relation result { args: [integer] }
  fact release_name { relation: named args: [release] }
  rule calculate { derive: "result(Result)" when: ["named(Name)", "Result = Name + 1"] }
}
"#;
    let error = walk_artifact(symbol_arithmetic).expect_err("symbol arithmetic must fail");
    assert!(
        error
            .to_string()
            .contains("arithmetic expression requires integer operands"),
        "{error}"
    );

    let incompatible_equality = r#"
spec incompatible_equality {
  relation named { args: [symbol] }
  relation selected { args: [symbol] }
  fact release_name { relation: named args: [release] }
  rule choose { derive: "selected(Name)" when: ["named(Name)", "Name = 1"] }
}
"#;
    let error = walk_artifact(incompatible_equality).expect_err("mixed equality must fail");
    assert!(
        error
            .to_string()
            .contains("equality comparison has incompatible symbol and integer operands"),
        "{error}"
    );

    let valid = r#"
spec valid_comparisons {
  relation alias { args: [symbol, symbol] }
  relation rank { args: [symbol, integer] }
  relation selected { args: [symbol, integer] }

  fact release_alias { relation: alias args: [release, release] }
  fact release_rank { relation: rank args: [release, 1] }

  rule choose {
    derive: "selected(Item, Next)"
    when: [
      "rank(Item, Current)",
      "alias(Item, Alias)",
      "Item = Alias",
      "Next = Current + 1",
      "Next > Current",
    ]
  }
}
"#;
    let report = walk_artifact(valid).expect("compatible comparisons must pass");
    assert!(report
        .facts
        .iter()
        .any(|fact| fact.relation == "selected" && fact.args == ["release", "2"]));
}

#[test]
fn rejects_nondeterministic_time_and_mixed_asserted_derived_relations() {
    let time = r#"
spec time {
  relation active { args: [symbol] }
  rule current { derive: "active(Item)" when: ["now(Item)"] }
}
"#;
    let error = walk_artifact(time).expect_err("now requires an artifact clock");
    assert!(error.to_string().contains("deterministic artifact clock"));

    let mixed = r#"
spec mixed {
  relation source { args: [symbol] }
  relation blocked { args: [symbol] }
  fact source_release { relation: source args: [release] }
  fact manual_block { relation: blocked args: [release] }
  rule derived_block { derive: "blocked(Item)" when: ["source(Item)"] }
}
"#;
    let error = walk_artifact(mixed).expect_err("asserted and derived relations are disjoint");
    assert!(
        error
            .to_string()
            .contains("fact `manual_block` asserts relation `blocked`, which is derived by rule `derived_block`"),
        "{error}"
    );
}

#[test]
fn malformed_artifacts_return_diagnostics() {
    for (source, expected) in [
        ("spec broken { /*", "unterminated block comment"),
        (
            "spec broken { relation item { args: [\"",
            "unterminated string",
        ),
        (
            "spec broken { relation item { args: [symbol] args: [symbol] } }",
            "duplicate field `args`",
        ),
        (
            "spec broken { relation item { args: [[symbol]] } }",
            "nested lists are not supported",
        ),
    ] {
        let error = parse_artifact(source).expect_err("malformed artifact must fail");
        assert!(error.to_string().contains(expected), "{error}");
        assert!(error.to_string().starts_with("1:"), "{error}");
    }
}

#[test]
fn failed_expectation_marks_walk_incomplete() {
    let source = EXAMPLE.replace("count: 1", "count: 0");
    let report = walk_artifact(&source).expect("walk incomplete example");

    assert_eq!(report.status, "incomplete");
    assert_eq!(report.expectations[0].actual_count, 1);
    assert!(!report.expectations[0].satisfied);
}

const MUTATION_ANALYSIS: &str = include_str!("../examples/mutation_analysis.stele");

#[test]
fn comments_document_the_spec_and_its_declarations() {
    let source = r#"
// Which releases are blocked?
//
// A release is blocked while any dependency is incomplete.

spec docs {
  // ======================================================= schema

  // A dependency between two named deliverables.
  relation depends_on { args: [symbol, symbol] } // release, dependency
  relation incomplete { args: [symbol] }
  relation blocked { args: [symbol] }

  # Observed in the tracker on release day.
  # Still true after the freeze.
  fact release_needs_tests { relation: depends_on args: [release, tests] }
  fact tests_are_incomplete { relation: incomplete args: [tests] }

  /* Blocking propagates
   * through any dependency. */
  rule blocked_by_incomplete_dependency {
    derive: "blocked(Release)"
    when: ["depends_on(Release, Dependency)", "incomplete(Dependency)"]
  }

  // ------------------------------------------------ acceptance claims
  expect release_is_blocked { query: "blocked(release)" count: 1 }
}
"#;
    let artifact = parse_artifact(source).expect("parse documented artifact");

    assert_eq!(
        artifact.doc.as_deref(),
        Some("Which releases are blocked?\n\nA release is blocked while any dependency is incomplete.")
    );
    let relation = |name: &str| {
        artifact
            .relations
            .iter()
            .find(|relation| relation.name == name)
            .expect("relation")
    };
    assert_eq!(
        relation("depends_on").doc.as_deref(),
        Some("A dependency between two named deliverables.\nrelease, dependency")
    );
    assert_eq!(
        relation("incomplete").doc,
        None,
        "section headings belong to nobody"
    );
    assert_eq!(
        artifact.facts[0].doc.as_deref(),
        Some("Observed in the tracker on release day.\nStill true after the freeze.")
    );
    assert_eq!(artifact.facts[1].doc, None);
    assert_eq!(
        artifact.rules[0].doc.as_deref(),
        Some("Blocking propagates\nthrough any dependency.")
    );
    assert_eq!(
        artifact.expectations[0].doc, None,
        "a ruled divider touching a declaration is still a heading"
    );
}

#[test]
fn existing_examples_keep_their_authored_prose() {
    let artifact = parse_artifact(MUTATION_ANALYSIS).expect("parse mutation analysis");

    assert!(artifact.doc.as_deref().is_some_and(
        |doc| doc.starts_with("Executable model of LemmaSpec mutation-analysis semantics.")
    ));
    let deferred = artifact
        .facts
        .iter()
        .find(|fact| fact.id == "defer_comparison_flip")
        .expect("deferred operator fact");
    assert!(deferred
        .doc
        .as_deref()
        .is_some_and(|doc| doc.starts_with("These need richer term mutation")));
    let oracle = artifact
        .relations
        .iter()
        .find(|relation| relation.name == "oracle")
        .expect("oracle relation");
    assert_eq!(oracle.roles, ["policy", "mode"]);
}

#[test]
fn relations_read_as_sentences_through_roles() {
    let source = r#"
spec roles {
  relation governed_by {
    args: [symbol, symbol]
    roles: [mutation, policy]
    reads: "{mutation} is governed by {policy}"
  }
  fact m { relation: governed_by args: [rule_deletion_caught, rule_coverage] }
  expect one { query: "governed_by(M, P)" count: 1 }
}
"#;
    let artifact = parse_artifact(source).expect("parse roles");
    assert_eq!(artifact.relations[0].roles, ["mutation", "policy"]);
    assert_eq!(
        artifact.relations[0].reads.as_deref(),
        Some("{mutation} is governed by {policy}")
    );

    let rejects = |replacement: &str, message: &str| {
        let broken = source.replace("roles: [mutation, policy]", replacement);
        let error = parse_artifact(&broken).expect_err(message).to_string();
        assert!(error.contains(message), "{error}");
    };
    rejects("roles: [mutation]", "declares 1 roles for 2 arguments");
    rejects("roles: [mutation, mutation]", "repeats role `mutation`");
    rejects("roles: [mutation, \"a policy\"]", "must be an identifier");

    let unknown = source.replace("{policy}", "{oracle}");
    let error = parse_artifact(&unknown)
        .expect_err("unknown placeholder")
        .to_string();
    assert!(
        error.contains("`{oracle}` names no role or argument position"),
        "{error}"
    );

    let unclosed = source.replace("{policy}", "{policy");
    let error = parse_artifact(&unclosed)
        .expect_err("unclosed placeholder")
        .to_string();
    assert!(error.contains("unclosed `{`"), "{error}");

    let positional = source.replace("roles: [mutation, policy]\n", "").replace(
        "{mutation} is governed by {policy}",
        "{0} is governed by {1}",
    );
    parse_artifact(&positional).expect("positional placeholders need no roles");
}

#[test]
fn parsed_artifact_entrypoints_match_source_entrypoints() {
    let parsed = parse_artifact(MUTATION_ANALYSIS).expect("parse mutation analysis");

    assert_eq!(
        lemmaspec::walk_parsed_artifact(&parsed).expect("walk parsed"),
        walk_artifact(MUTATION_ANALYSIS).expect("walk source"),
    );
    assert_eq!(
        lemmaspec::mutate_parsed_artifact(&parsed).expect("mutate parsed"),
        lemmaspec::mutate_artifact(MUTATION_ANALYSIS).expect("mutate source"),
    );
    assert_eq!(
        lemmaspec::project_parsed_artifact(&parsed).expect("project parsed"),
        lemmaspec::project_artifact(MUTATION_ANALYSIS).expect("project source"),
    );

    let checker_source = r#"
        spec checker {
          relation input { args: [symbol] }
          relation flagged { args: [symbol] }
          fact fixture { relation: input args: [fixture] }
          rule flag_every_input { derive: "flagged(X)" when: ["input(X)"] }
          expect fixture_is_flagged { query: "flagged(fixture)" count: 1 }
        }
    "#;
    let evidence_source = r#"
        spec evidence {
          fact real { relation: input args: [real] }
          expect real_is_flagged { query: "flagged(real)" count: 1 }
        }
    "#;
    let checker = parse_artifact(checker_source).expect("parse checker");
    let evidence = parse_artifact(evidence_source).expect("parse evidence");

    assert_eq!(
        lemmaspec::check_parsed_artifact(&checker, &evidence).expect("check parsed"),
        lemmaspec::check_artifact(checker_source, evidence_source).expect("check source"),
    );
    let bound = lemmaspec::bind_parsed_artifact(&checker, &evidence).expect("bind parsed");
    assert_eq!(
        lemmaspec::print_artifact(&bound),
        lemmaspec::bind_artifact(checker_source, evidence_source).expect("bind source"),
    );
}

#[test]
fn integer_overflow_in_linear_expression_does_not_wrap() {
    let source = format!(
        r#"
spec overflow {{
  relation num {{ args: [integer] }}
  relation next {{ args: [integer] }}
  fact max_val {{ relation: num args: [{}] }}
  rule step {{
    derive: "next(Y)"
    when: ["num(X)", "Y = X + 1"]
  }}
  expect no_wrapped_fact {{ query: "next(Y)" count: 0 }}
}}
"#,
        i64::MAX
    );
    let report = walk_artifact(&source).expect("overflow must not panic or wrap");
    assert_eq!(report.status, "clean");
    assert_eq!(report.derived, 0);
}

#[test]
fn inequality_holds_against_a_symbol_no_fact_mentions() {
    let artifact = r#"
spec unseen_symbol {
  relation item { args: [symbol] }
  relation not_ghost { args: [symbol] }
  relation is_ghost { args: [symbol] }
  fact alpha { relation: item args: [alpha] }
  fact beta { relation: item args: [beta] }
  rule differs_from_ghost { derive: "not_ghost(X)" when: ["item(X)", "X \\= ghost"] }
  rule equals_ghost { derive: "is_ghost(X)" when: ["item(X)", "X = ghost"] }
  expect both_items_differ_from_ghost { query: "not_ghost(X)" count: 2 }
  expect no_item_equals_ghost { query: "is_ghost(X)" count: 0 }
}
"#;
    let report = walk_artifact(artifact).expect("walk artifact");

    assert_eq!(report.status, "clean", "{:#?}", report.expectations);
}
