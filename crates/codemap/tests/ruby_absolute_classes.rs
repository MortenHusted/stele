//! Global declaration identity and lexical nesting are distinct in Ruby.
use std::collections::{BTreeMap, BTreeSet};

use codemap::schema::{EdgeKind, Language, NodeKind, Snapshot, TypeKind};
use codemap::{ExtractOptions, extract};
use serde_json::Value;

mod common;

fn snapshot(semantic: bool) -> Snapshot {
    let source = include_str!("fixtures/ruby_absolute/absolute.rb");
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/ruby_absolute/expected.json")).unwrap();
    assert_eq!(
        codemap::schema::sha256_hex(source.as_bytes()),
        expected["source_sha256"]
    );
    let repo = common::make_repo_with(&[("lib/absolute.rb".into(), source.into())]).unwrap();
    extract(&ExtractOptions {
        path: repo,
        language: Language::Ruby,
        calls_required: semantic,
        measures: false,
        twins: false,
    })
    .unwrap()
}

fn symbol(snapshot: &Snapshot, id: &str) -> String {
    snapshot
        .nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap()
        .symbol_key
        .split_once('#')
        .unwrap()
        .1
        .to_string()
}

#[test]
fn absolute_declarations_keep_global_identity_and_lexical_lookup() {
    let snapshot = snapshot(false);
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/ruby_absolute/expected.json")).unwrap();
    let nodes: BTreeMap<_, _> = snapshot
        .nodes
        .iter()
        .filter(|node| node.kind != NodeKind::File)
        .map(|node| {
            let kind = match node.kind {
                NodeKind::Type(TypeKind::Class) => "Class",
                NodeKind::Module => "Module",
                NodeKind::Method => "Method",
                _ => panic!("unexpected fixture node"),
            };
            (symbol(&snapshot, &node.id), kind.to_string())
        })
        .collect();
    let expected_nodes: BTreeMap<_, _> = expected["nodes"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, kind)| (name.clone(), kind.as_str().unwrap().to_string()))
        .collect();
    assert_eq!(nodes.len(), snapshot.nodes.len() - 1);
    assert_eq!(nodes, expected_nodes);
    let heritage: BTreeSet<_> = snapshot
        .edges
        .iter()
        .filter(|edge| {
            matches!(
                edge.kind,
                EdgeKind::Inherits | EdgeKind::Implements | EdgeKind::Extends
            )
        })
        .map(|edge| {
            [
                format!("{:?}", edge.kind),
                symbol(&snapshot, &edge.from),
                symbol(&snapshot, &edge.to),
            ]
        })
        .collect();
    let expected_heritage: BTreeSet<[String; 3]> =
        serde_json::from_value(expected["heritage"].clone()).unwrap();
    assert_eq!(heritage, expected_heritage);
}

#[test]
#[ignore = "requires the pinned Ruby-LSP operator bundle"]
fn ruby_lsp_0_26_10_omits_implicit_calls_in_nested_absolute_bodies() {
    let snapshot = snapshot(true);
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/ruby_absolute/expected.json")).unwrap();
    let mut calls: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
        .map(|edge| [symbol(&snapshot, &edge.from), symbol(&snapshot, &edge.to)])
        .collect();
    calls.sort();
    // Provider evidence, not the source oracle: 0.26.10 constructs the
    // implicit self type as Outer::::Top. Keep all seven truth labels in
    // expected.json; the production admission unit test checks those answers.
    assert_eq!(snapshot.identity.tools.ruby_lsp.as_deref(), Some("0.26.10"));
    let expected_calls: Vec<[String; 2]> =
        serde_json::from_value(expected["calls"].clone()).unwrap();
    let answered: Vec<_> = expected_calls
        .into_iter()
        .filter(|pair| pair[1] == "AbsoluteSpace::OtherTop::self.ping")
        .collect();
    assert_eq!(calls, answered);
    let source = include_str!("fixtures/ruby_absolute/absolute.rb");
    let expected_sites: Vec<_> = [
        (38, 6, "root_marker", "Unmapped"),
        (39, 6, "lexical_marker", "Unmapped"),
        (40, 6, "own_marker", "Unmapped"),
        (41, 6, "base_marker", "Unmapped"),
        (42, 6, "ping", "Resolved"),
        (48, 6, "root_marker", "Unmapped"),
        (60, 6, "lexical_marker", "Unmapped"),
    ]
    .into_iter()
    .map(|(line, column, name, category)| {
        (
            "lib/absolute.rb",
            source
                .split_inclusive('\n')
                .take(line - 1)
                .map(str::len)
                .sum::<usize>()
                + column,
            name,
            category,
        )
    })
    .collect();
    let actual_sites: Vec<_> = snapshot
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
    assert_eq!(actual_sites, expected_sites);
}
