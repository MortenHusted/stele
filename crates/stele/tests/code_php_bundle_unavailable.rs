//! P2 fail-closed gate: when `--calls=required` is requested for PHP but
//! no conformant phpactor tool bundle is available, extraction must fail
//! clearly and never silently fall back to name-only call edges.
//!
//! The tool bundle is operator-supplied via `CODEMAP_PHP_TOOL_BUNDLE`;
//! pointing it at a nonexistent or drifted directory must abort
//! extraction. Ignored by default — run with `cargo test -- --ignored`.

use std::process::Command;

#[path = "../../codemap/tests/common/mod.rs"]
mod common;

#[test]
#[ignore = "spawns stele against a bogus tool bundle"]
fn php_calls_required_fails_when_the_tool_bundle_is_absent() {
    let repo = common::make_repo_with(&php_files()).expect("create php repo");

    let output = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args([
            "code",
            "extract",
            "--path",
            repo.to_str().expect("utf-8 path"),
            "--language",
            "php",
            "--calls",
            "required",
            "--out",
            repo.join("snap.json").to_str().expect("utf-8 path"),
        ])
        .env("CODEMAP_PHP_TOOL_BUNDLE", "/nonexistent/phpactor-bundle")
        .output()
        .expect("spawn stele");

    assert!(
        !output.status.success(),
        "extraction must fail without a tool bundle: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("phpactor") && stderr.contains("CODEMAP_PHP_TOOL_BUNDLE"),
        "stderr: {stderr}"
    );
}

#[test]
#[ignore = "spawns stele against a drifted tool bundle"]
fn php_calls_required_fails_when_the_lockfile_drifts() {
    let repo = common::make_repo_with(&php_files()).expect("create php repo");

    // A bundle directory whose composer.lock is not byte-identical to the
    // canonical pin.
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("composer.lock"), "DRIFTED\n").expect("write lockfile");
    let bin_dir = tmp.path().join("vendor/bin");
    std::fs::create_dir_all(&bin_dir).expect("vendor/bin");
    std::fs::write(bin_dir.join("phpactor"), "#!/bin/sh\necho 0\n").expect("write stub");

    let output = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args([
            "code",
            "extract",
            "--path",
            repo.to_str().expect("utf-8 path"),
            "--language",
            "php",
            "--calls",
            "required",
            "--out",
            repo.join("snap.json").to_str().expect("utf-8 path"),
        ])
        .env("CODEMAP_PHP_TOOL_BUNDLE", tmp.path())
        .output()
        .expect("spawn stele");

    assert!(
        !output.status.success(),
        "extraction must fail against a drifted lockfile"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("phpactor") && stderr.contains("CODEMAP_PHP_TOOL_BUNDLE"),
        "stderr: {stderr}"
    );
}

#[test]
#[ignore = "spawns stele without the env var set"]
fn php_calls_required_fails_without_env_var() {
    let repo = common::make_repo_with(&php_files()).expect("create php repo");

    let output = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args([
            "code",
            "extract",
            "--path",
            repo.to_str().expect("utf-8 path"),
            "--language",
            "php",
            "--calls",
            "required",
            "--out",
            repo.join("snap.json").to_str().expect("utf-8 path"),
        ])
        .env_remove("CODEMAP_PHP_TOOL_BUNDLE")
        .output()
        .expect("spawn stele");

    assert!(
        !output.status.success(),
        "extraction must fail when CODEMAP_PHP_TOOL_BUNDLE is unset"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("phpactor") && stderr.contains("CODEMAP_PHP_TOOL_BUNDLE"),
        "stderr: {stderr}"
    );
}

fn php_files() -> Vec<(String, String)> {
    vec![(
        "app.php".to_string(),
        "<?php\nnamespace App;\nfinal class Service {\n    public function run(): void {\n        $this->work();\n    }\n    private function work(): void {}\n}\n".to_string(),
    )]
}
