//! Positive consumer proofs; these handbuilt graphs test projection, not extraction.

mod common;

use codemap_graph::schema::{
    CallSite, CallSiteCategory, Edge, EdgeKind, FactSource, NodeKind, Snapshot, Span, edge_id,
};
use codemap_lemmaspec::positive_path::{Options, project};

#[test]
fn exact_selections_and_snapshot_provenance_are_byte_stable() {
    let (snapshot, options, decoy) = common::positive_path_fixture();
    let first = project(&snapshot, &options).unwrap();
    assert_eq!(first, project(&snapshot, &options).unwrap());
    assert!(first.contains(&format!(
        "relation: entry_point args: [\"{}\"] basis: reviewer_declared",
        options.entry_node
    )));
    assert!(first.contains(&format!(
        "relation: selected_target args: [\"{}\"] basis: reviewer_declared",
        options.target_node
    )));
    assert!(!first.contains(&format!("relation: selected_target args: [\"{decoy}\"]")));
    assert!(first.contains("basis: snapshot"));
    assert!(first.contains(&snapshot.identity.snapshot_id));
    assert!(first.contains("src/main.cpp:1"));
    let mut wrong = options;
    wrong.target_node = snapshot.nodes[2].symbol_key.clone();
    assert!(
        project(&snapshot, &wrong)
            .unwrap_err()
            .to_string()
            .contains("exact callable node ID")
    );
}

#[test]
fn invalid_snapshot_modes_and_selectors_are_rejected() {
    let (snapshot, options, _) = common::positive_path_fixture();
    let mut syntax = snapshot.clone();
    syntax.edges.clear();
    syntax.call_sites.clear();
    syntax.identity.cpp_semantic = None;
    syntax.identity.snapshot_id = syntax.identity.compute_snapshot_id();
    syntax.report = common::report_for(&syntax);
    assert!(
        project(&syntax, &options)
            .unwrap_err()
            .to_string()
            .contains("semantic C++")
    );
    let mut dirty = snapshot.clone();
    dirty.identity.dirty = true;
    dirty.identity.snapshot_id = dirty.identity.compute_snapshot_id();
    assert!(
        project(&dirty, &options)
            .unwrap_err()
            .to_string()
            .contains("--allow-dirty")
    );
    project(
        &dirty,
        &Options {
            allow_dirty: true,
            ..options.clone()
        },
    )
    .unwrap();
    assert!(
        project(&common::rust_snapshot(false), &options)
            .unwrap_err()
            .to_string()
            .contains("semantic C++")
    );
    let mut noncallable = snapshot.clone();
    let mut file_node = snapshot.nodes[0].clone();
    file_node.kind = NodeKind::File;
    file_node.id = format!("cpp:file:{}", file_node.provenance.rel_path);
    file_node.symbol_key = format!("cpp:{}", file_node.provenance.rel_path);
    file_node.name = file_node.provenance.rel_path.clone();
    file_node.provenance.span = Span::from_ts(0, snapshot.files[0].byte_len, (0, 0), (99, 0));
    let file_options = Options {
        entry_node: file_node.id.clone(),
        ..options.clone()
    };
    noncallable.nodes.push(file_node);
    noncallable.report = common::report_for(&noncallable);
    codemap_graph::validate(&noncallable).unwrap();
    assert!(
        project(&noncallable, &file_options)
            .unwrap_err()
            .to_string()
            .contains("exact callable node ID")
    );
    let mut missing = options;
    missing.entry_node = "cpp:function:absent.cpp#missing()".into();
    assert!(
        project(&snapshot, &missing)
            .unwrap_err()
            .to_string()
            .contains("exact callable node ID")
    );
}

