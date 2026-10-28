//! Source-declared Go targets distinguish declarations from same-name bindings.

use std::collections::{BTreeMap, BTreeSet};

use codemap::schema::{CallSiteCategory, EdgeKind, Language};
use codemap::{ExtractOptions, extract};
use serde::Deserialize;

mod common;

#[derive(Deserialize)]
struct Expected {
    canonical_edges: BTreeSet<(String, String)>,
    forbidden_callers: BTreeSet<String>,
    candidate_observations: Vec<String>,
    noncanonical_observations: Vec<String>,
}

#[test]
#[ignore = "requires a gopls tool bundle via CODEMAP_GO_TOOL_BUNDLE"]
fn go_semantic_calls_require_exact_callable_declarations() {
    let expected: Expected =
        serde_json::from_str(include_str!("fixtures/go_callable_targets/expected.json"))
            .expect("parse intended calls");
    let files = [
        (
            "go.mod",
            include_str!("fixtures/go_callable_targets/go.mod"),
        ),
        (
            "targets.go",
            include_str!("fixtures/go_callable_targets/targets.go"),
        ),
    ]
    .map(|(path, content)| (path.to_owned(), content.to_owned()));
    let repo = common::make_repo_with(&files).expect("create Go declaration fixture");
    let snapshot = extract(&ExtractOptions {
        path: repo,
        calls_required: true,
        measures: false,
        twins: false,
        language: Language::Go,
    })
    .expect("extract Go declarations");
    let names: BTreeMap<_, _> = snapshot
        .nodes
        .iter()
        .map(|node| {
            (
                node.id.as_str(),
                node.symbol_key
                    .strip_prefix("go:targets.go#targets::")
                    .unwrap_or(&node.symbol_key),
            )
        })
        .collect();
    let calls: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
        .collect();
    let actual: BTreeSet<_> = calls
        .iter()
        .map(|edge| {
            (
                names[edge.from.as_str()].to_owned(),
                names[edge.to.as_str()].to_owned(),
            )
        })
        .collect();
    assert_eq!(actual, expected.canonical_edges);
    assert_eq!(
        calls.len(),
        expected.canonical_edges.len(),
        "duplicates must not hide behind pair equality"
    );
    assert!(
        actual
            .iter()
            .all(|(from, _)| !expected.forbidden_callers.contains(from))
    );
    for (labels, category) in [
        (
            &expected.noncanonical_observations,
            CallSiteCategory::Unmapped,
        ),
        (
            &expected.candidate_observations,
            CallSiteCategory::Candidate,
        ),
    ] {
        for name in labels {
            let sites: Vec<_> = snapshot
                .call_sites
                .iter()
                .filter(|site| &site.raw_name == name)
                .collect();
            assert_eq!(sites.len(), 1, "missing or duplicate observation: {name}");
            assert_eq!(sites[0].category, category, "wrong category for {name}");
        }
    }
    assert_eq!(
        snapshot
            .call_sites
            .iter()
            .filter(|site| site.category == CallSiteCategory::Resolved)
            .count(),
        expected.canonical_edges.len()
    );
}
