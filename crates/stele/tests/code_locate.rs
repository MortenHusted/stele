#[path = "../../codemap/tests/common/mod.rs"]
mod common;

use std::process::{Command, Output};

fn locate(file: &std::path::Path, query: &str, json: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_stele"));
    command.args(["code", "locate", query, "--file"]).arg(file);
    if json {
        command.arg("--json");
    }
    command.output().unwrap()
}

#[test]
fn locate_uses_only_pinned_snapshot_and_never_classifies_executable_lines() {
    let root = common::make_repo_with(&[(
        "sample.rs".into(),
        "// module comment\nfn first() {\n    // body comment\n\n    let value = 1;\n}\n\nfn second() {}\n".into(),
    )])
    .unwrap();
    let snapshot = codemap::extract(&codemap::ExtractOptions {
        path: root.clone(),
        ..Default::default()
    })
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("snapshot.json");
    std::fs::write(&file, codemap::to_json(&snapshot).unwrap()).unwrap();
    std::fs::remove_dir_all(root).unwrap();

    for line in [2, 3, 4, 5, 6] {
        let output = locate(&file, &format!("sample.rs:{line}"), true);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["snapshot_id"], snapshot.identity.snapshot_id);
        assert_eq!(result["location"]["rel_path"], "sample.rs");
        assert_eq!(
            result["definitions"].as_array().unwrap().len(),
            1,
            "line {line}"
        );
        assert_eq!(result["definitions"][0]["name"], "first");
        assert_eq!(result["executable_classification"], "unsupported");
    }
    for query in [
        "sample.rs:1",
        "sample.rs:7",
        "sample.rs:99",
        "absent.rs:3",
        "sample.rs:5-8",
    ] {
        let output = locate(&file, query, true);
        assert!(output.status.success());
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["definitions"], serde_json::json!([]));
    }
    let text = locate(&file, "sample.rs:3", false);
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains(&snapshot.identity.snapshot_id));
    assert!(text.contains("sample.rs:3"));
    assert!(text.contains("first"));
    assert!(text.contains("Executable classification: unsupported"));

    let malformed = locate(&file, "../sample.rs:3", true);
    assert!(!malformed.status.success());
    assert_eq!(malformed.stdout, [] as [u8; 0]);
    std::fs::write(&file, "{}").unwrap();
    assert!(!locate(&file, "sample.rs:3", true).status.success());
}

#[test]
fn locate_requires_a_snapshot_file() {
    let output = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(["code", "locate", "sample.rs:3"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--file"));
}
