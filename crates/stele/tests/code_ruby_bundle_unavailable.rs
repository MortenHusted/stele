//! R2 fail-closed gate: when `--calls=required` is requested for Ruby but
//! no conformant ruby-lsp tool bundle is available, extraction must fail
//! clearly and never silently fall back to name-only call edges.
//!
//! The tool bundle is operator-supplied via `CODEMAP_RUBY_TOOL_BUNDLE`;
//! pointing it at a nonexistent or drifted lockfile must abort extraction.
//! Ignored by default — run with `cargo test -- --ignored`.

use std::process::Command;

#[path = "../../codemap/tests/common/mod.rs"]
mod common;

#[test]
#[ignore = "spawns stele against a bogus tool bundle"]
fn ruby_calls_required_fails_when_the_tool_bundle_is_absent() {
    let repo = common::make_repo_with(&ruby_files()).expect("create ruby repo");

    let output = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args([
            "code",
            "extract",
            "--path",
            repo.to_str().unwrap(),
            "--language=ruby",
            "--calls=required",
        ])
        .env("CODEMAP_RUBY_TOOL_BUNDLE", "/nonexistent/Gemfile")
        .output()
        .expect("spawn stele");

    assert!(
        !output.status.success(),
        "extract --calls=required must fail when the tool bundle is absent"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ruby-lsp") && stderr.contains("CODEMAP_RUBY_TOOL_BUNDLE"),
        "failure must state the bundle requirement: {stderr}"
    );
}

#[test]
#[ignore = "spawns stele against a drifted tool bundle"]
fn ruby_calls_required_fails_when_the_lockfile_drifts() {
    let repo = common::make_repo_with(&ruby_files()).expect("create ruby repo");

    // A Gemfile whose lockfile is not byte-identical to the canonical pin.
    let tmp = tempfile::tempdir().expect("tempdir");
    let gemfile = tmp.path().join("Gemfile");
    let lockfile = tmp.path().join("Gemfile.lock");
    std::fs::write(&gemfile, "source \"https://rubygems.org\"\n").expect("write Gemfile");
    std::fs::write(&lockfile, "DRIFTED\n").expect("write lockfile");

    let output = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args([
            "code",
            "extract",
            "--path",
            repo.to_str().unwrap(),
            "--language=ruby",
            "--calls=required",
        ])
        .env("CODEMAP_RUBY_TOOL_BUNDLE", &gemfile)
        .output()
        .expect("spawn stele");

    assert!(
        !output.status.success(),
        "extract --calls=required must fail when the bundle lockfile drifted"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ruby-lsp") && stderr.contains("CODEMAP_RUBY_TOOL_BUNDLE"),
        "failure must state the bundle requirement: {stderr}"
    );
}

fn ruby_files() -> Vec<(String, String)> {
    vec![(
        "app.rb".to_string(),
        "class A\n  def b\n  end\nend\n".to_string(),
    )]
}
