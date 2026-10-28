//! P0 validator negative controls: deliberately invalid snapshots must
//! fail [`codemap::validate`].

use std::collections::BTreeMap;

use codemap::schema::{
    CallSite, CallSiteCategory, Edge, EdgeKind, FactSource, FileRecord, Language, Node, NodeKind,
    Provenance, RepoIdentity, Report, Snapshot, SnapshotIdentity, Span, ToolVersions,
};
use codemap::validate;

/// Build a minimal valid snapshot. Each negative test clones this and
/// mutates one aspect.
fn minimal_snapshot() -> Snapshot {
    let repo = RepoIdentity {
        id: "r".into(),
        origin: None,
        is_canonical: false,
    };
    let identity = SnapshotIdentity::build(
        Language::Rust,
        repo,
        "c".into(),
        "t".into(),
        ToolVersions::syntax(),
        false,
    );
    let provenance = Provenance {
        repository: "r".into(),
        commit_sha: "c".into(),
        rel_path: "a.rs".into(),
        span: Span::from_ts(0, 1, (0, 0), (0, 1)),
        file_hash: "h".into(),
        extractor_version: identity.extractor_version.clone(),
        fact_source: FactSource::TreeSitter,
        confidence: 255,
    };
    let file_node = Node {
        python: None,
        id: "rust:file:a.rs".into(),
        kind: NodeKind::File,
        name: "a.rs".into(),
        symbol_key: "rust:file:a.rs".into(),
        provenance,
    };
    Snapshot {
        identity,
        files: vec![FileRecord {
            rel_path: "a.rs".into(),
            sha256: "h".into(),
            byte_len: 1,
        }],
        nodes: vec![file_node],
        edges: vec![],
        call_sites: vec![],
        measures: Vec::new(),
        file_measures: Vec::new(),
        clone_groups: Vec::new(),
        twin_pairs: Vec::new(),
        report: Report {
            files_scanned: 1,
            files_skipped: 0,
            parse_errors: 0,
            node_counts: BTreeMap::from([("file".into(), 1)]),
            edge_counts: BTreeMap::new(),
            call_sites_by_category: BTreeMap::new(),
            import_ref_nodes: 0,
            observations: BTreeMap::new(),
        },
    }
}

#[test]
fn snapshot_report_counts_must_match_contents() {
    let mut files = minimal_snapshot();
    files.report.files_scanned = 0;
    assert!(validate(&files).is_err(), "file count drift must fail");

    let mut skipped = minimal_snapshot();
    skipped.report.files_skipped = 1;
    assert!(validate(&skipped).is_err(), "skipped files must fail");

    let mut nodes = minimal_snapshot();
    nodes.report.node_counts.clear();
    assert!(validate(&nodes).is_err(), "node count drift must fail");

    let mut edges = minimal_snapshot();
    edges.report.edge_counts.insert("contains".into(), 1);
    assert!(validate(&edges).is_err(), "edge count drift must fail");

    let mut imports = minimal_snapshot();
    imports.report.import_ref_nodes = 1;
    assert!(
        validate(&imports).is_err(),
        "import-ref count drift must fail"
    );
}

#[test]
fn structural_facts_require_maximum_confidence() {
    let mut snap = minimal_snapshot();
    snap.nodes[0].provenance.confidence = 1;
    assert!(
        validate(&snap).is_err(),
        "structural confidence below 255 must fail"
    );
}

fn add_second_file(snapshot: &mut Snapshot) {
    snapshot.files.push(FileRecord {
        rel_path: "b.rs".into(),
        sha256: "other".into(),
        byte_len: 1,
    });
}

fn call_site(category: CallSiteCategory) -> CallSite {
    CallSite {
        rel_path: "a.rs".into(),
        span: Span::from_ts(0, 1, (0, 0), (0, 1)),
        raw_name: "call".into(),
        category,
    }
}

