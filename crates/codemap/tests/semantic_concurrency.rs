//! Concurrency determinism gate: resolving definitions across many workers
//! must produce the same bytes as resolving them one at a time.
//!
//! The existing gate re-extracts the same commit in two independent checkouts,
//! which proves snapshot bytes do not depend on *where* the tree lives. This
//! proves they do not depend on *what order the server answered in* — the risk
//! the concurrent client introduces, and the only one that could let wall-clock
//! scheduling reach a fact.

use std::path::Path;

use codemap::{ExtractOptions, extract, to_json};

mod common;

fn extract_with_workers(repo: &Path, workers: &str) -> String {
    // SAFETY-adjacent note: the worker count is read once per extraction, and
    // these two runs are sequential, so setting it here cannot race a reader.
    unsafe { std::env::set_var("CODEMAP_DEFINITION_WORKERS", workers) };
    let snapshot = extract(&ExtractOptions {
        path: repo.to_path_buf(),
        calls_required: true,
        measures: false,
        twins: false,
        language: codemap::schema::Language::Rust,
    })
    .unwrap_or_else(|error| panic!("semantic extraction with {workers} worker(s): {error:#}"));
    to_json(&snapshot).expect("serialize semantic snapshot")
}

/// Requires rust-analyzer on PATH; ignored so `cargo test` stays hermetic.
/// Run with `cargo test -- --ignored`.
#[test]
#[ignore = "requires rust-analyzer on PATH"]
fn concurrent_and_sequential_resolution_are_byte_identical() {
    let repo = common::make_calls_cargo_repo().expect("create calls cargo repo");

    let sequential = extract_with_workers(&repo, "1");
    let concurrent = extract_with_workers(&repo, "8");
    unsafe { std::env::remove_var("CODEMAP_DEFINITION_WORKERS") };

    assert_eq!(
        sequential, concurrent,
        "definition_batch returns answers in request order, so the number of \
         workers must not change a single byte of the snapshot"
    );
}
