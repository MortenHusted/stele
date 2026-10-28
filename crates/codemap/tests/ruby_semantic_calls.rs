//! R2 Ruby semantic call-impact gate: ruby-lsp must produce exactly the
//! labelled in-repository call graph admitted by codemap's static model
//! (constant-receiver, implicit-self and verified constructor shapes, target owner inside
//! the receiver's `inherits`/`implements` closure), while every guess-regime
//! and unverified call stays a non-canonical observation.
//!
//! The oracle fixture mirrors `calls_cargo`; the expected set is labelled in
//! `fixtures/ruby_calls/expected.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use codemap::schema::{CallSiteCategory, EdgeKind, FactSource, Language, Snapshot};
use codemap::{ExtractOptions, extract, to_json};
use serde::Deserialize;

mod common;

#[derive(Debug, Deserialize)]
struct ExpectedCalls {
    canonical_edges: BTreeSet<(String, String)>,
    external_observations: BTreeSet<String>,
    standard_library_observations: BTreeSet<String>,
    candidate_observations: BTreeSet<String>,
    unmapped_observations: BTreeSet<String>,
}

/// This test requires a ruby-lsp tool bundle: `CODEMAP_RUBY_TOOL_BUNDLE`
/// must point at a Gemfile whose lockfile is byte-identical to codemap's
/// `toolchain/ruby/Gemfile.lock`. Ignored by default so `cargo test` stays
/// hermetic; run with `cargo test -- --ignored`.
#[test]
#[ignore = "requires a ruby-lsp tool bundle via CODEMAP_RUBY_TOOL_BUNDLE"]
fn ruby_semantic_mode_matches_the_labelled_call_oracle() {
    let expected: ExpectedCalls =
        serde_json::from_str(include_str!("fixtures/ruby_calls/expected.json"))
            .expect("parse expected ruby call oracle");
    // Two independent checkouts, resolved over one lane and then over
    // three: the bytes must not depend on the clone or on the lane count.
    let repo = common::make_ruby_calls_repo().expect("create ruby calls repo");
    // SAFETY: this integration test binary runs this one test; nothing else
    // reads the environment concurrently.
    unsafe { std::env::set_var("CODEMAP_DEFINITION_WORKERS", "1") };
    let snapshot = extract_semantic(&repo);
    let repo2 = common::make_ruby_calls_repo().expect("create independent ruby calls repo");
    unsafe { std::env::set_var("CODEMAP_DEFINITION_WORKERS", "3") };
    let snapshot2 = extract_semantic(&repo2);
    unsafe { std::env::remove_var("CODEMAP_DEFINITION_WORKERS") };
    assert_eq!(
        to_json(&snapshot).expect("serialize first semantic snapshot"),
        to_json(&snapshot2).expect("serialize second semantic snapshot"),
        "semantic extraction must be byte-identical across independent checkouts and lane counts"
    );
    assert_semantic_oracle(&snapshot, &expected);

    let syntax_snapshot = extract(&ExtractOptions {
        path: PathBuf::from(&repo2),
        calls_required: false,
        measures: false,
        twins: false,
        language: Language::Ruby,
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
        language: Language::Ruby,
    })
    .expect("semantic ruby extraction")
}

fn assert_semantic_oracle(snapshot: &Snapshot, expected: &ExpectedCalls) {
    assert!(
        snapshot.identity.tools.ruby_lsp.is_some(),
        "semantic mode must record the ruby-lsp version in identity"
    );
    assert!(
        snapshot.identity.tools.ruby_tool_bundle_sha256.is_some(),
        "semantic mode must pin the tool bundle digest in identity"
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
    // The canonical graph is a multiset over call sites: two sites with the
    // same endpoints must both be emitted, so the count must match the
    // labelled set length, not just the endpoint set.
    assert_eq!(
        calls.len(),
        expected.canonical_edges.len(),
        "every labelled canonical edge must be emitted exactly once"
    );
    assert_eq!(
        actual_edges, expected.canonical_edges,
        "canonical ruby call edges must match the labelled oracle exactly"
    );
    assert!(calls.iter().all(|edge| {
        edge.provenance.fact_source == FactSource::RubyLsp
            && edge.provenance.confidence == 255
            && symbol_by_id.contains_key(edge.to.as_str())
    }));

    // Complete call-site census: every semantic-mode site must be labelled
    // by (path, start byte, raw name, category) and fall into one of the
    // four observation buckets or the resolved category. This catches
    // duplicates, dropped sites, and unexpected categories.
    let census: BTreeSet<_> = snapshot
        .call_sites
        .iter()
        .map(|site| {
            (
                site.rel_path.as_str(),
                site.span.start_byte,
                site.raw_name.as_str(),
                site.category.report_key(),
            )
        })
        .collect();
    assert_eq!(
        census.len(),
        snapshot.call_sites.len(),
        "call-site census must not collapse duplicate sites"
    );
    assert!(
        snapshot
            .call_sites
            .iter()
            .all(|site| site.category != CallSiteCategory::Observed),
        "every call site must be classified in semantic mode"
    );

    let external = observation_names(snapshot, CallSiteCategory::ExternalCrate);
    assert_eq!(
        external, expected.external_observations,
        "external gem calls must stay observations"
    );
    let standard_library = observation_names(snapshot, CallSiteCategory::StandardLibrary);
    assert_eq!(
        standard_library, expected.standard_library_observations,
        "stdlib calls must stay observations"
    );
    let candidates = observation_names(snapshot, CallSiteCategory::Candidate);
    assert_eq!(
        candidates, expected.candidate_observations,
        "guess-regime sites must be non-authoritative candidates, never edges"
    );
    let unmapped = observation_names(snapshot, CallSiteCategory::Unmapped);
    assert_eq!(
        unmapped, expected.unmapped_observations,
        "unresolved sites must be unmapped observations"
    );
}

/// Distinct raw names observed per category. Used for the labelled
/// expectation sets; the full per-site census above still guarantees
/// exactness of sites, not just names.
fn observation_names(snapshot: &Snapshot, category: CallSiteCategory) -> BTreeSet<String> {
    snapshot
        .call_sites
        .iter()
        .filter(|call| call.category == category)
        .map(|call| call.raw_name.clone())
        .collect()
}