fn make_semantic(snapshot: &mut Snapshot) {
    snapshot.identity = SnapshotIdentity::build(
        Language::Rust,
        snapshot.identity.repository.clone(),
        snapshot.identity.commit_sha.clone(),
        snapshot.identity.tree_sha.clone(),
        ToolVersions::semantic(
            "rust-analyzer test".into(),
            "rustc test".into(),
            "cargo test".into(),
            "test-target".into(),
            None,
        ),
        false,
    );
}

/// Historical `codemap.snapshot.v3` snapshots are not v4 snapshots: the
/// validator must reject the stale marker rather than silently accept a
/// snapshot from a previous schema generation.
#[test]
fn v3_schema_marker_is_rejected() {
    let mut snap = minimal_snapshot();
    snap.identity.schema_version = "codemap.snapshot.v3".into();
    let err = validate(&snap).expect_err("stale schema marker must fail");
    assert!(
        err.to_string().contains("schema_version mismatch"),
        "{err:#}"
    );
}

#[test]
fn duplicate_node_id_is_rejected() {
    let mut snap = minimal_snapshot();
    snap.nodes.push(snap.nodes[0].clone());
    assert!(
        validate(&snap).is_err(),
        "duplicate node id must fail validation"
    );
}

#[test]
fn dangling_edge_endpoint_is_rejected() {
    let mut snap = minimal_snapshot();
    let prov = snap.nodes[0].provenance.clone();
    snap.edges.push(Edge {
        id: "contains:rust:file:a.rs->rust:missing@0".into(),
        kind: EdgeKind::Contains,
        from: "rust:file:a.rs".into(),
        to: "rust:function:a.rs#missing".into(),
        provenance: prov,
    });
    assert!(
        validate(&snap).is_err(),
        "dangling edge target must fail validation"
    );
}

#[test]
fn invalid_span_is_rejected() {
    let mut snap = minimal_snapshot();
    snap.nodes[0].provenance.span.start_byte = 10;
    snap.nodes[0].provenance.span.end_byte = 5;
    assert!(
        validate(&snap).is_err(),
        "span with start_byte > end_byte must fail validation"
    );
}

#[test]
fn unknown_file_hash_is_rejected() {
    let mut snap = minimal_snapshot();
    snap.nodes[0].provenance.file_hash =
        "0000000000000000000000000000000000000000000000000000000000000000".into();
    assert!(
        validate(&snap).is_err(),
        "node file_hash not present in snapshot files must fail validation"
    );
}

#[test]
fn same_snapshot_path_hash_swap_is_rejected() {
    let mut snap = minimal_snapshot();
    add_second_file(&mut snap);
    snap.nodes[0].provenance.file_hash = "other".into();
    assert!(
        validate(&snap).is_err(),
        "a hash belonging to another path in the same snapshot must fail validation"
    );
}

#[test]
fn unknown_provenance_path_is_rejected() {
    let mut snap = minimal_snapshot();
    snap.nodes[0].provenance.rel_path = "missing.rs".into();
    assert!(
        validate(&snap).is_err(),
        "a provenance path absent from the snapshot must fail validation"
    );
}

#[test]
fn out_of_file_node_span_is_rejected() {
    let mut snap = minimal_snapshot();
    snap.nodes[0].provenance.span.end_byte = 2;
    assert!(
        validate(&snap).is_err(),
        "a node span beyond its source file must fail validation"
    );
}

#[test]
fn file_node_must_cover_complete_file_bytes() {
    let mut snap = minimal_snapshot();
    snap.nodes[0].provenance.span.end_byte = 0;
    assert!(
        validate(&snap).is_err(),
        "a file node that does not cover the complete file must fail validation"
    );
}

#[test]
fn provenance_repository_drift_is_rejected() {
    let mut snap = minimal_snapshot();
    snap.nodes[0].provenance.repository = "other-repository".into();
    assert!(
        validate(&snap).is_err(),
        "repository drift must fail validation"
    );
}

