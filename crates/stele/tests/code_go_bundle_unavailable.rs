//! G2 fail-closed gate: when `--calls=required` is requested for Go but
//! no conformant gopls tool bundle is available, extraction must fail
//! clearly and never silently fall back to name-only call edges.
//!
//! The tool bundle is operator-supplied via `CODEMAP_GO_TOOL_BUNDLE`;
//! pointing it at a nonexistent or drifted directory must abort
//! extraction. Ignored by default — run with `cargo test -- --ignored`.

use std::process::Command;

#[path = "../../codemap/tests/common/mod.rs"]
mod common;

#[test]
#[ignore = "spawns stele against a bogus tool bundle"]
fn go_calls_required_fails_when_the_tool_bundle_is_absent() {
    let repo = common::make_repo_with(&go_files()).expect("create go repo");
    let output = extract_semantic(&repo, Some("/nonexistent/gopls-bundle"));
    assert_refused(&output, "extraction must fail without a tool bundle");
}

#[test]
#[ignore = "spawns stele against a drifted tool bundle"]
fn go_calls_required_fails_when_the_lockfile_drifts() {
    let repo = common::make_repo_with(&go_files()).expect("create go repo");

    // A bundle directory whose go.sum is not byte-identical to the
    // canonical pin, with a binary that would answer if asked.
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("go.sum"), "DRIFTED\n").expect("write lockfile");
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin");
    std::fs::write(bin_dir.join("gopls"), "#!/bin/sh\necho gopls v0\n").expect("write stub");

    let output = extract_semantic(&repo, tmp.path().to_str());
    assert_refused(&output, "extraction must fail against a drifted lockfile");
}

#[test]
#[ignore = "spawns stele without the env var set"]
fn go_calls_required_fails_without_env_var() {
    let repo = common::make_repo_with(&go_files()).expect("create go repo");
    let output = extract_semantic(&repo, None);
    assert_refused(
        &output,
        "extraction must fail when CODEMAP_GO_TOOL_BUNDLE is unset",
    );
}

fn extract_semantic(repo: &std::path::Path, bundle: Option<&str>) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_stele"));
    command.args([
        "code",
        "extract",
        "--path",
        repo.to_str().expect("utf-8 path"),
        "--language",
        "go",
        "--calls",
        "required",
        "--out",
        repo.join("snap.json").to_str().expect("utf-8 path"),
    ]);
    match bundle {
        Some(bundle) => command.env("CODEMAP_GO_TOOL_BUNDLE", bundle),
        None => command.env_remove("CODEMAP_GO_TOOL_BUNDLE"),
    };
    command.output().expect("spawn stele")
}

fn assert_refused(output: &std::process::Output, what: &str) {
    assert!(
        !output.status.success(),
        "{what}: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("gopls") && stderr.contains("CODEMAP_GO_TOOL_BUNDLE"),
        "stderr: {stderr}"
    );
}

fn go_files() -> Vec<(String, String)> {
    vec![
        (
            "go.mod".to_string(),
            "module example.invalid/app\n\ngo 1.25\n".to_string(),
        ),
        (
            "app.go".to_string(),
            "package app\n\ntype Service struct{}\n\nfunc (s *Service) Run() { s.work() }\n\nfunc (s *Service) work() {}\n".to_string(),
        ),
    ]
}
