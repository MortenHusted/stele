//! Trait relations require lexical proof or exact compiler declarations.
use codemap::schema::{EdgeKind, Language};
use codemap::{ExtractOptions, extract};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
mod common;

fn relations(semantic: bool) -> BTreeSet<[String; 2]> {
    let source = include_str!("fixtures/rust_implements/lib.rs");
    assert_eq!(
        codemap::schema::sha256_hex(source.as_bytes()),
        "286f0fc7f28c44030dad2931397f13a8694e64f52c2b166304dac26110b28f34"
    );
    let repo = common::make_repo_with(&[
        ("src/lib.rs".into(), source.into()),
        ("Cargo.toml".into(), "[package]\nname = \"implementation_control\"\nversion = \"0.1.0\"\nedition = \"2024\"\n".into()),
        ("Cargo.lock".into(), "version = 4\n[[package]]\nname = \"implementation_control\"\nversion = \"0.1.0\"\n".into()),
    ]).unwrap();
    let snapshot = extract(&ExtractOptions {
        path: repo,
        language: Language::Rust,
        calls_required: semantic,
        measures: false,
        twins: false,
    })
    .unwrap();
    let symbols: BTreeMap<_, _> = snapshot
        .nodes
        .iter()
        .map(|node| {
            (
                node.id.as_str(),
                node.symbol_key
                    .split_once('#')
                    .map_or("", |(_, symbol)| symbol),
            )
        })
        .collect();
    let edges: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Implements)
        .map(|edge| {
            [
                symbols[edge.from.as_str()].to_string(),
                symbols[edge.to.as_str()].to_string(),
            ]
        })
        .collect();
    let headers = BTreeMap::from([
        (
            ["positive::Item", "positive::Marker"],
            "impl Marker for Item {}",
        ),
        (
            ["positive::Item", "decoy::Write"],
            "impl crate::decoy::Write for crate::positive::Item {}",
        ),
        (
            ["aliased::Thing", "positive::Marker"],
            "impl Tag for Thing {}",
        ),
        (
            ["shapes_b::Square", "shapes_b::Shape"],
            "impl Shape for Square {}",
        ),
    ]);
    for edge in snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Implements)
    {
        assert_eq!(edge.provenance.rel_path, "src/lib.rs");
        assert_eq!(
            edge.provenance.fact_source,
            if semantic {
                codemap::schema::FactSource::RustAnalyzer
            } else {
                codemap::schema::FactSource::TreeSitter
            }
        );
        let span = edge.provenance.span;
        assert_eq!(
            &source[span.start_byte..span.end_byte],
            headers[&[symbols[edge.from.as_str()], symbols[edge.to.as_str()]]]
        );
    }
    let unique = edges.iter().cloned().collect::<BTreeSet<_>>();
    assert_eq!(unique.len(), edges.len(), "duplicate implementation facts");
    unique
}

#[test]
fn syntax_implements_requires_both_declarations_in_the_lexical_module() {
    assert_eq!(
        relations(false),
        BTreeSet::from([
            ["positive::Item".into(), "positive::Marker".into()],
            ["shapes_b::Square".into(), "shapes_b::Shape".into()],
        ])
    );
}

#[test]
#[ignore = "requires the prepared rust-analyzer and rust-src operator"]
fn semantic_implements_matches_the_source_reviewed_bindings() {
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/rust_implements/expected.json")).unwrap();
    let required: BTreeSet<[String; 2]> =
        serde_json::from_value(expected["required"].clone()).unwrap();
    assert_eq!(relations(true), required);
}

#[test]
fn a_function_local_trait_does_not_bind_a_module_level_implementation() {
    let source = "#![allow(dead_code)]\nstruct Item;\nfn hidden() { trait Default {} }\nimpl Default for Item { fn default() -> Self { Item } }\n";
    let repo = common::make_repo_with(&[("lib.rs".into(), source.into())]).unwrap();
    let snapshot = extract(&ExtractOptions {
        path: repo,
        language: Language::Rust,
        calls_required: false,
        measures: false,
        twins: false,
    })
    .unwrap();
    assert_eq!(
        snapshot
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Implements)
            .count(),
        0
    );
}
