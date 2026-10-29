//! Source-backed commands must select the same language at every extraction.

use std::path::Path;
use std::process::Command;

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(
        repo.path(),
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(repo.path(), &["config", "user.name", "Synthetic fixture"]);
    std::fs::write(
        repo.path().join("sample.rb"),
        "class Sample\n  def original\n    1\n  end\nend\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("sample.rs"), "pub fn rust_original() {}\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "base"]);
    std::fs::write(
        repo.path().join("sample.rb"),
        "class Sample\n  def original\n    2\n  end\n  def added\n    3\n  end\nend\n",
    )
    .unwrap();
    std::fs::write(
        repo.path().join("sample.rs"),
        "pub fn rust_original() {}\npub fn rust_added() {}\n",
    )
    .unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "head"]);
    repo
}

fn code(path: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(["code"])
        .args(args)
        .arg("--path")
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn ruby_diff_reports_added_and_changed_declarations() {
    let repo = repository();
    let output = code(
        repo.path(),
        &["diff", "HEAD~1", "HEAD", "--language", "ruby"],
    );
    assert!(output.contains("added nodes:"), "{output}");
    assert!(output.contains("+ method added (ruby:"), "{output}");
    assert!(output.contains("~ original [sample.rb]"), "{output}");
    assert!(!output.contains("rust_added"), "{output}");
}

#[test]
fn ruby_path_navigation_selects_ruby() {
    let repo = repository();
    for args in [
        vec!["context", "Sample"],
        vec!["search", "Sample"],
        vec!["structure"],
    ] {
        let mut args = args;
        args.extend(["--language", "ruby"]);
        let output = code(repo.path(), &args);
        assert!(output.contains("Sample"), "{output}");
        assert!(!output.contains("rust_original"), "{output}");
    }
    for args in [
        vec!["impact", "Sample", "--kinds", "contains"],
        vec!["trace", "Sample", "added", "--kinds", "contains"],
        vec!["cycles"],
        vec!["refactor", "--diff-base", "HEAD~1", "--diff-head", "HEAD"],
        vec!["clusters"],
        vec!["processes"],
    ] {
        let mut args = args;
        args.extend(["--language", "ruby"]);
        code(repo.path(), &args);
    }
}

fn assert_ruby_churn(output: &str) {
    assert!(
        output
            .lines()
            .any(|line| line.contains("ruby:") && line.contains("churn=1")),
        "{output}"
    );
}

#[test]
fn ruby_hotspots_weights_ruby_changes() {
    let repo = repository();
    let output = code(
        repo.path(),
        &[
            "hotspots",
            "--language",
            "ruby",
            "--diff-base",
            "HEAD~1",
            "--diff-head",
            "HEAD",
        ],
    );
    assert_ruby_churn(&output);
}

#[test]
fn snapshot_language_controls_navigation_and_churn_without_a_language_flag() {
    let repo = repository();
    let out = tempfile::tempdir().unwrap();
    let snapshot = out.path().join("ruby.json");
    code(
        repo.path(),
        &[
            "extract",
            "--language",
            "ruby",
            "--out",
            snapshot.to_str().unwrap(),
        ],
    );
    let output = code(
        repo.path(),
        &["context", "Sample", "--file", snapshot.to_str().unwrap()],
    );
    assert!(output.contains("Sample"), "{output}");
    let output = code(
        repo.path(),
        &[
            "hotspots",
            "--file",
            snapshot.to_str().unwrap(),
            "--diff-base",
            "HEAD~1",
            "--diff-head",
            "HEAD",
        ],
    );
    assert_ruby_churn(&output);
}

#[test]
fn source_commands_still_default_to_rust() {
    let repo = repository();
    let output = code(repo.path(), &["search", "rust_original"]);
    assert!(output.contains("rust_original"), "{output}");
    let output = code(repo.path(), &["diff", "HEAD~1", "HEAD"]);
    assert!(output.contains("rust_added"), "{output}");
    assert!(!output.contains("sample.rb"), "{output}");
}
