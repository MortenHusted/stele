mod common;

use codemap_lemmaspec::{Checker, Options, citations};

fn options() -> Options {
    Options {
        checker: Checker::Citations,
        spec_name: Some("rule_review".into()),
        allow_dirty: false,
        min_call_coverage_per_mille: 1000,
        core_files: Vec::new(),
        presentation_files: Vec::new(),
        entry_points: Vec::new(),
        public_api: Vec::new(),
    }
}

fn card(id: &str, source: &str, claim: &str) -> String {
    format!("### {id}: Discount cap\n**Source:** `{source}`\n**Plain English:** {claim}\n")
}

#[test]
fn every_card_retains_separate_location_and_semantic_results() {
    let snapshot = common::rust_snapshot(false);
    let cards = [
        card(
            "RULE-001",
            "src/core.rs:5",
            "The discount is capped at 20 percent.",
        ),
        card(
            "RULE-002",
            "absent.rs:5",
            "The discount is capped at 5 percent.",
        ),
        card("RULE-003", "../src/core.rs:5", "Escaping citation."),
        card("RULE-004", "src/core.rs:999", "Beyond recorded spans."),
        "### RULE-005: Missing source\n**Plain English:** Still retained.\n".into(),
    ]
    .join("\n");
    let text = citations::project(&snapshot, &options(), &cards).unwrap();
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("relation: rule_card "))
            .count(),
        5
    );
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("relation: location_status "))
            .count(),
        5
    );
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("relation: located "))
            .count(),
        1
    );
    assert!(text.contains("args: [\"RULE-001\", 1,"));
    for id in 2..=5 {
        assert!(text.contains(&format!("args: [\"RULE-{id:03}\", 0,")));
    }
    for line in text
        .lines()
        .filter(|line| line.contains("relation: rule_card "))
    {
        assert!(line.contains("basis: reviewer_declared"), "{line}");
    }
    for line in text
        .lines()
        .filter(|line| line.contains("relation: located "))
    {
        assert!(line.contains("basis: snapshot"), "{line}");
        assert!(line.contains(&snapshot.identity.snapshot_id));
    }
    assert!(text.contains("Executable classification: unsupported"));
    assert_eq!(
        text,
        citations::project(&snapshot, &options(), &cards).unwrap()
    );
}

#[test]
fn parser_is_narrow_and_rejects_duplicates_instead_of_overwriting() {
    let snapshot = common::rust_snapshot(false);
    let valid = card("RULE-001", "src/core.rs:5", "Claim.");
    for invalid in [
        valid.repeat(2),
        format!("{valid}**Source:** `src/core.rs:9`\n"),
        format!("{valid}**Plain English:** Other.\n"),
        "### RULE-X: Invalid\n**Plain English:** Claim.\n".into(),
        "### RULE-001: No claim\n".into(),
        "```markdown\n### RULE-001: Example\n**Plain English:** Claim.\n```\n".into(),
    ] {
        assert!(
            citations::project(&snapshot, &options(), &invalid).is_err(),
            "{invalid}"
        );
    }
    let text = citations::project(
        &snapshot,
        &options(),
        &format!("```markdown\n{valid}```\n{valid}"),
    )
    .unwrap();
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("relation: rule_card "))
            .count(),
        1
    );
}

#[test]
fn location_validity_never_certifies_claims_including_comments_and_wrong_thresholds() {
    // Synthetic source represented by the span below. The producer receives no bytes.
    let source = "fn discount() {\n    // cap is 20\n\n    let cap = 5;\n}\n";
    assert!(source.contains("let cap = 5;"));
    let mut snapshot = common::rust_snapshot(false);
    snapshot.nodes.retain(|node| node.id == common::helper_id());
    snapshot.nodes[0].provenance.span =
        codemap_graph::Span::from_ts(0, source.len() - 1, (0, 0), (4, 1));
    snapshot.edges.clear();
    snapshot.call_sites.clear();
    snapshot.files.retain(|file| file.rel_path == "src/core.rs");
    snapshot.report = common::report_for(&snapshot);
    let cards = [
        card("RULE-001", "src/core.rs:2", "The cap is 20."),
        card("RULE-002", "src/core.rs:3", "A blank proves the cap."),
        card("RULE-003", "src/core.rs:4", "The executable cap is 20."),
        card("RULE-004", "absent.rs:4", "No location."),
        card("RULE-005", "src/core.rs:4-1", "Reversed range."),
    ]
    .join("\n");
    let evidence = citations::project(&snapshot, &options(), &cards).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let checker = dir.path().join("citations.lemmaspec");
    let input = dir.path().join("evidence.lemmaspec");
    std::fs::write(&checker, Checker::Citations.source()).unwrap();
    std::fs::write(&input, evidence).unwrap();
    let (code, output) = common::run_lemmaspec(&[
        "check",
        checker.to_str().unwrap(),
        input.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 1, "{output}");
    let report: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(report["status"], "incomplete");
    let claims = report["expectations"].as_array().unwrap();
    let statuses = claims
        .iter()
        .find(|claim| claim["id"] == "location_results_for_every_card")
        .unwrap();
    assert_eq!(statuses["found"].as_array().unwrap().len(), 5, "{report}");
    let unverified = claims
        .iter()
        .find(|claim| claim["id"] == "no_semantically_unverified_claims")
        .unwrap();
    assert_eq!(unverified["found"].as_array().unwrap().len(), 5, "{report}");
    let invalid = claims
        .iter()
        .find(|claim| claim["id"] == "no_invalid_locations")
        .unwrap();
    assert_eq!(invalid["found"].as_array().unwrap().len(), 2, "{report}");
}

#[test]
fn citations_describe_extraction_limits_without_requiring_absence_admission() {
    let snapshot = common::rust_snapshot(false);
    let text = citations::project(
        &snapshot,
        &options(),
        &card("RULE-001", "src/core.rs:5", "Unverified claim."),
    )
    .unwrap();
    for statement in [
        "Extraction scope:",
        "skipped files:",
        "parse errors:",
        "semantic:",
        "Unsupported observations: unknown",
        "excluded-source inventory and reasons: unknown",
        "Entry-point policy:",
        "not applied to declaration containment",
        "Resolved sites / all recognized sites:",
        "An empty census has no coverage ratio and cannot admit absence.",
        "Scope file:",
        "Call-site category Candidate:",
        "Call-site category Resolved:",
    ] {
        assert!(text.contains(statement), "missing {statement}");
    }
    assert!(text.contains("args: [\"RULE-001\", 1,"));
    assert!(!text.contains("query: \"coverage_admitted"));
}