#[test]
fn actual_engine_proves_the_selected_path_and_detects_mutations() {
    let (snapshot, options, decoy) = common::positive_path_fixture();
    let dir = tempfile::tempdir().unwrap();
    let artifact = dir.path().join("positive.lemmaspec");
    let evidence = project(&snapshot, &options).unwrap();
    let mut mutations = evidence;
    let end = mutations.rfind('}').unwrap();
    mutations.insert_str(end, "\n  mutation dropped_call { operator: drop_fact relation: calls must_fail: selected_path_is_proven }\n  mutation dropped_rule { operator: drop_rule must_fail: selected_path_is_proven }\n");
    std::fs::write(&artifact, mutations).unwrap();
    for command in ["walk", "mutate"] {
        let (code, report) =
            common::run_lemmaspec(&[command, artifact.to_str().unwrap(), "--json"]);
        assert_eq!(code, 0, "{command}: {report}");
    }
    let unproved = project(
        &snapshot,
        &Options {
            target_node: decoy,
            ..options
        },
    )
    .unwrap();
    std::fs::write(&artifact, unproved).unwrap();
    let (code, _) = common::run_lemmaspec(&["walk", artifact.to_str().unwrap(), "--json"]);
    assert_eq!(
        code, 1,
        "same symbol key must not select the unreachable decoy"
    );
}

#[test]
#[ignore = "requires exported guarded C++ fixture (CODEMAP_CPP_CANONICAL_SNAPSHOT)"]
fn guarded_cpp_fixture_projects_the_independent_oracle_calls() {
    let path = std::env::var_os("CODEMAP_CPP_CANONICAL_SNAPSHOT")
        .expect("export the guarded canonical fixture first");
    let mut snapshot: Snapshot = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    codemap_graph::validate(&snapshot).unwrap();
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../codemap/tests/fixtures/cpp_calls/expected.json"
    ))
    .unwrap();
    let positives = oracle["canonical"].as_array().unwrap();
    assert_eq!(
        snapshot
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Calls)
            .count(),
        positives.len()
    );
    let dir = tempfile::tempdir().unwrap();
    let artifact = dir.path().join("guarded.lemmaspec");
    let mut selections = Vec::new();
    for label in positives {
        let caller_key = format!("#{}(", label["caller"].as_str().unwrap());
        let target_key = format!("#{}", label["callee"].as_str().unwrap());
        let callers: Vec<_> = snapshot
            .nodes
            .iter()
            .filter(|node| node.symbol_key.contains(&caller_key))
            .collect();
        let targets: Vec<_> = snapshot
            .nodes
            .iter()
            .filter(|node| node.symbol_key.ends_with(&target_key))
            .collect();
        assert_eq!(callers.len(), 1);
        assert_eq!(targets.len(), 1);
        let options = Options {
            entry_node: callers[0].id.clone(),
            target_node: targets[0].id.clone(),
            allow_dirty: false,
        };
        assert!(
            snapshot
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::Calls
                    && edge.from == options.entry_node
                    && edge.to == options.target_node
                    && edge.provenance.fact_source == FactSource::Clangd)
        );
        let evidence = project(&snapshot, &options).unwrap();
        assert!(evidence.contains("basis: snapshot"));
        std::fs::write(&artifact, evidence).unwrap();
        let (code, report) = common::run_lemmaspec(&["walk", artifact.to_str().unwrap(), "--json"]);
        assert_eq!(code, 0, "{report}");
        selections.push(options);
    }
    snapshot.edges.retain(|edge| edge.kind != EdgeKind::Calls);
    for site in &mut snapshot.call_sites {
        if site.category == CallSiteCategory::Resolved {
            site.category = CallSiteCategory::Unmapped;
        }
    }
    snapshot.report = common::report_for(&snapshot);
    for options in selections {
        std::fs::write(&artifact, project(&snapshot, &options).unwrap()).unwrap();
        let (code, report) = common::run_lemmaspec(&["walk", artifact.to_str().unwrap(), "--json"]);
        assert_eq!(
            code, 1,
            "removing canonical calls must remove the positive proof: {report}"
        );
    }
}

