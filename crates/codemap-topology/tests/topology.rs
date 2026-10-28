use codemap_graph::{CallSiteCategory, Snapshot};
use codemap_topology::{project, render_html};

fn snapshot(language: &str) -> Snapshot {
    serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/tests/fixtures/{language}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn accepted_language_snapshots_preserve_only_proven_calls_and_annotations() {
    for language in ["rust", "ruby", "php", "go"] {
        let source = snapshot(language);
        let topology = project(&source, None).unwrap();
        let value = serde_json::to_value(&topology).unwrap();
        assert_eq!(value["edges"].as_array().unwrap().len(), 1);
        assert_eq!(value["edges"][0]["kind"], "call");
        assert_eq!(
            value["edges"][0]["codemap"]["provenance"]["confidence"],
            255
        );
        assert_eq!(
            value["codemap"]["identity"],
            serde_json::to_value(&source.identity).unwrap()
        );
        assert_eq!(value["deadEnds"], serde_json::json!([]));
        assert!(value.get("flows").is_none());
        assert!(value.get("observations").is_none());
        let json = serde_json::to_string(&topology).unwrap();
        assert!(json.contains("Candidate"));
        assert!(!json.contains("\"dispatch\""));
        assert_eq!(
            json,
            serde_json::to_string(&project(&source, None).unwrap()).unwrap()
        );
        let mut leaves = Vec::new();
        leaf_ids(&value["root"], &mut leaves);
        for edge in value["edges"].as_array().unwrap() {
            assert!(leaves.contains(&edge["source"].as_str().unwrap()));
            assert!(leaves.contains(&edge["target"].as_str().unwrap()));
        }
    }
}

fn leaf_ids<'a>(node: &'a serde_json::Value, ids: &mut Vec<&'a str>) {
    if let Some(children) = node["children"].as_array() {
        for child in children {
            leaf_ids(child, ids);
        }
    } else {
        ids.push(node["id"].as_str().unwrap());
    }
}

#[test]
fn malicious_source_names_cannot_close_the_data_script() {
    let mut source = snapshot("rust");
    source.nodes[1].name = "</script><script>alert('&')</script>".into();
    let topology = project(&source, None).unwrap();
    let html = render_html(&topology).unwrap();
    assert!(!html.contains("</script><script>alert"));
    assert!(html.contains("\\u003c/script\\u003e"));
    assert!(html.contains("\\u0026"));
    assert_eq!(html, render_html(&topology).unwrap());
}

#[test]
fn malformed_snapshots_are_rejected_before_projection() {
    let mut source = snapshot("rust");
    source.call_sites[0].category = CallSiteCategory::Candidate;
    assert!(project(&source, None).is_err());
}

fn fixture_value(language: &str, suffix: &str) -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/tests/fixtures/{language}-{suffix}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap()
}

fn checked(
    source: &Snapshot,
    report: serde_json::Value,
    selection: serde_json::Value,
) -> anyhow::Result<codemap_topology::Topology> {
    project(
        source,
        Some((
            &serde_json::from_value(report)?,
            &serde_json::from_value(selection)?,
        )),
    )
}

#[test]
fn admitted_incomplete_reports_have_qualified_witnesses_in_all_accepted_languages() {
    for language in ["rust", "ruby", "php", "go"] {
        let report = fixture_value(language, "check");
        assert_eq!(report["status"], "incomplete");
        let topology = checked(
            &snapshot(language),
            report,
            fixture_value(language, "selection"),
        )
        .unwrap();
        assert_eq!(topology.dead_ends.len(), 1);
        let absence = &topology.codemap.absence[&topology.dead_ends[0]];
        assert!(
            absence
                .qualification
                .starts_with("no path in the observed graph under entry-point policy ")
        );
        assert!(absence.witness.contains("shipped_without_observed_reach"));
        assert!(absence.admission_witness.contains("coverage_input("));
    }
}

