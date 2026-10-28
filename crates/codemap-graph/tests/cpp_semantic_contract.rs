use codemap_graph::schema::{
    CPP_SEMANTIC_POLICY, CallSite, CallSiteCategory, CppSemanticInputs, Edge, EdgeKind, FactSource,
    FileRecord, Language, Node, NodeKind, Provenance, RepoIdentity, Report, Snapshot,
    SnapshotIdentity, Span, ToolVersions, canonical_json, edge_id, sha256_hex,
};
use codemap_graph::validate::validate;

fn inputs() -> CppSemanticInputs {
    CppSemanticInputs {
        profile_sha256: "a".repeat(64),
        inputs_sha256: "b".repeat(64),
        runtime_sha256: "c".repeat(64),
        clangd_sha256: "d".repeat(64),
        policy: CPP_SEMANTIC_POLICY.into(),
    }
}

fn snapshot() -> Snapshot {
    let identity = SnapshotIdentity::build(
        Language::Cpp,
        RepoIdentity {
            id: "r".into(),
            origin: None,
            is_canonical: false,
        },
        "commit".into(),
        "tree".into(),
        ToolVersions::syntax_cpp(),
        false,
    )
    .with_cpp_semantic(inputs());
    let provenance = |start, end, source| Provenance {
        repository: "r".into(),
        commit_sha: "commit".into(),
        rel_path: "unit.cpp".into(),
        span: Span::from_ts(start, end, (0, start), (0, end)),
        file_hash: sha256_hex(&[b' '; 100]),
        extractor_version: identity.extractor_version.clone(),
        fact_source: source,
        confidence: u8::MAX,
    };
    let nodes = vec![
        Node {
            python: None,
            id: "cpp:file:unit.cpp".into(),
            kind: NodeKind::File,
            name: "unit.cpp".into(),
            symbol_key: "cpp:unit.cpp".into(),
            provenance: provenance(0, 100, FactSource::TreeSitter),
        },
        Node {
            python: None,
            id: "cpp:function:unit.cpp#caller()".into(),
            kind: NodeKind::Function,
            name: "caller()".into(),
            symbol_key: "cpp:unit.cpp#caller()".into(),
            provenance: provenance(0, 40, FactSource::TreeSitter),
        },
        Node {
            python: None,
            id: "cpp:function:unit.cpp#target()".into(),
            kind: NodeKind::Function,
            name: "target()".into(),
            symbol_key: "cpp:unit.cpp#target()".into(),
            provenance: provenance(60, 90, FactSource::Clangd),
        },
    ];
    let edge = |kind, from: usize, to: usize, provenance: Provenance| Edge {
        id: edge_id(
            kind,
            &nodes[from].id,
            &nodes[to].id,
            provenance.span.start_byte,
        ),
        kind,
        from: nodes[from].id.clone(),
        to: nodes[to].id.clone(),
        provenance,
    };
    let edges = vec![
        edge(EdgeKind::Contains, 0, 1, nodes[1].provenance.clone()),
        edge(EdgeKind::Contains, 0, 2, nodes[2].provenance.clone()),
        edge(EdgeKind::Calls, 1, 2, provenance(4, 8, FactSource::Clangd)),
    ];
    Snapshot {
        identity,
        files: vec![FileRecord {
            rel_path: "unit.cpp".into(),
            sha256: sha256_hex(&[b' '; 100]),
            byte_len: 100,
        }],
        nodes,
        edges,
        call_sites: vec![CallSite {
            rel_path: "unit.cpp".into(),
            span: Span::from_ts(4, 8, (0, 4), (0, 8)),
            raw_name: "target".into(),
            category: CallSiteCategory::Resolved,
        }],
        measures: vec![],
        file_measures: vec![],
        clone_groups: vec![],
        twin_pairs: vec![],
        report: Report {
            files_scanned: 1,
            node_counts: [("file".into(), 1), ("function".into(), 2)].into(),
            edge_counts: [("contains".into(), 2), ("calls".into(), 1)].into(),
            call_sites_by_category: [("Resolved".into(), 1)].into(),
            ..Report::default()
        },
    }
}

fn rejects(mut snapshot: Snapshot, message: &str) {
    snapshot.identity.snapshot_id = snapshot.identity.compute_snapshot_id();
    let error = validate(&snapshot).unwrap_err().to_string();
    assert!(error.contains(message), "{error}");
}

#[test]
fn compiler_facts_require_complete_cpp_identity() {
    let original = snapshot();
    validate(&original).unwrap();
    assert!(original.identity.is_semantic());
    for component in 0..4 {
        for invalid in [
            String::new(),
            "A".repeat(64),
            "0".repeat(63),
            "z".repeat(64),
        ] {
            let mut changed = original.clone();
            let inputs = changed.identity.cpp_semantic.as_mut().unwrap();
            let digest = match component {
                0 => &mut inputs.profile_sha256,
                1 => &mut inputs.inputs_sha256,
                2 => &mut inputs.runtime_sha256,
                _ => &mut inputs.clangd_sha256,
            };
            *digest = invalid;
            assert!(!changed.identity.is_semantic());
            rejects(changed, "SHA-256");
        }
    }
    let mut changed = original.clone();
    changed.identity.cpp_semantic.as_mut().unwrap().policy = "unguarded".into();
    assert!(!changed.identity.is_semantic());
    rejects(changed, "non-canonical policy");
    let mut changed = original.clone();
    changed.identity.language = Language::Rust;
    rejects(changed, "C++ semantic input identity in a non-C++ snapshot");
    let mut changed = original;
    changed.identity.cpp_semantic = None;
    assert!(!changed.identity.is_semantic());
    rejects(changed, "syntax-only snapshot");
}

