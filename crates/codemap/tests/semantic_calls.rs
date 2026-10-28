//! P2 semantic call-impact gate: rust-analyzer must produce exactly the
//! labelled in-repository call graph while retaining external and standard
//! library calls as non-canonical observations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use codemap::schema::{CallSiteCategory, EdgeKind, FactSource, Snapshot};
use codemap::{ExtractOptions, extract, to_json};
use serde::Deserialize;

mod common;

#[derive(Debug, Deserialize)]
struct ExpectedCalls {
    canonical_edges: BTreeSet<(String, String)>,
    external_observations: BTreeSet<String>,
    standard_library_observations: BTreeSet<String>,
}

/// This test requires rust-analyzer on PATH. It is ignored by default so
/// `cargo test` stays hermetic; run with `cargo test -- --ignored`.
#[test]
#[ignore = "requires rust-analyzer on PATH"]
fn semantic_mode_matches_the_labelled_call_oracle() {
    let expected: ExpectedCalls =
        serde_json::from_str(include_str!("fixtures/calls_cargo/expected.json"))
            .expect("parse expected call oracle");
    let repo = common::make_calls_cargo_repo().expect("create calls cargo repo");
    let snapshot = extract_semantic(&repo);
    let repo2 = common::make_calls_cargo_repo().expect("create independent calls cargo repo");
    let snapshot2 = extract_semantic(&repo2);
    assert_eq!(
        to_json(&snapshot).expect("serialize first semantic snapshot"),
        to_json(&snapshot2).expect("serialize second semantic snapshot"),
        "semantic extraction must be byte-identical across independent checkouts"
    );
    assert_semantic_oracle(&snapshot, &expected);

    let syntax_snapshot = extract(&ExtractOptions {
        path: PathBuf::from(&repo2),
        calls_required: false,
        measures: false,
        twins: false,
        language: codemap::schema::Language::Rust,
    })
    .expect("syntax extraction");
    assert!(
        syntax_snapshot
            .edges
            .iter()
            .all(|edge| edge.kind != EdgeKind::Calls),
        "syntax mode must emit zero calls edges"
    );
}

fn extract_semantic(repo: &std::path::Path) -> Snapshot {
    extract(&ExtractOptions {
        path: repo.to_path_buf(),
        calls_required: true,
        measures: false,
        twins: false,
        language: codemap::schema::Language::Rust,
    })
    .expect("semantic extraction")
}

fn assert_semantic_oracle(snapshot: &Snapshot, expected: &ExpectedCalls) {
    assert!(
        snapshot.identity.tools.rust_analyzer.is_some(),
        "semantic mode must record rust-analyzer version in identity"
    );

    let symbol_by_id: BTreeMap<_, _> = snapshot
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.symbol_key.as_str()))
        .collect();
    let calls: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
        .collect();
    let actual_edges: BTreeSet<_> = calls
        .iter()
        .map(|edge| {
            (
                symbol_by_id[edge.from.as_str()].to_string(),
                symbol_by_id[edge.to.as_str()].to_string(),
            )
        })
        .collect();
    assert_eq!(
        actual_edges, expected.canonical_edges,
        "canonical call edges must match the labelled oracle exactly"
    );
    assert!(calls.iter().all(|edge| {
        edge.provenance.fact_source == FactSource::RustAnalyzer
            && symbol_by_id.contains_key(edge.to.as_str())
    }));

    let external = observations(snapshot, CallSiteCategory::ExternalCrate);
    assert_eq!(
        external, expected.external_observations,
        "observations: {:?}",
        snapshot.call_sites
    );
    assert!(
        external.len() >= 3,
        "oracle must cover three external calls"
    );

    let standard_library = observations(snapshot, CallSiteCategory::StandardLibrary);
    assert_eq!(standard_library, expected.standard_library_observations);
    assert!(
        !standard_library.is_empty(),
        "oracle must cover a standard-library call"
    );
}

fn observations(snapshot: &Snapshot, category: CallSiteCategory) -> BTreeSet<String> {
    snapshot
        .call_sites
        .iter()
        .filter(|call| call.category == category)
        .map(|call| call.raw_name.clone())
        .collect()
}
