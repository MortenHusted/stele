//! `stele check` gives the same report as the manual sequence it replaces:
//! `stele code evidence`, `stele code checker` and `stele spec check`.

#![allow(clippy::doc_markdown)] // Example tags name catalog IDs, not code.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn stele(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(args)
        .output()
        .expect("run stele")
}

/// A validated Rust snapshot with verified calls, from the topology tests.
fn snapshot() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../codemap-topology/tests/fixtures/rust.json")
}

fn manual(checker: &str, snapshot: &Path, json: bool) -> Output {
    let directory = tempfile::tempdir().expect("tempdir");
    let evidence = directory.path().join("evidence.stele");
    let rules = directory.path().join(format!("{checker}.stele"));
    let projected = stele(&[
        "code",
        "evidence",
        "--file",
        snapshot.to_str().unwrap(),
        "--checker",
        checker,
        "--out",
        evidence.to_str().unwrap(),
    ]);
    assert!(projected.status.success(), "{projected:?}");
    let printed = stele(&["code", "checker", checker, "--out", rules.to_str().unwrap()]);
    assert!(printed.status.success(), "{printed:?}");
    let mut args = vec![
        "spec",
        "check",
        rules.to_str().unwrap(),
        evidence.to_str().unwrap(),
    ];
    if json {
        args.push("--json");
    }
    stele(&args)
}

/// intent: example check_equals_manual_steps exemplifies check_matches_manual_steps on cli
#[test]
fn one_step_matches_the_manual_sequence_byte_for_byte() {
    let snapshot = snapshot();
    for checker in ["architecture", "symbols"] {
        for json in [false, true] {
            let expected = manual(checker, &snapshot, json);
            let mut args = vec!["check", checker, "--snapshot", snapshot.to_str().unwrap()];
            if json {
                args.push("--json");
            }
            let actual = stele(&args);
            assert_eq!(actual.status.code(), expected.status.code(), "{checker}");
            assert_eq!(actual.stdout, expected.stdout, "{checker} json={json}");
            assert!(actual.stderr.is_empty(), "{actual:?}");
        }
    }
}

/// intent: example check_open_claim_exits_one exemplifies exit_status_contract on cli
#[test]
fn an_open_expectation_exits_one_and_names_it() {
    let snapshot = snapshot();
    let output = stele(&["check", "symbols", "--snapshot", snapshot.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("[FAIL] coverage_is_admitted"), "{report}");
}

/// intent: example check_error_exits_two exemplifies exit_status_contract on cli
#[test]
fn errors_exit_two_with_the_reason() {
    let snapshot = snapshot();
    let output = stele(&[
        "check",
        "hotspots",
        "--snapshot",
        snapshot.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("stele check: "));

    let not_a_repository = tempfile::tempdir().expect("tempdir");
    let output = stele(&[
        "check",
        "symbols",
        "--path",
        not_a_repository.path().to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("stele check: "));
}

/// intent: example check_refuses_without_resolver exemplifies missing_resolver_refuses on cli
#[test]
fn a_check_that_needs_calls_refuses_without_the_resolver() {
    let repository = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(repository.path().join("src")).unwrap();
    std::fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        repository.path().join("src/lib.rs"),
        "pub fn answer() -> u32 {\n    42\n}\n",
    )
    .unwrap();
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

    // A PATH without the toolchain, so no rust-analyzer can be found
    // whether or not this machine has one installed.
    let output = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(["check", "symbols", "--path"])
        .arg(repository.path())
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run stele");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("stele check: "), "{stderr}");
    assert!(
        stderr.contains("rust-analyzer is not available"),
        "{stderr}"
    );
}

#[test]
fn it_needs_exactly_one_source_of_facts() {
    assert_eq!(stele(&["check", "symbols"]).status.code(), Some(2));
    let snapshot = snapshot();
    let both = stele(&[
        "check",
        "symbols",
        "--path",
        ".",
        "--snapshot",
        snapshot.to_str().unwrap(),
    ]);
    assert_eq!(both.status.code(), Some(2));
}
