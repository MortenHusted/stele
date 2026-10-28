//! Call-impact fixture smoke test.
//!
//! The `tests/fixtures/calls/` corpus documents labelled call sites for the
//! future P2 semantic phase. In P1 syntax mode all sites are observations;
//! this test ensures the fixture extracts without errors and that no
//! canonical `calls` edges are emitted.

use std::fs;
use std::path::PathBuf;

use codemap::schema::EdgeKind;
use codemap::{ExtractOptions, extract};

mod common;

#[test]
fn calls_fixture_extracts_as_observations_only() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/calls");
    let mut files = Vec::new();
    for entry in fs::read_dir(dir.join("src")).expect("read calls src dir") {
        let entry = entry.expect("calls src entry");
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("rs") {
            let rel = PathBuf::from("src").join(path.file_name().unwrap());
            let content = fs::read_to_string(&path).expect("read calls source");
            files.push((rel.to_string_lossy().to_string(), content));
        }
    }

    let repo = common::make_repo_with(&files).expect("create calls repo");
    let snapshot = extract(&ExtractOptions {
        path: PathBuf::from(&repo),
        calls_required: false,
        measures: false,
        twins: false,
        language: codemap::schema::Language::Rust,
    })
    .expect("extract calls fixture");

    assert!(
        !snapshot.call_sites.is_empty(),
        "fixture must produce call-site observations"
    );
    assert!(
        snapshot.edges.iter().all(|e| e.kind != EdgeKind::Calls),
        "syntax mode must not emit calls edges"
    );
}
