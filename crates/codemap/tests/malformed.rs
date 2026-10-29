//! P0 malformed-file gate: a file with parse errors is reported but does
//! not panic or fail extraction.

use std::path::PathBuf;

use codemap::{ExtractOptions, extract};

mod common;

#[test]
fn malformed_file_reports_parse_errors() {
    let broken = include_str!("../tests/fixtures/malformed/broken.rs");
    let repo = common::make_repo_with(&[
        ("malformed/broken.rs".into(), broken.into()),
        ("src/lib.rs".into(), "pub fn valid() {}".into()),
    ])
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

#[test]
fn parse_error_report_identifies_only_the_malformed_source() {
    let broken = include_str!("fixtures/malformed/broken.rs");
    let repo = common::make_repo_with(&[
        ("malformed/broken.rs".into(), broken.into()),
        ("src/lib.rs".into(), "pub fn valid() {}".into()),
    ])
    .unwrap();
    let snapshot = extract(&ExtractOptions {
        path: repo,
        calls_required: false,
        measures: false,
        twins: false,
        language: codemap::schema::Language::Rust,
    })
    .unwrap();
    let json = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(
        json["report"]["parse_error_files"],
        serde_json::json!({
            "malformed/broken.rs": snapshot.report.parse_errors
        })
    );
}