#[test]
fn js_ts_positive_paths_preserve_exact_selections_and_reject_syntax() {
    use codemap_graph::schema::{JS_TS_SEMANTIC_POLICY, JsTsSemanticInputs};
    let mut snapshot = common::js_ts_snapshot();
    let options = Options {
        entry_node: snapshot.nodes[0].id.clone(),
        target_node: snapshot.nodes[1].id.clone(),
        allow_dirty: false,
    };
    assert!(project(&snapshot, &options).is_err());
    snapshot.identity = snapshot.identity.with_js_ts_semantic(JsTsSemanticInputs {
        project: "tsconfig.json".into(),
        inputs_sha256: "a".repeat(64),
        runtime_sha256: "b".repeat(64),
        compiler_sha256: "c".repeat(64),
        helper_sha256: "d".repeat(64),
        evidence_sha256: "e".repeat(64),
        node_version: "22.22.2".into(),
        typescript_version: "5.9.3".into(),
        policy: JS_TS_SEMANTIC_POLICY.into(),
    });
    let mut provenance = snapshot.nodes[0].provenance.clone();
    provenance.span = Span::from_ts(45, 48, (0, 5), (0, 8));
    provenance.fact_source = FactSource::TypeScript;
    snapshot.call_sites.push(CallSite {
        rel_path: provenance.rel_path.clone(),
        span: provenance.span,
        raw_name: "unused".into(),
        category: CallSiteCategory::Resolved,
    });
    snapshot.edges.push(Edge {
        id: edge_id(
            EdgeKind::Calls,
            &options.entry_node,
            &options.target_node,
            45,
        ),
        kind: EdgeKind::Calls,
        from: options.entry_node.clone(),
        to: options.target_node.clone(),
        provenance,
    });
    snapshot.report = common::report_for(&snapshot);
    let output = project(&snapshot, &options).unwrap();
    assert!(output.contains("Policy: js-ts-positive-path/v1"));
    assert!(output.contains("codemap_js_ts_positive_path"));
    assert!(output.contains(&options.entry_node));
    assert!(output.contains(&options.target_node));
    assert_eq!(output, project(&snapshot, &options).unwrap());
    snapshot.call_sites[0].category = CallSiteCategory::Unsupported;
    snapshot.report = common::report_for(&snapshot);
    assert!(project(&snapshot, &options).is_err());
}

#[test]
#[ignore = "requires exported guarded JS/TS fixture"]
fn guarded_js_ts_fixture_proves_exact_pairs_and_detects_removed_edges() {
    let path = std::env::var_os("CODEMAP_JS_TS_CANONICAL_SNAPSHOT").unwrap();
    let mut snapshot: Snapshot = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    codemap_graph::validate(&snapshot).unwrap();
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../codemap/tests/fixtures/js_ts_calls/expected.json"
    ))
    .unwrap();
    let pairs = oracle["supported_calls"].as_object().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let artifact = dir.path().join("guarded.lemmaspec");
    let mut selections = Vec::new();
    for pair in pairs.values() {
        let select = |path: &str, name: &str| {
            let matches: Vec<_> = snapshot
                .nodes
                .iter()
                .filter(|node| {
                    node.provenance.rel_path == path
                        && node.name == name
                        && matches!(node.kind, NodeKind::Function | NodeKind::Method)
                })
                .collect();
            assert_eq!(matches.len(), 1);
            matches[0].id.clone()
        };
        let options = Options {
            entry_node: select(pair[0].as_str().unwrap(), pair[1].as_str().unwrap()),
            target_node: select(pair[2].as_str().unwrap(), pair[3].as_str().unwrap()),
            allow_dirty: false,
        };
        assert!(
            snapshot
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::Calls
                    && edge.from == options.entry_node
                    && edge.to == options.target_node
                    && edge.provenance.fact_source == FactSource::TypeScript)
        );
        let evidence = project(&snapshot, &options).unwrap();
        assert!(evidence.contains("basis: snapshot"));
        assert!(evidence.contains("basis: reviewer_declared"));
        std::fs::write(&artifact, evidence).unwrap();
        let (code, report) = common::run_lemmaspec(&["walk", artifact.to_str().unwrap(), "--json"]);
        assert_eq!(code, 0, "{report}");
        selections.push(options);
    }
    snapshot.edges.retain(|edge| edge.kind != EdgeKind::Calls);
    for site in &mut snapshot.call_sites {
        if site.category == CallSiteCategory::Resolved {
            site.category = CallSiteCategory::Unmapped;
        }
    }
    snapshot.report = common::report_for(&snapshot);
    for options in selections {
        std::fs::write(&artifact, project(&snapshot, &options).unwrap()).unwrap();
        let (code, report) = common::run_lemmaspec(&["walk", artifact.to_str().unwrap(), "--json"]);
        assert_eq!(
            code, 1,
            "removing canonical calls removes the positive proof: {report}"
        );
    }
}
