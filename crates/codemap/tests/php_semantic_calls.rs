//! P2 PHP adapter gate: semantic call-impact oracle against phpactor under
//! codemap's verified-edge policy.
//!
//! The oracle fixture mirrors `ruby_calls`; the expected set is labelled in
//! `fixtures/php_calls/expected.json`.
//!
//! This test requires a phpactor tool bundle: `CODEMAP_PHP_TOOL_BUNDLE`
//! pointing at the installed `toolchain/php/` directory (composer bundle
//! whose lockfile is byte-identical to the canonical pin). Ignored by
//! default — run with `cargo test -- --ignored`.

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

#[test]
#[ignore = "requires a phpactor tool bundle via CODEMAP_PHP_TOOL_BUNDLE"]
fn php_semantic_mode_matches_the_labelled_call_oracle() {
    let expected: ExpectedCalls =
        serde_json::from_str(include_str!("fixtures/php_calls/expected.json"))
            .expect("parse expected php call oracle");
    // Two independent checkouts, resolved over one server and then over
    // three: the bytes must not depend on the clone or on the pool size.
    let repo = common::make_php_calls_repo().expect("create php calls repo");
    // SAFETY: this integration test binary runs this one test; nothing else
    // reads the environment concurrently.
    unsafe { std::env::set_var("CODEMAP_DEFINITION_WORKERS", "1") };
    let snapshot = extract_semantic(&repo);
    let repo2 = common::make_php_calls_repo().expect("create independent php calls repo");
    unsafe { std::env::set_var("CODEMAP_DEFINITION_WORKERS", "3") };
    let snapshot2 = extract_semantic(&repo2);
    unsafe { std::env::remove_var("CODEMAP_DEFINITION_WORKERS") };
    assert_eq!(
        to_json(&snapshot).expect("serialize first semantic snapshot"),
        to_json(&snapshot2).expect("serialize second semantic snapshot"),
        "semantic extraction must be byte-identical across independent checkouts and pool sizes"
    );
    assert_semantic_oracle(&snapshot, &expected);
    // The fixture carries a project .phpactor.json that enables the composer
    // autoloader and vendor files that write a marker when included. A
    // launch that honoured either would have executed fixture code. Its
    // composer.lock names phpstan: phpactor's auto-configuration would
    // answer that by writing into .phpactor.json, and the clean check
    // inside extraction already refuses such a run. Prove the tree is
    // untouched.
    for root in [&repo, &repo2] {
        assert!(
            !root.join(".codemap-autoloader-executed").exists(),
            "phpactor executed code from the analyzed tree under {}",
            root.display()
        );
        let status = std::process::Command::new("git")
            .current_dir(root)
            .args(["status", "--porcelain"])
            .output()
            .expect("git status on the analyzed tree");
        assert!(
            status.status.success() && status.stdout.is_empty(),
            "phpactor wrote into the analyzed tree under {}:\n{}",
            root.display(),
            String::from_utf8_lossy(&status.stdout)
        );
    }

    let syntax_snapshot = extract(&ExtractOptions {
        path: PathBuf::from(&repo2),
        calls_required: false,
        measures: false,
        twins: false,
        language: Language::Php,
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
        language: Language::Php,
    })
    .expect("semantic php extraction")
}

fn assert_semantic_oracle(snapshot: &Snapshot, expected: &ExpectedCalls) {
    assert!(
        snapshot.identity.tools.phpactor.is_some(),
        "semantic mode must record the phpactor version in identity"
    );
    assert!(
        snapshot.identity.tools.php_tool_bundle_sha256.is_some(),
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
    assert_eq!(
        calls.len(),
        expected.canonical_edges.len(),
        "every labelled canonical edge must be emitted exactly once"
    );
    assert_eq!(
        actual_edges, expected.canonical_edges,
        "canonical php call edges must match the labelled oracle exactly"
    );
    assert!(
        calls.iter().all(|edge| {
            edge.provenance.fact_source == FactSource::Phpactor
                && edge.provenance.confidence == 255
                && symbol_by_id.contains_key(edge.to.as_str())
        }),
        "every canonical edge must carry phpactor provenance at full confidence"
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

    let external = observation_names(snapshot, CallSiteCategory::ExternalCrate);
    assert_eq!(
        external, expected.external_observations,
        "external vendor calls must stay observations"
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
