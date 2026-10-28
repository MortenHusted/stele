use lemmaspec::{mutate_artifact, parse_artifact, print_artifact, project_artifact, walk_artifact};

const BLOCKED: &str = include_str!("fixtures/readability/blocked_dependency.lemmaspec");
const ACCEPTED: &str = include_str!("fixtures/readability/accepted_plan.lemmaspec");
const FAILED: &str = include_str!("fixtures/readability/failed_admission.lemmaspec");

fn with_metadata(source: &str, declarations: &str) -> String {
    let index = source.find('{').unwrap() + 1;
    format!("{}\n{declarations}\n{}", &source[..index], &source[index..])
}

#[test]
fn synthetic_reports_keep_the_existing_status_and_proof_contract() {
    for (source, status, relation, symbol) in [
        (BLOCKED, "incomplete", "blocked", "publish"),
        (ACCEPTED, "clean", "accepted", "plan"),
        (FAILED, "incomplete", "rejected", "change"),
    ] {
        let report = walk_artifact(source).unwrap();
        assert_eq!(report.status, status);
        assert_eq!(report.expectations[0].actual_count, 1);
        assert!(report
            .facts
            .iter()
            .any(|fact| fact.relation == relation && fact.args == [symbol]));
        let projection = project_artifact(source).unwrap();
        projection.validate_closed().unwrap();
        assert!(projection.edges.iter().any(|edge| edge.witness.is_some()));
    }
}

#[test]
fn labels_and_notes_do_not_change_walk_mutation_or_proof_identity() {
    let labelled = with_metadata(
        ACCEPTED,
        r#"symbol plan { label: "Verification plan" source: "plan.md#verification" }
           notes { text: "Update the verification evidence when the plan changes." }"#,
    );
    assert_eq!(
        serde_json::to_vec(&walk_artifact(ACCEPTED).unwrap()).unwrap(),
        serde_json::to_vec(&walk_artifact(&labelled).unwrap()).unwrap()
    );
    assert_eq!(
        serde_json::to_vec(&mutate_artifact(ACCEPTED).unwrap()).unwrap(),
        serde_json::to_vec(&mutate_artifact(&labelled).unwrap()).unwrap()
    );
    let plain = project_artifact(ACCEPTED).unwrap();
    let decorated = project_artifact(&labelled).unwrap();
    assert_eq!(plain.edges, decorated.edges);
    assert_eq!(
        plain.nodes.iter().map(|node| &node.id).collect::<Vec<_>>(),
        decorated
            .nodes
            .iter()
            .map(|node| &node.id)
            .collect::<Vec<_>>()
    );
    let mut decorated_json = serde_json::to_value(&decorated).unwrap();
    for node in decorated_json["nodes"].as_array_mut().unwrap() {
        let node = node.as_object_mut().unwrap();
        for field in ["label", "source", "notes"] {
            node.remove(field);
        }
    }
    assert_eq!(serde_json::to_value(&plain).unwrap(), decorated_json);
    let artifact = parse_artifact(&labelled).unwrap();
    let printed = print_artifact(&artifact);
    assert_eq!(parse_artifact(&printed).unwrap(), artifact);
    assert_eq!(print_artifact(&parse_artifact(&printed).unwrap()), printed);
}

