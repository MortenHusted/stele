mod common;

use codemap_lemmaspec::{Checker, Options, project, project_readable};

fn options() -> Options {
    Options {
        checker: Checker::Symbols,
        spec_name: None,
        allow_dirty: false,
        min_call_coverage_per_mille: 1000,
        core_files: Vec::new(),
        presentation_files: Vec::new(),
        entry_points: vec!["rust:tests/it.rs#it_probes".into()],
        public_api: vec!["rust:src/core.rs#helper".into()],
    }
}

#[test]
fn readable_facts_have_explicit_bases_labels_notes_and_pinned_identity() {
    let snapshot = common::rust_snapshot(false);
    let text = project_readable(&snapshot, &options()).unwrap();
    assert!(text.starts_with("// Which shipped symbols have no path in the observed call graph?"));
    assert!(text.contains("notes { text:"));
    assert!(text.contains("Unsupported observations: unknown"));
    assert!(text.contains("symbol \"rust:function:src/core.rs#helper\" { label: \"helper (src/core.rs:5)\" source: \"./src/core.rs#L5\" }"));
    for line in text
        .lines()
        .filter(|line| line.trim_start().starts_with("fact "))
    {
        assert!(line.contains(" basis: "), "{line}");
        assert!(line.contains(" source: "), "{line}");
        if line.contains("relation: defines ")
            || line.contains("relation: calls ")
            || line.contains("relation: call_census ")
        {
            assert!(line.contains("basis: snapshot"), "{line}");
            assert!(
                line.contains(&format!("identity: \"{}\"", snapshot.identity.snapshot_id)),
                "{line}"
            );
        }
        if line.contains("relation: production_file ") || line.contains("relation: coverage_input ")
        {
            assert!(line.contains("basis: policy"), "{line}");
        }
        if line.contains("relation: public_api ")
            || (line.contains("relation: entry_point ") && line.contains(&common::it_probes_id()))
        {
            assert!(line.contains("basis: reviewer_declared"), "{line}");
        }
    }
    let legacy = project(&snapshot, &options()).unwrap();
    assert!(!legacy.contains(" basis: "));
    assert!(!legacy.contains("notes {"));
    assert!(!legacy.contains("\n  symbol "));
    assert_eq!(text, project_readable(&snapshot, &options()).unwrap());
}

#[test]
fn readable_check_and_bound_walk_are_byte_identical_to_legacy() {
    let snapshot = common::rust_snapshot(false);
    let directory = tempfile::tempdir().unwrap();
    let checker = directory.path().join("symbols.lemmaspec");
    std::fs::write(&checker, Checker::Symbols.source()).unwrap();
    let mut results = Vec::new();
    for (name, content) in [
        ("legacy", project(&snapshot, &options()).unwrap()),
        ("readable", project_readable(&snapshot, &options()).unwrap()),
    ] {
        let path = directory.path().join(format!("{name}.lemmaspec"));
        std::fs::write(&path, content).unwrap();
        let (code, stdout) = common::run_lemmaspec(&[
            "check",
            checker.to_str().unwrap(),
            path.to_str().unwrap(),
            "--json",
        ]);
        assert!(!stdout.is_empty(), "exit {code}");
        let (bind_code, bound) =
            common::run_lemmaspec(&["bind", checker.to_str().unwrap(), path.to_str().unwrap()]);
        assert_eq!(bind_code, 0, "{bound}");
        let bound_path = directory.path().join(format!("{name}-bound.lemmaspec"));
        std::fs::write(&bound_path, bound).unwrap();
        let walk = common::run_lemmaspec(&["walk", bound_path.to_str().unwrap(), "--json"]);
        results.push((code, stdout, walk));
    }
    assert_eq!(results[0], results[1]);
}
