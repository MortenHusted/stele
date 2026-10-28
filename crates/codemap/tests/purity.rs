//! P0 language-purity gate: only tracked `*.rs` files produce facts.
//!
//! The corpus deliberately includes `helper.rb`, `helper.ts`, `helper.php`,
//! and `helper.go`. This test asserts that no node, file record, or
//! provenance references those files.

use std::path::Path;
use std::path::PathBuf;

use codemap::{ExtractOptions, extract};

mod common;

fn is_rust_file(rel_path: &str) -> bool {
    Path::new(rel_path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
}

#[test]
fn only_rust_files_produce_facts() {
    let files = common::corpus_files();
    let repo = common::make_repo_with(&files).expect("create corpus repo");
    let snapshot = extract(&ExtractOptions {
        path: PathBuf::from(&repo),
        calls_required: false,
        measures: false,
        twins: false,
        language: codemap::schema::Language::Rust,
    })
    .expect("extract corpus");

    // Every node id must be in the rust: namespace.
    for node in &snapshot.nodes {
        assert!(
            node.id.starts_with("rust:"),
            "non-rust node id: {}",
            node.id
        );
    }

    // Only `.rs` files are recorded in the snapshot file inventory.
    for file in &snapshot.files {
        assert!(
            is_rust_file(&file.rel_path),
            "non-Rust file in snapshot: {}",
            file.rel_path
        );
    }

    // No provenance may reference the mixed-language helpers.
    let is_helper = |rel_path: &str| {
        ["helper.rb", "helper.ts", "helper.php", "helper.go"]
            .iter()
            .any(|helper| rel_path.ends_with(helper))
    };
    for node in &snapshot.nodes {
        assert!(
            !is_helper(&node.provenance.rel_path),
            "node provenance references non-Rust file: {}",
            node.provenance.rel_path
        );
    }
    for edge in &snapshot.edges {
        assert!(
            !is_helper(&edge.provenance.rel_path),
            "edge provenance references non-Rust file: {}",
            edge.provenance.rel_path
        );
    }

    // The tracked helpers are present in the repo but absent from facts.
    let helper_files = ["helper.rb", "helper.ts", "helper.php", "helper.go"];
    for helper in &helper_files {
        assert!(
            snapshot.files.iter().all(|f| f.rel_path != *helper),
            "helper file must not appear in snapshot files: {helper}"
        );
        assert!(
            snapshot
                .nodes
                .iter()
                .all(|n| n.provenance.rel_path != *helper),
            "helper file must not be referenced by nodes: {helper}"
        );
    }
}
