//! P0 malformed-file gate: a file with parse errors is reported but does
//! not panic or fail extraction.

use std::path::PathBuf;

use codemap::{ExtractOptions, extract};

mod common;

#[test]
fn malformed_file_reports_parse_errors() {
    let broken = include_str!("../tests/fixtures/malformed/broken.rs");
    let repo = common::make_repo_with(&[("malformed/broken.rs".into(), broken.into())])
        .expect("create malformed repo");

    let snapshot = extract(&ExtractOptions {
        path: PathBuf::from(&repo),
        calls_required: false,
        measures: false,
        twins: false,
        language: codemap::schema::Language::Rust,
    })
    .expect("extraction must succeed despite parse errors");

    assert!(
        snapshot.report.parse_errors > 0,
        "malformed file must report parse_errors > 0"
    );
}
