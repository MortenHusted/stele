//! R2 fail-closed gate: when `--calls=required` is requested for Ruby but
//! no conformant ruby-lsp tool bundle is available, extraction must fail
//! clearly and never silently fall back to name-only call edges.
//!
//! The tool bundle is operator-supplied via `CODEMAP_RUBY_TOOL_BUNDLE`;
//! pointing it at a nonexistent or drifted lockfile must abort extraction.
//! The original refusal gates are ignored by default — run with
//! `cargo test -- --include-ignored` to include them with the hermetic tests.

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
    assert!(stderr.contains("cannot read configured"), "{stderr}");
    assert!(!stderr.contains("Set CODEMAP_RUBY_TOOL_BUNDLE"), "{stderr}");
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
    assert!(stderr.contains("does not match"), "{stderr}");
    assert!(!stderr.contains("Set CODEMAP_RUBY_TOOL_BUNDLE"), "{stderr}");
    assert_eq!(std::fs::read_to_string(lockfile).unwrap(), "DRIFTED\n");
}

#[test]
fn ruby_calls_required_explains_unset_tool_bundle() {
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
        .env_remove("CODEMAP_RUBY_TOOL_BUNDLE")
        .output()
        .expect("spawn stele");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Set CODEMAP_RUBY_TOOL_BUNDLE"), "{stderr}");
}

fn ruby_files() -> Vec<(String, String)> {
    vec![(
        "app.rb".to_string(),
        "class A\n  def b\n  end\nend\n".to_string(),
    )]
}

#[test]
#[cfg(unix)]
fn ruby_calls_required_preserves_configured_bundle_probe_failure() {
    use std::os::unix::fs::PermissionsExt;

    let repo = common::make_repo_with(&ruby_files()).expect("create ruby repo");
    let tmp = tempfile::tempdir().expect("tempdir");
    let gemfile = tmp.path().join("Gemfile");
    let lockfile = tmp.path().join("Gemfile.lock");
    let canonical = include_str!("../../../toolchain/ruby/Gemfile.lock");
    std::fs::write(&gemfile, "source \"https://rubygems.org\"\n").unwrap();
    std::fs::write(&lockfile, canonical).unwrap();
    let bundle = tmp.path().join("bundle");
    std::fs::write(
        &bundle,
        "#!/bin/sh\n[ \"$BUNDLE_FROZEN\" = true ] || exit 91\n[ \"$*\" = 'exec ruby-lsp --version' ] || exit 92\nprintf '%s\\n' 'synthetic native extension incompatible with selected Ruby' >&2\nexit 42\n",
    )
    .unwrap();
    std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(tmp.path().to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
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
        .env("PATH", path)
        .output()
        .expect("spawn stele");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("synthetic native extension incompatible with selected Ruby"),
        "configured bundle must retain its probe failure: {stderr}"
    );
    assert!(!stderr.contains("Set CODEMAP_RUBY_TOOL_BUNDLE"), "{stderr}");
    assert_eq!(std::fs::read_to_string(lockfile).unwrap(), canonical);
}
