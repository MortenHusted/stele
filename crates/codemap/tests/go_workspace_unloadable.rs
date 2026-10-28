//! G2 fail-closed gate: a workspace gopls cannot load (a dependency the
//! operator neither vendored nor cached) aborts `--calls=required` rather
//! than resolving against the packages that did load.
//!
//! Ignored by default: requires a gopls tool bundle via
//! `CODEMAP_GO_TOOL_BUNDLE`. Run with `cargo test -- --ignored`.

use std::path::PathBuf;

use codemap::schema::Language;
use codemap::{ExtractOptions, extract};

mod common;

#[test]
#[ignore = "requires a gopls tool bundle via CODEMAP_GO_TOOL_BUNDLE"]
fn a_workspace_with_a_missing_dependency_is_refused() {
    let files = vec![
        (
            "go.mod".to_string(),
            "module example.invalid/app\n\ngo 1.25\n\nrequire example.invalid/missing v1.0.0\n"
                .to_string(),
        ),
        (
            "app.go".to_string(),
            "package app\n\nimport \"example.invalid/missing\"\n\nfunc Run() { missing.Do() }\n"
                .to_string(),
        ),
    ];
    let repo = common::make_repo_with(&files).expect("create go repo");
    let error = extract(&ExtractOptions {
        path: PathBuf::from(&repo),
        calls_required: true,
        measures: false,
        twins: false,
        language: Language::Go,
    })
    .expect_err("a workspace gopls cannot load must be refused");
    let message = format!("{error:#}");
    assert!(
        message.contains("could not load the workspace") && message.contains("missing"),
        "{message}"
    );
}
