#[path = "../../codemap/tests/common/mod.rs"]
mod common;
#[path = "../../codemap-lemmaspec/tests/common/mod.rs"]
mod observed;

use std::collections::BTreeMap;

use codemap_lemmaspec::coverage::{Coverage, DEFAULT_MIN_PER_MILLE};

/// Each file's only error sits where the extraction walker never looks: a
/// signature that loses its `)` and a where-clause with a stray `+,`.
/// Counting at the walker reported no parse errors here, so an otherwise
/// admitted snapshot passed `Errors < 1`.
#[test]
fn errors_hidden_from_the_walker_count_and_refuse_admission() {
    let root = common::make_repo_with(&[
        (
            "src/signature.rs".into(),
            "pub fn ok() {}\n\npub fn broken(value: u32 {\n    let _ = value;\n}\n".into(),
        ),
        (
            "src/where_clause.rs".into(),
            "pub fn bounded<T>(value: T) -> T\nwhere\n    T: Clone +,,\n{\n    value\n}\n".into(),
        ),
    ])
    .unwrap();
    let snapshot = codemap::extract(&codemap::ExtractOptions {
        path: root,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        snapshot.report.parse_error_files,
        BTreeMap::from([
            ("src/signature.rs".into(), 1),
            ("src/where_clause.rs".into(), 1),
        ])
    );

    let mut admitted = observed::rust_snapshot(false);
    assert!(Coverage::from_snapshot(&admitted).admitted(DEFAULT_MIN_PER_MILLE));
    admitted.report.parse_errors = snapshot.report.parse_errors;
    assert!(!Coverage::from_snapshot(&admitted).admitted(DEFAULT_MIN_PER_MILLE));
}
