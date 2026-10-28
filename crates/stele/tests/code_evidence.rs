//! The evidence projection under `stele code`: `evidence` and
//! `positive-path` write their outputs, bound their options and refuse to
//! overwrite what they read.

#[path = "../../codemap-lemmaspec/tests/common/mod.rs"]
mod common;

use codemap_lemmaspec::positive_path::project;

#[test]
fn cli_bounds_the_admission_override() {
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_stele"))
        .args([
            "code",
            "evidence",
            "--file",
            "unused.json",
            "--min-call-coverage-per-mille",
            "1001",
        ])
        .output()
        .expect("CLI");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("1001"));
}

#[test]
fn cli_writes_selection_and_rejects_output_aliases_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = dir.path().join("snapshot.json");
    let evidence = dir.path().join("evidence.lemmaspec");
    let selection = dir.path().join("selection.json");
    std::fs::write(
        &snapshot,
        serde_json::to_vec(&common::rust_snapshot(false)).unwrap(),
    )
    .unwrap();
    let run = |destination: &std::path::Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_stele"))
            .args([
                "code",
                "evidence",
                "--checker",
                "symbols",
                "--readable",
                "--file",
            ])
            .arg(&snapshot)
            .arg("--out")
            .arg(&evidence)
            .arg("--selection-out")
            .arg(destination)
            .output()
            .unwrap()
    };
    let output = run(&selection);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&selection).unwrap()).unwrap();
    assert_eq!(
        value["entry_points"],
        serde_json::json!([common::main_id()])
    );
    let before = std::fs::read(&evidence).unwrap();
    let output = run(&dir.path().join("./evidence.lemmaspec"));
    assert!(!output.status.success());
    assert_eq!(before, std::fs::read(&evidence).unwrap());
}

#[test]
fn cli_projects_exact_nodes_and_refuses_to_overwrite_the_snapshot() {
    let (snapshot, options, _) = common::positive_path_fixture();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.lemmaspec");
    let output = dir.path().join("positive.lemmaspec");
    let original = serde_json::to_vec(&snapshot).unwrap();
    std::fs::write(&input, &original).unwrap();
    let run = |destination: &std::path::Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_stele"))
            .args(["code", "positive-path"])
            .arg("--file")
            .arg(&input)
            .arg("--entry-node")
            .arg(&options.entry_node)
            .arg("--target-node")
            .arg(&options.target_node)
            .arg("--out")
            .arg(destination)
            .output()
            .unwrap()
    };
    let result = run(&output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        project(&snapshot, &options).unwrap()
    );
    let rejected = run(&input);
    assert!(!rejected.status.success());
    assert_eq!(std::fs::read(&input).unwrap(), original);
    let hardlink = dir.path().join("hardlink.lemmaspec");
    std::fs::hard_link(&input, &hardlink).unwrap();
    assert!(!run(&hardlink).status.success());
    assert!(!run(&dir.path().join("./input.lemmaspec")).status.success());
    #[cfg(unix)]
    {
        let symlink = dir.path().join("symlink.lemmaspec");
        std::os::unix::fs::symlink(&input, &symlink).unwrap();
        assert!(!run(&symlink).status.success());
    }
    assert_eq!(std::fs::read(&input).unwrap(), original);
}