#[test]
fn mismatched_or_missing_admission_and_forged_findings_fail_closed() {
    let source = snapshot("rust");
    let original = fixture_value("rust", "check");
    let selection = fixture_value("rust", "selection");
    let mutate_fact = |relation: &str, field: &str, value: serde_json::Value| {
        let mut report = original.clone();
        let fact = report["facts"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|f| f["relation"] == relation)
            .unwrap();
        fact[field] = value;
        assert!(
            checked(&source, report, selection.clone()).is_err(),
            "{relation}.{field}"
        );
    };
    for index in 0..5 {
        let mut report = original.clone();
        let fact = report["facts"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|f| f["relation"] == "snapshot_identity")
            .unwrap();
        fact["args"][index] = "other".into();
        assert!(checked(&source, report, selection.clone()).is_err());
    }
    mutate_fact("coverage_admitted", "origin", "asserted".into());
    mutate_fact("coverage_admitted", "why", "invented".into());
    mutate_fact(
        "coverage_input",
        "args",
        serde_json::json!(["policy:entry-point/rust", "2", "500", "0", "1", "0", "0"]),
    );
    mutate_fact(
        "call_census",
        "args",
        serde_json::json!(["Ambiguous", "999"]),
    );
    mutate_fact("unused_production_symbol", "origin", "asserted".into());
    mutate_fact("unused_production_symbol", "why", "invented".into());
    mutate_fact("calls", "args", serde_json::json!(["invented", "invented"]));
    for relation in [
        "coverage_input",
        "coverage_admitted",
        "call_census",
        "defines",
        "production_file",
    ] {
        let mut report = original.clone();
        report["facts"]
            .as_array_mut()
            .unwrap()
            .retain(|f| f["relation"] != relation);
        assert!(
            checked(&source, report, selection.clone()).is_err(),
            "missing {relation}"
        );
    }
    let mut report = original.clone();
    report["expectations"]
        .as_array_mut()
        .unwrap()
        .retain(|e| e["id"] != "coverage_is_admitted");
    assert!(checked(&source, report, selection.clone()).is_err());
    let mut report = original.clone();
    report["expectations"][0]["found"] = serde_json::json!(["P=other"]);
    assert!(checked(&source, report, selection.clone()).is_err());
    let mut report = original.clone();
    report["expectations"][0]["satisfied"] = false.into();
    assert!(checked(&source, report, selection.clone()).is_err());
    // A valid-looking witness cannot make the actual entry point absent.
    let mut forged = serde_json::to_string(&original).unwrap();
    forged = forged.replace("#unused", "#entry");
    let mut report: serde_json::Value = serde_json::from_str(&forged).unwrap();
    let facts = report["facts"].as_array_mut().unwrap();
    let mut seen = std::collections::BTreeSet::new();
    facts.retain(|fact| seen.insert(format!("{}{}", fact["relation"], fact["args"])));
    assert!(checked(&source, report, selection).is_err());
}

#[test]
fn expected_selections_bind_actual_entry_and_public_sets() {
    let source = snapshot("rust");
    for field in ["entry_points", "public_api"] {
        for replacement in [
            serde_json::json!(["unknown"]),
            serde_json::json!([source.nodes[2].id, source.nodes[2].id]),
        ] {
            let mut selection = fixture_value("rust", "selection");
            selection[field] = replacement;
            assert!(checked(&source, fixture_value("rust", "check"), selection).is_err());
        }
    }
    let mut selection = fixture_value("rust", "selection");
    selection["entry_points"] = serde_json::json!([]);
    assert!(checked(&source, fixture_value("rust", "check"), selection).is_err());
}

#[test]
fn source_links_reject_active_schemes_and_browser_normalization() {
    for link in [
        "javascript:alert(1)",
        "JaVaScRiPt:alert(1)",
        "data:text/html,x",
        "//evil.test",
        "\\evil.test",
        "/absolute",
        "https:///evil.test",
        "https://",
        "\njavascript:x",
        " java\nscript:x",
    ] {
        assert!(!codemap_topology::safe_link(link), "{link}");
    }
    for link in [
        "src/main.rs#L1-L4",
        "../src/main.rs#L1",
        "#node",
        "https://example.test/src/main.rs#L1",
    ] {
        assert!(codemap_topology::safe_link(link), "{link}");
    }
}

#[test]
fn unresolved_sites_belong_only_to_unique_innermost_callables() {
    let mut source = snapshot("rust");
    let mut nested = source.nodes[2].clone();
    nested.id.push_str("::nested");
    nested.symbol_key.push_str("::nested");
    nested.name = "nested".into();
    nested.provenance.span.start_byte = 45;
    nested.provenance.span.end_byte = 65;
    nested.provenance.span.start_line = 5;
    nested.provenance.span.end_line = 7;
    source.nodes.push(nested.clone());
    *source.report.node_counts.get_mut("function").unwrap() += 1;
    let topology = project(&source, None).unwrap();
    let owner = annotated_nodes(&topology.root);
    assert_eq!(owner, vec![nested.id.clone()]);
    let mut equally_inner = nested.clone();
    equally_inner.id.push_str("_other");
    equally_inner.symbol_key.push_str("_other");
    source.nodes.push(equally_inner);
    *source.report.node_counts.get_mut("function").unwrap() += 1;
    let topology = project(&source, None).unwrap();
    assert_eq!(
        annotated_nodes(&topology.root),
        [] as [std::string::String; 0]
    );
    assert_eq!(topology.codemap.unowned_call_sites.len(), 1);
}

fn annotated_nodes(node: &codemap_topology::TreeNode) -> Vec<String> {
    let mut ids = vec![];
    if node
        .codemap
        .as_ref()
        .is_some_and(|e| !e.unresolved_call_sites.is_empty())
    {
        ids.push(node.id.clone());
    }
    for child in &node.children {
        ids.extend(annotated_nodes(child));
    }
    ids
}

#[test]
fn snapshot_order_does_not_change_topology_bytes() {
    let mut source = snapshot("rust");
    let first = serde_json::to_string(&project(&source, None).unwrap()).unwrap();
    source.nodes.reverse();
    source.edges.reverse();
    source.call_sites.reverse();
    source.files.reverse();
    assert_eq!(
        first,
        serde_json::to_string(&project(&source, None).unwrap()).unwrap()
    );
}