#[test]
fn labels_accept_quoted_keys_forward_references_and_repeated_display_text() {
    let source = r#"
        // What was verified?
        spec labels {
          notes { text: "Recheck after edits.\nKeep the evidence current." }
          symbol "module::verify" { label: "Verify calls" source: "src/verify.rs#L10" }
          symbol simple { label: "Verify calls" }
          symbol rule_only { label: "Rule constant" }
          symbol comparison_only { label: "Comparison constant" }
          symbol query_only { label: "Query constant" }
          relation item { args: [symbol] }
          relation result { args: [symbol] }
          fact first { relation: item args: ["module::verify"] }
          fact second { relation: item args: [simple] }
          rule selected {
            derive: "result(rule_only)"
            when: ["item(Item)", "Item \\= comparison_only"]
          }
          expect absent { query: "result(query_only)" count: 0 }
        }
    "#;
    let artifact = parse_artifact(source).unwrap();
    assert_eq!(artifact.symbols.len(), 5);
    assert_eq!(artifact.doc.as_deref(), Some("What was verified?"));
    assert_eq!(
        artifact.notes.as_deref(),
        Some("Recheck after edits.\nKeep the evidence current.")
    );
    assert_eq!(
        parse_artifact(&print_artifact(&artifact)).unwrap(),
        artifact
    );
    let projection = project_artifact(source).unwrap();
    let serialized = serde_json::to_value(projection).unwrap();
    let nodes = serialized["nodes"].as_array().unwrap();
    let symbol = nodes
        .iter()
        .find(|node| node["value"] == "module::verify")
        .unwrap();
    assert_eq!(symbol["label"], "Verify calls");
    assert_eq!(symbol["source"], "src/verify.rs#L10");
    assert!(nodes
        .iter()
        .any(|node| node["notes"] == "Recheck after edits.\nKeep the evidence current."));
}

