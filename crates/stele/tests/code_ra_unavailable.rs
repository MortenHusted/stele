//! P2 fail-closed gate: when `--calls=required` is requested but
//! rust-analyzer is not available, extraction must fail clearly and never
//! silently fall back to name-only call edges.
//!
//! This test runs `stele code extract` with a `PATH` that does not
//! contain `rust-analyzer`, so the detection fails. It is ignored by
//! default — run with `cargo test -- --ignored`.

use std::process::Command;

#[path = "../../codemap/tests/common/mod.rs"]
mod common;

#[test]
#[ignore = "spawns stele with a restricted PATH"]
fn calls_required_fails_when_rust_analyzer_absent() {
    let files = common::calls_cargo_files();
    let repo = common::make_repo_with(&files).expect("create calls cargo repo");

    // Run with a PATH containing only the system basics (no cargo bin dir,
    // so rust-analyzer is not found). /usr/bin has `git` but not rust-analyzer.
    let output = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args([
            "code",
            "extract",
            "--path",
            repo.to_str().unwrap(),
            "--calls=required",
        ])
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("spawn stele");

    assert!(
        !output.status.success(),
        "extract --calls=required must fail when rust-analyzer is absent"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rust-analyzer") && stderr.contains("not available"),
        "failure must clearly state rust-analyzer is not available: {stderr}"
    );
}