#[test]
fn provenance_commit_drift_is_rejected() {
    let mut snap = minimal_snapshot();
    snap.nodes[0].provenance.commit_sha = "other-commit".into();
    assert!(
        validate(&snap).is_err(),
        "commit drift must fail validation"
    );
}

#[test]
fn provenance_extractor_drift_is_rejected() {
    let mut snap = minimal_snapshot();
    snap.nodes[0].provenance.extractor_version = "other-extractor".into();
    assert!(
        validate(&snap).is_err(),
        "extractor drift must fail validation"
    );
}

#[test]
fn calls_edge_with_tree_sitter_source_is_rejected() {
    let mut snap = minimal_snapshot();
    let prov = Provenance {
        repository: "r".into(),
        commit_sha: "c".into(),
        rel_path: "a.rs".into(),
        span: Span::from_ts(0, 1, (0, 0), (0, 1)),
        file_hash: "h".into(),
        extractor_version: snap.identity.extractor_version.clone(),
        fact_source: FactSource::TreeSitter,
        confidence: 1,
    };

    snap.nodes.push(Node {
        python: None,
        id: "rust:function:a.rs#caller".into(),
        kind: NodeKind::Function,
        name: "caller".into(),
        symbol_key: "rust:a.rs#caller".into(),
        provenance: prov.clone(),
    });
    snap.nodes.push(Node {
        python: None,
        id: "rust:function:a.rs#callee".into(),
        kind: NodeKind::Function,
        name: "callee".into(),
        symbol_key: "rust:a.rs#callee".into(),
        provenance: prov.clone(),
    });
    snap.edges.push(Edge {
        id: "calls:rust:function:a.rs#caller->rust:function:a.rs#callee@0".into(),
        kind: EdgeKind::Calls,
        from: "rust:function:a.rs#caller".into(),
        to: "rust:function:a.rs#callee".into(),
        provenance: prov,
    });

    assert!(
        validate(&snap).is_err(),
        "calls edge with TreeSitter fact source must fail validation"
    );
}

#[test]
fn structural_edge_with_rust_analyzer_source_is_rejected() {
    let mut snap = minimal_snapshot();
    let mut provenance = snap.nodes[0].provenance.clone();
    provenance.fact_source = FactSource::RustAnalyzer;
    snap.edges.push(Edge {
        id: "contains:rust:file:a.rs->rust:file:a.rs@0".into(),
        kind: EdgeKind::Contains,
        from: "rust:file:a.rs".into(),
        to: "rust:file:a.rs".into(),
        provenance,
    });
    assert!(
        validate(&snap).is_err(),
        "a structural edge with RustAnalyzer provenance must fail validation"
    );
}

#[test]
fn node_with_rust_analyzer_source_is_rejected() {
    let mut snap = minimal_snapshot();
    snap.nodes[0].provenance.fact_source = FactSource::RustAnalyzer;
    assert!(
        validate(&snap).is_err(),
        "a syntax node with RustAnalyzer provenance must fail validation"
    );
}

#[test]
fn edge_path_hash_mismatch_is_rejected() {
    let mut snap = minimal_snapshot();
    add_second_file(&mut snap);
    let mut provenance = snap.nodes[0].provenance.clone();
    provenance.file_hash = "other".into();
    snap.edges.push(Edge {
        id: "contains:rust:file:a.rs->rust:file:a.rs@0".into(),
        kind: EdgeKind::Contains,
        from: "rust:file:a.rs".into(),
        to: "rust:file:a.rs".into(),
        provenance,
    });
    assert!(
        validate(&snap).is_err(),
        "an edge hash belonging to another path must fail validation"
    );
}

