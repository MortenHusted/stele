mod common;

use codemap_lemmaspec::{Checker, Options, project_output};

fn options() -> Options {
    Options {
        checker: Checker::Symbols,
        min_call_coverage_per_mille: 750,
        spec_name: None,
        allow_dirty: false,
        core_files: Vec::new(),
        presentation_files: Vec::new(),
        entry_points: vec![
            "rust:tests/it.rs#it_probes".into(),
            "rust:src/main.rs#main".into(),
        ],
        public_api: vec![
            "rust:src/core.rs#helper".into(),
            "rust:src/core.rs#helper".into(),
        ],
    }
}

#[test]
fn selections_are_expanded_canonical_ids_from_the_same_projection() {
    let snapshot = common::rust_snapshot(false);
    let mut entries = vec![common::main_id(), common::it_probes_id()];
    entries.sort();
    let result = project_output(&snapshot, &options(), true).unwrap();
    assert_eq!(
        result.selection.unwrap(),
        serde_json::json!({
            "entry_policy": "policy:entry-point/rust", "coverage_policy": "observed-call-coverage/v1",
            "min_call_coverage_per_mille": 750, "entry_points": entries, "public_api": [common::helper_id()],
        })
    );
    for id in entries {
        assert!(
            result
                .evidence
                .contains(&format!("relation: entry_point args: [\"{id}\"]"))
        );
    }
}
