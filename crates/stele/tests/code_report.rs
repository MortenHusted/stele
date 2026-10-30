#[path = "../../codemap/tests/common/mod.rs"]
mod common;

use std::process::Command;

#[test]
fn report_names_where_each_file_first_fails_to_parse() {
    let root = common::make_repo_with(&[
        (
            "malformed/broken.rs".into(),
            include_str!("../../codemap/tests/fixtures/malformed/broken.rs").into(),
        ),
        ("src/lib.rs".into(), "pub fn valid() {}\n".into()),
    ])
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

    let output = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(["code", "report", "--file"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("  malformed/broken.rs: 1 parse errors; first at 4:16 (columns from 0)\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("src/lib.rs"), "{stdout}");
}