#[test]
fn unknown_edge_provenance_path_is_rejected() {
    let mut snap = minimal_snapshot();
    let mut provenance = snap.nodes[0].provenance.clone();
    provenance.rel_path = "missing.rs".into();
    snap.edges.push(Edge {
        id: "contains:rust:file:a.rs->rust:file:a.rs@0".into(),
        kind: EdgeKind::Contains,
        from: "rust:file:a.rs".into(),
        to: "rust:file:a.rs".into(),
        provenance,
    });
    assert!(
        validate(&snap).is_err(),
        "an edge provenance path absent from the snapshot must fail validation"
    );
}

#[test]
fn out_of_file_edge_span_is_rejected() {
    let mut snap = minimal_snapshot();
    let mut provenance = snap.nodes[0].provenance.clone();
    provenance.span.end_byte = 2;
    snap.edges.push(Edge {
        id: "contains:rust:file:a.rs->rust:file:a.rs@0".into(),
        kind: EdgeKind::Contains,
        from: "rust:file:a.rs".into(),
        to: "rust:file:a.rs".into(),
        provenance,
    });
    assert!(
        validate(&snap).is_err(),
        "an edge span beyond its source file must fail validation"
    );
}

#[test]
fn out_of_file_call_site_span_is_rejected() {
    let mut snap = minimal_snapshot();
    snap.call_sites.push(codemap::schema::CallSite {
        rel_path: "a.rs".into(),
        span: Span::from_ts(0, 2, (0, 0), (0, 2)),
        raw_name: "call".into(),
        category: codemap::schema::CallSiteCategory::Observed,
    });
    assert!(
        validate(&snap).is_err(),
        "a call-site span beyond its source file must fail validation"
    );
}

#[test]
fn unknown_call_site_path_is_rejected() {
    let mut snap = minimal_snapshot();
    snap.call_sites.push(codemap::schema::CallSite {
        rel_path: "missing.rs".into(),
        span: Span::from_ts(0, 1, (0, 0), (0, 1)),
        raw_name: "call".into(),
        category: codemap::schema::CallSiteCategory::Observed,
    });
    assert!(
        validate(&snap).is_err(),
        "a call-site path absent from the snapshot must fail validation"
    );
}

#[test]
fn syntax_snapshot_rejects_semantic_call_site_category() {
    let mut snap = minimal_snapshot();
    snap.call_sites
        .push(call_site(CallSiteCategory::ExternalCrate));
    assert!(
        validate(&snap).is_err(),
        "syntax snapshots must not claim semantic call classification"
    );
}

#[test]
fn semantic_snapshot_rejects_syntax_only_observations() {
    let mut snap = minimal_snapshot();
    make_semantic(&mut snap);
    snap.call_sites.push(call_site(CallSiteCategory::Observed));
    assert!(
        validate(&snap).is_err(),
        "semantic snapshots must classify every call observation"
    );
}

#[test]
fn successful_snapshot_rejects_failed_call_observations() {
    let mut snap = minimal_snapshot();
    make_semantic(&mut snap);
    snap.call_sites.push(call_site(CallSiteCategory::Failed));
    assert!(
        validate(&snap).is_err(),
        "required semantic failures must abort instead of entering snapshots"
    );
}

#[test]
fn call_site_report_counts_must_match_observations() {
    let mut snap = minimal_snapshot();
    snap.call_sites.push(call_site(CallSiteCategory::Observed));
    assert!(
        validate(&snap).is_err(),
        "call-site report counts must be derived from the observations"
    );
}

#[test]
fn duplicate_edge_id_is_rejected() {
    let mut snap = minimal_snapshot();
    let edge = Edge {
        id: "contains:rust:file:a.rs->rust:file:a.rs@0".into(),
        kind: EdgeKind::Contains,
        from: "rust:file:a.rs".into(),
        to: "rust:file:a.rs".into(),
        provenance: snap.nodes[0].provenance.clone(),
    };
    snap.edges.push(edge.clone());
    snap.edges.push(edge);
    assert!(
        validate(&snap).is_err(),
        "duplicate edge id must fail validation"
    );
}
