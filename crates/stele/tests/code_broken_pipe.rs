#[path = "../../codemap/tests/common/mod.rs"]
mod common;

use std::fmt::Write;
use std::io::Read;
use std::process::{Command, Stdio};

fn close_stdout(command: &mut Command) -> std::process::Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut first = [0];
    stdout.read_exact(&mut first).unwrap();
    drop(stdout);
    child.wait_with_output().unwrap()
}

#[test]
fn closed_stdout_finishes_code_output_without_a_panic() {
    let mut source = String::new();
    for n in 0..3000 {
        writeln!(
            source,
            "fn function_{n:04}_with_a_long_name_for_pipe_output() {{}}"
        )
        .unwrap();
    }
    let root = common::make_repo_with(&[("sample.rs".into(), source)]).unwrap();
    let snapshot = codemap::extract(&codemap::ExtractOptions {
        path: root.clone(),
        ..Default::default()
    })
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("snapshot.json");
    std::fs::write(&file, codemap::to_json(&snapshot).unwrap()).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    for subcommand in ["processes", "structure", "search"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_stele"));
        command.args(["code", subcommand]);
        if subcommand == "search" {
            command.arg("function");
        }
        command.arg("--file").arg(&file);
        let output = close_stdout(&mut command);
        assert!(
            output.status.success(),
            "{subcommand}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
    }
}

#[test]
fn closed_stdout_does_not_hide_input_failures() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(["code", "processes", "--file"])
        .arg(temp.path().join("absent.json"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("stele code:"));
}