#[test]
fn compiler_provenance_is_limited_to_exact_callable_facts() {
    let mut changed = snapshot();
    changed.nodes[0].provenance.fact_source = FactSource::Clangd;
    rejects(
        changed,
        "node cpp:file:unit.cpp has non-authoritative source",
    );
    let mut changed = snapshot();
    changed.edges[0].provenance.fact_source = FactSource::Clangd;
    rejects(changed, "structural edge"); // a spelled child cannot acquire compiler provenance
    let mut changed = snapshot();
    changed.edges[1].kind = EdgeKind::Imports;
    rejects(changed, "structural edge");
    let mut changed = snapshot();
    changed.edges[1].from = changed.nodes[1].id.clone();
    rejects(changed, "structural edge"); // recovered body lies outside this proposed parent
    let mut changed = snapshot();
    let mut foreign_parent = changed.nodes[0].clone();
    foreign_parent.id = "cpp:file:other.cpp".into();
    foreign_parent.name = "other.cpp".into();
    foreign_parent.symbol_key = "cpp:other.cpp".into();
    foreign_parent.provenance.rel_path = "other.cpp".into();
    changed.edges[1].from = foreign_parent.id.clone();
    changed.nodes.push(foreign_parent);
    let mut foreign_file = changed.files[0].clone();
    foreign_file.rel_path = "other.cpp".into();
    changed.files.push(foreign_file);
    changed.report.files_scanned = 2;
    changed.report.node_counts.insert("file".into(), 2);
    rejects(changed, "structural edge"); // compiler identity cannot justify cross-file containment
    let mut changed = snapshot();
    changed.edges[1].from = changed.nodes[2].id.clone();
    rejects(changed, "structural edge"); // no self-containment
    let mut changed = snapshot();
    changed.edges[2].to = changed.nodes[0].id.clone();
    rejects(changed, "must connect callable bodies");
    let mut changed = snapshot();
    changed.edges[2].provenance.span = Span::from_ts(80, 84, (0, 80), (0, 84));
    rejects(changed, "outside its caller body");
    let mut changed = snapshot();
    changed.edges[2].provenance.fact_source = FactSource::TreeSitter;
    rejects(changed, "non-authoritative source");
}

#[test]
fn every_cpp_input_component_changes_identity_without_physical_paths() {
    let identity = snapshot().identity;
    for component in 0..5 {
        let mut changed = identity.clone();
        let inputs = changed.cpp_semantic.as_mut().unwrap();
        let value = match component {
            0 => &mut inputs.profile_sha256,
            1 => &mut inputs.inputs_sha256,
            2 => &mut inputs.runtime_sha256,
            3 => &mut inputs.clangd_sha256,
            _ => &mut inputs.policy,
        };
        *value = "e".repeat(64);
        assert_ne!(identity.snapshot_id, changed.compute_snapshot_id());
    }
    let mut syntax = identity;
    syntax.cpp_semantic = None;
    syntax.snapshot_id = syntax.compute_snapshot_id();
    let json = canonical_json(&syntax).unwrap();
    assert!(!json.contains("cpp_semantic"));
    let decoded: SnapshotIdentity = serde_json::from_str(&json).unwrap();
    assert_eq!(canonical_json(&decoded).unwrap(), json);
    assert_eq!(decoded.compute_snapshot_id(), syntax.snapshot_id);
}

#[test]
fn unrelated_input_fields_are_rejected() {
    let mut json = serde_json::to_value(inputs()).unwrap();
    json["installation_path"] = serde_json::json!("/operator/path");
    assert!(serde_json::from_value::<CppSemanticInputs>(json).is_err());
}

#[test]
fn cpp_resolved_census_requires_exactly_one_edge_per_site() {
    let mut changed = snapshot();
    changed.edges.retain(|edge| edge.kind != EdgeKind::Calls);
    changed.report.edge_counts.remove("calls");
    rejects(changed, "resolved call site has no exact call edge");

    let mut changed = snapshot();
    changed.call_sites.clear();
    changed.report.call_sites_by_category.clear();
    rejects(changed, "call edge has no exact resolved call site");

    for category in [
        CallSiteCategory::Unsupported,
        CallSiteCategory::Inactive,
        CallSiteCategory::Unprofiled,
    ] {
        let mut changed = snapshot();
        changed.call_sites[0].category = category;
        changed.report.call_sites_by_category = [(category.report_key().into(), 1)].into();
        rejects(
            changed,
            "calls edge contradicts its exact resolved call site",
        );
    }

    let mut changed = snapshot();
    changed.call_sites[0].span.start_col += 1;
    rejects(
        changed,
        "calls edge contradicts its exact resolved call site",
    );
}

#[test]
fn cpp_exclusion_categories_remain_semantic_and_language_specific() {
    for category in [
        CallSiteCategory::Unsupported,
        CallSiteCategory::Inactive,
        CallSiteCategory::Unprofiled,
    ] {
        let mut changed = snapshot();
        changed.edges.retain(|edge| edge.kind != EdgeKind::Calls);
        changed.report.edge_counts.remove("calls");
        changed.call_sites[0].category = category;
        changed.report.call_sites_by_category = [(category.report_key().into(), 1)].into();
        validate(&changed).unwrap();
        let mut foreign = changed.clone();
        foreign.identity.language = Language::Rust;
        rejects(foreign, "C++ call classification in a non-C++ snapshot");
        changed.identity.cpp_semantic = None;
        rejects(changed, "claims semantic category");
    }
}
