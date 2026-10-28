//! Each `stele` group reaches the command line it wraps.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use codemap_lemmaspec::Checker;

fn stele(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(args)
        .output()
        .expect("run stele")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A validated Rust snapshot the topology tests already carry.
fn snapshot() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../codemap-topology/tests/fixtures/rust.json")
}

#[test]
fn help_names_every_group() {
    let output = stele(&["--help"]);
    assert!(output.status.success());
    let help = stdout(&output);
    for group in ["code", "spec", "slop", "topology"] {
        assert!(help.contains(&format!("  {group} ")), "{help}");
    }
}

#[test]
fn code_runs_codemap_and_its_evidence_commands() {
    let snapshot = snapshot();
    let output = stele(&["code", "validate", "--file", snapshot.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert!(stdout(&output).starts_with("ok: "), "{output:?}");

    let output = stele(&["code", "checker", "symbols"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stdout(&output), Checker::Symbols.source());

    let help = stdout(&stele(&["code", "--help"]));
    for command in ["extract", "impact", "evidence", "positive-path", "checker"] {
        assert!(help.contains(&format!("  {command} ")), "{help}");
    }
}

#[test]
fn code_search_prints_each_hits_kind_name_file_and_symbol_key() {
    let snapshot = snapshot();
    let search = |needle: &str| {
        let output = stele(&[
            "code",
            "search",
            needle,
            "--file",
            snapshot.to_str().unwrap(),
        ]);
        assert!(output.status.success(), "{output:?}");
        stdout(&output)
    };
    assert_eq!(
        search("entry"),
        "function  entry  [src/main.rs]  rust:src/main.rs#entry\n"
    );
    assert_eq!(
        search("rust:src/main.rs#work"),
        "function  work  [src/main.rs]  rust:src/main.rs#work\n"
    );
    assert_eq!(search("nowhere"), "no symbols matching 'nowhere'\n");
}

#[test]
fn code_errors_keep_codemap_exit_status() {
    let output = stele(&["code", "validate", "--file", "does-not-exist.json"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("stele code: "));
}

#[test]
fn evidence_slop_and_topology_errors_name_their_group() {
    let directory = tempfile::tempdir().expect("tempdir");
    let out = directory.path().join("topology.json");
    for (args, prefix) in [
        (
            vec!["code", "evidence", "--file", "does-not-exist.json"],
            "stele code: ",
        ),
        (
            vec!["slop", "report", "--file", "does-not-exist.json"],
            "stele slop: ",
        ),
        (
            vec![
                "topology",
                "--snapshot",
                "does-not-exist.json",
                "--out",
                out.to_str().unwrap(),
            ],
            "stele topology: ",
        ),
    ] {
        let output = stele(&args);
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with(prefix), "{stderr}");
    }
}

#[test]
fn a_dirty_worktree_names_the_way_to_check_it() {
    let repository = tempfile::tempdir().expect("tempdir");
    std::fs::write(repository.path().join("lib.rs"), "pub fn answer() {}\n").unwrap();
    for args in [
        &["init", "-q"][..],
        &["add", "-A"],
        &[
            "-c",
            "user.email=stele@example.com",
            "-c",
            "user.name=stele",
            "commit",
            "-qm",
            "start",
        ],
    ] {
        let status = Command::new("git")
            .args(args)
            .current_dir(repository.path())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?}");
    }
    std::fs::write(repository.path().join("lib.rs"), "pub fn changed() {}\n").unwrap();
    let path = repository.path().to_str().unwrap();
    for (args, prefix) in [
        (vec!["code", "extract", "--path", path], "stele code: "),
        (vec!["check", "hotspots", "--path", path], "stele check: "),
    ] {
        let stderr = String::from_utf8_lossy(&stele(&args).stderr).into_owned();
        assert!(stderr.starts_with(prefix), "{stderr}");
        assert!(stderr.contains("worktree is dirty"), "{stderr}");
        assert!(stderr.contains("stele code extract --dirty"), "{stderr}");
        assert!(stderr.contains("--allow-dirty"), "{stderr}");
    }
}

#[test]
fn spec_passes_every_word_to_the_engine() {
    let output = stele(&["spec", "--help"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("stele spec walk <path.stele> [--json]"));

    let output = stele(&["spec"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
}

#[test]
fn slop_and_topology_reach_their_commands() {
    let help = stdout(&stele(&["slop", "--help"]));
    for command in ["report", "diff", "gate", "series"] {
        assert!(help.contains(&format!("  {command} ")), "{help}");
    }

    let directory = tempfile::tempdir().expect("tempdir");
    let out = directory.path().join("topology.json");
    let snapshot = snapshot();
    let output = stele(&[
        "topology",
        "--snapshot",
        snapshot.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");
    assert!(out.is_file());
}

#[test]
fn code_evidence_writes_a_stele_file_and_names_the_stele_commands() {
    let directory = tempfile::tempdir().expect("tempdir");
    let out = directory.path().join("evidence.stele");
    let snapshot = snapshot();
    let output = stele(&[
        "code",
        "evidence",
        "--file",
        snapshot.to_str().unwrap(),
        "--checker",
        "architecture",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");
    assert!(out.is_file());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("stele spec check architecture.stele"),
        "{stderr}"
    );

    let refused = stele(&[
        "code",
        "evidence",
        "--file",
        snapshot.to_str().unwrap(),
        "--out",
        directory.path().join("evidence.txt").to_str().unwrap(),
    ]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("must end in .stele"));
}
