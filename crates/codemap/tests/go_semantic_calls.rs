//! G2 Go semantic call-impact oracle: canonical `calls` edges must match
//! the labelled fixture exactly under codemap's gates over gopls answers,
//! stay byte-deterministic across independent checkouts and lane counts,
//! and leave the analyzed tree untouched.
//!
//! Ignored by default: requires a gopls tool bundle via
//! `CODEMAP_GO_TOOL_BUNDLE`. Run with `cargo test -- --ignored`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use codemap::schema::{CallSiteCategory, EdgeKind, FactSource, Language};
use codemap::{ExtractOptions, Snapshot, extract, to_json};
use serde::Deserialize;

mod common;

#[derive(Debug, Deserialize)]
struct ExpectedCalls {
    canonical_edges: BTreeSet<(String, String)>,
    external_observations: BTreeSet<String>,
    standard_library_observations: BTreeSet<String>,
    candidate_observations: BTreeSet<String>,
    ambiguous_observations: BTreeSet<String>,
    unmapped_observations: BTreeSet<String>,
}

#[test]
#[ignore = "requires a gopls tool bundle via CODEMAP_GO_TOOL_BUNDLE"]
fn go_semantic_mode_matches_the_labelled_call_oracle() {
    let expected: ExpectedCalls =
        serde_json::from_str(include_str!("fixtures/go_calls/expected.json"))
            .expect("parse expected go call oracle");
    // Two independent checkouts, resolved over one lane and then over
    // three: the bytes must not depend on the clone or on the lane count.
    let repo = common::make_go_calls_repo().expect("create go calls repo");
    // SAFETY: this integration test binary runs this one test; nothing else
    // reads the environment concurrently.
    unsafe { std::env::set_var("CODEMAP_DEFINITION_WORKERS", "1") };
    let snapshot = extract_semantic(&repo);
    let repo2 = common::make_go_calls_repo().expect("create independent go calls repo");
    unsafe { std::env::set_var("CODEMAP_DEFINITION_WORKERS", "3") };
    let snapshot2 = extract_semantic(&repo2);
    unsafe { std::env::remove_var("CODEMAP_DEFINITION_WORKERS") };
    assert_eq!(
        to_json(&snapshot).expect("serialize first semantic snapshot"),
        to_json(&snapshot2).expect("serialize second semantic snapshot"),
        "semantic extraction must be byte-identical across independent checkouts and lane counts"
    );
    assert_semantic_oracle(&snapshot, &expected);
    // gopls keeps its caches under the throwaway home; the analyzed tree
    // must come out of the run exactly as it went in.
    for root in [&repo, &repo2] {
        let status = std::process::Command::new("git")
            .current_dir(root)
            .args(["status", "--porcelain"])
            .output()
            .expect("git status on the analyzed tree");
        assert!(
            status.status.success() && status.stdout.is_empty(),
            "gopls wrote into the analyzed tree under {}:\n{}",
            root.display(),
            String::from_utf8_lossy(&status.stdout)
        );
    }

    let syntax_snapshot = extract(&ExtractOptions {
        path: PathBuf::from(&repo2),
        calls_required: false,
        measures: false,
        twins: false,
        language: Language::Go,
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
        language: Language::Go,
    })
    .expect("semantic go extraction")
}

#[allow(clippy::too_many_lines)] // one labelled oracle, one assertion per gate
fn assert_semantic_oracle(snapshot: &Snapshot, expected: &ExpectedCalls) {
    assert!(
        snapshot.identity.tools.gopls.is_some(),
        "semantic mode must record the gopls version in identity"
    );
    assert!(
        snapshot.identity.tools.go.is_some(),
        "semantic mode must record the companion go version in identity"
    );
    assert!(
        snapshot.identity.tools.go_tool_bundle_sha256.is_some(),
        "semantic mode must pin the tool bundle digest in identity"
    );
    // The vendored dependency is outside the inventory.
    assert!(
        snapshot
            .files
            .iter()
            .all(|file| !file.rel_path.starts_with("vendor/")),
        "vendor must never enter the Go inventory"
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
        calls.len(),
        expected.canonical_edges.len(),
        "every labelled canonical edge must be emitted exactly once: {actual_edges:?}"
    );
    assert_eq!(
        actual_edges, expected.canonical_edges,
        "canonical go call edges must match the labelled oracle exactly"
    );
    assert!(
        calls.iter().all(|edge| {
            edge.provenance.fact_source == FactSource::Gopls
                && edge.provenance.confidence == 255
                && symbol_by_id.contains_key(edge.to.as_str())
        }),
        "every canonical edge must carry gopls provenance at full confidence"
    );

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

    for (category, expected_names, what) in [
        (
            CallSiteCategory::ExternalCrate,
            &expected.external_observations,
            "vendored and module-cache calls must stay observations",
        ),
        (
            CallSiteCategory::StandardLibrary,
            &expected.standard_library_observations,
            "standard-library and builtin calls must stay observations",
        ),
        (
            CallSiteCategory::Candidate,
            &expected.candidate_observations,
            "a resolved call outside any callable is a candidate, never an edge",
        ),
        (
            CallSiteCategory::Ambiguous,
            &expected.ambiguous_observations,
            "several in-repo answers are never one edge",
        ),
        (
            CallSiteCategory::Unmapped,
            &expected.unmapped_observations,
            "conversions, function values, and method values must stay observations",
        ),
    ] {
        assert_eq!(
            &observation_names(snapshot, category),
            expected_names,
            "{what}"
        );
    }
}

/// Distinct raw names observed per category. Used for the labelled
/// expectation sets; the full per-site census above still guarantees
/// exactness of sites, not just names.
fn observation_names(snapshot: &Snapshot, category: CallSiteCategory) -> BTreeSet<String> {
    snapshot
        .call_sites
        .iter()
        .filter(|site| site.category == category)
        .map(|site| site.raw_name.clone())
        .collect()
}
