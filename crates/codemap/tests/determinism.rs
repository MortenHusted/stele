//! P0 determinism gate: re-extracting the same clean commit produces
//! byte-identical canonical JSON and a stable snapshot ID.

#![allow(clippy::doc_markdown)] // Example tags name catalog IDs, not code.

use std::path::PathBuf;

use codemap::{ExtractOptions, extract, to_json};

mod common;

/// intent: example extraction_is_byte_identical exemplifies same_commit_same_facts on cli
#[test]
fn two_extractions_are_byte_identical() {
    let files = common::corpus_files();
    let repo = common::make_repo_with(&files).expect("create corpus repo");

    let opts = ExtractOptions {
        path: PathBuf::from(&repo),
        calls_required: false,
        measures: false,
        twins: false,
        language: codemap::schema::Language::Rust,
    };

    let first = extract(&opts).expect("first extraction");
    let second = extract(&opts).expect("second extraction");

    let json_a = to_json(&first).expect("serialize first snapshot");
    let json_b = to_json(&second).expect("serialize second snapshot");

    assert_eq!(
        json_a, json_b,
        "canonical JSON must be byte-identical across two extractions"
    );
    assert_eq!(
        first.identity.snapshot_id, second.identity.snapshot_id,
        "snapshot_id must be stable across two extractions"
    );
}