#[test]
fn duplicate_or_unknown_symbol_metadata_is_rejected() {
    for (declarations, message) in [
        (
            r#"symbol plan { label: "First" } symbol "plan" { label: "Second" }"#,
            "duplicate symbol `plan`",
        ),
        (
            r#"symbol missing { label: "Unknown" }"#,
            "labels an unknown symbol",
        ),
        (
            r#"notes { text: "First" } notes { text: "Second" }"#,
            "duplicate notes",
        ),
    ] {
        let error = parse_artifact(&with_metadata(ACCEPTED, declarations)).unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
}

#[test]
fn arbitrary_symbol_keys_and_metadata_escape_losslessly() {
    let source = r#"
        spec escaped {
          // The unusual key is still the same symbol.
          symbol "a \"quote\"\\path\nnext" { label: "Verify <calls> & \"returns\"" source: "docs/notes%20page.md" }
          relation item { args: [symbol] }
          fact item { relation: item args: ["a \"quote\"\\path\nnext"] }
        }
    "#;
    let artifact = parse_artifact(source).unwrap();
    assert_eq!(artifact.symbols[0].value, "a \"quote\"\\path\nnext");
    assert_eq!(
        parse_artifact(&print_artifact(&artifact)).unwrap(),
        artifact
    );
    let projection = project_artifact(source).unwrap();
    assert!(projection.nodes.iter().any(|node| matches!(
        &node.data,
        lemmaspec::GraphNodeData::Symbol { doc: Some(doc), .. }
            if doc == "The unusual key is still the same symbol."
    )));
}

#[test]
fn metadata_does_not_reject_a_mutant_that_removes_its_only_symbol_reference() {
    let source = r#"
        spec singleton {
          symbol unique { label: "The only item" }
          relation item { args: [symbol] }
          fact only { relation: item args: [unique] }
          expect one { query: "item(Item)" count: 1 }
          mutation remove { operator: drop_fact relation: item }
        }
    "#;
    let report = mutate_artifact(source).unwrap();
    assert_eq!(report.summary.killed, 1);
    assert_eq!(report.summary.rejected, 0);
}

#[test]
fn binding_preserves_surviving_checker_metadata_and_evidence_overrides() {
    let checker = r#"
        // Checker question.
        spec checker {
          notes { text: "Keep the rule current." }
          symbol fixture { label: "Fixture only" }
          symbol ready { label: "Ready result" source: "rules.md#ready" }
          symbol shared { label: "Checker label" }
          relation input { args: [symbol] }
          relation result { args: [symbol] }
          fact fixture { relation: input args: [fixture] }
          rule admit { derive: "result(ready)" when: ["input(Item)", "Item \\= shared"] }
          expect ready { query: "result(ready)" count: 1 }
        }
    "#;
    let evidence = r#"
        // Is this evidence admitted?
        spec evidence {
          notes { text: "Refresh the observations." }
          symbol observed { label: "Observed input" source: "evidence.md" }
          symbol shared { label: "Evidence label" }
          fact observation { relation: input args: [observed] }
          expect ready { query: "result(ready)" count: 1 }
          expect absent { query: "input(shared)" count: 0 }
        }
    "#;
    let bound = lemmaspec::bind_artifact(checker, evidence).unwrap();
    let artifact = parse_artifact(&bound).unwrap();
    assert_eq!(
        artifact.notes.as_deref(),
        Some("Keep the rule current.\n\nRefresh the observations.")
    );
    assert!(artifact
        .doc
        .as_deref()
        .unwrap()
        .ends_with("Is this evidence admitted?"));
    assert_eq!(
        artifact
            .symbols
            .iter()
            .map(|symbol| (symbol.value.as_str(), symbol.label.as_str()))
            .collect::<Vec<_>>(),
        [
            ("observed", "Observed input"),
            ("ready", "Ready result"),
            ("shared", "Evidence label")
        ]
    );
    assert_eq!(
        lemmaspec::check_artifact(checker, evidence).unwrap(),
        walk_artifact(&bound).unwrap()
    );
    let mut plain_checker = parse_artifact(checker).unwrap();
    let mut plain_evidence = parse_artifact(evidence).unwrap();
    plain_checker.symbols.clear();
    plain_checker.notes = None;
    plain_evidence.symbols.clear();
    plain_evidence.notes = None;
    assert_eq!(
        serde_json::to_vec(&lemmaspec::check_artifact(checker, evidence).unwrap()).unwrap(),
        serde_json::to_vec(
            &lemmaspec::check_artifact(
                &print_artifact(&plain_checker),
                &print_artifact(&plain_evidence)
            )
            .unwrap()
        )
        .unwrap()
    );
}

#[test]
fn source_policy_accepts_only_unambiguous_relative_fragment_or_https_destinations() {
    for source in [
        "plan.md#verify",
        "./docs/plan.md",
        "docs/plan%20notes.md",
        "#verify",
        "https://example.org/docs#verify",
        "HTTPS://example.org/docs",
    ] {
        assert!(lemmaspec::is_safe_source(source), "{source}");
    }
    for source in [
        "",
        "javascript:alert(1)",
        "JaVaScRiPt:alert(1)",
        "data:text/html,hello",
        "file:///tmp/plan",
        "http://example.org",
        "//example.org",
        "/plan.md",
        "../plan.md",
        "docs/../plan.md",
        "https://",
        "https:///path",
        "https://?query",
        "\\example.org",
        "docs\\plan.md",
        " plan.md",
        "plan\n.md",
        "plan\t.md",
        "%2f%2fexample.org",
        "docs/%2e%2e/plan.md",
        "java%73cript:alert(1)",
        "docs/%5cplan.md",
        "plan%0a.md",
        "plan%",
        "plan%xy",
    ] {
        assert!(!lemmaspec::is_safe_source(source), "{source:?}");
    }
}

#[test]
fn unsafe_source_is_preserved_as_metadata_without_affecting_evaluation() {
    let source = with_metadata(
        ACCEPTED,
        r#"symbol plan { label: "Plan <review>" source: "javascript:alert(1)" }"#,
    );
    let artifact = parse_artifact(&source).unwrap();
    let destination = artifact.symbols[0].source.as_deref().unwrap();
    assert!(!lemmaspec::is_safe_source(destination));
    assert_eq!(
        parse_artifact(&print_artifact(&artifact)).unwrap(),
        artifact
    );
    assert_eq!(
        walk_artifact(&source).unwrap(),
        walk_artifact(ACCEPTED).unwrap()
    );
    assert!(project_artifact(&source).is_ok());
}

#[test]
fn artifacts_without_metadata_keep_their_serialized_shape() {
    let artifact = parse_artifact(ACCEPTED).unwrap();
    assert!(artifact.symbols.is_empty());
    assert!(artifact.notes.is_none());
    let json = serde_json::to_value(&artifact).unwrap();
    assert!(json.get("symbols").is_none());
    assert!(json.get("notes").is_none());
    let projection = serde_json::to_value(project_artifact(ACCEPTED).unwrap()).unwrap();
    for node in projection["nodes"].as_array().unwrap() {
        assert!(node.get("label").is_none());
        assert!(node.get("source").is_none());
        assert!(node.get("notes").is_none());
    }
}
