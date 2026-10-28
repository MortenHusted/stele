//! P4 producer gate: `stele code history --measures` keeps one measured
//! snapshot per walked commit and lists it with the author label, leaves
//! the three existing streams byte-identical, and supports each counting policy.

use std::path::{Path, PathBuf};
use std::process::Command;

use codemap::schema::{Language, Snapshot, measures_policy};
use codemap::{HistoryOptions, Selector, validate, walk_history};

mod common;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", "2000-01-02T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-02T00:00:00Z")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The measures fixture repo with a second commit by a second author that
/// adds one branchy function, so the walk has two commits and two labels.
fn two_commit_repo(language: Language) -> PathBuf {
    let repo = common::make_measures_repo_for(language).expect("fixture repo");
    let (rel, body) = match language {
        Language::Rust => (
            "later.rs",
            "pub fn later(x: i32) -> i32 {\n    if x > 1 { x } else { 0 }\n}\n",
        ),
        Language::Ruby => ("later.rb", "def later(x)\n  x > 1 ? x : 0\nend\n"),
        Language::Php => (
            "later.php",
            "<?php\nfunction later(int $x): int {\n    return $x > 1 ? $x : 0;\n}\n",
        ),
        Language::Go => (
            "later.go",
            "package fixture\n\nfunc later(x int) int {\n\tif x > 1 {\n\t\treturn x\n\t}\n\treturn 0\n}\n",
        ),
        Language::Cpp | Language::JsTs | Language::Python => panic!("cpp has no measures history"),
    };
    std::fs::write(repo.join(rel), body).expect("write second file");
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Second Author",
            "-c",
            "user.email=second@example.invalid",
            "commit",
            "-m",
            "later",
            "--quiet",
        ],
    );
    repo
}

fn walk(repo: &Path, language: Language, measures: bool) -> PathBuf {
    let out = tempfile::tempdir().expect("out dir").keep();
    let options = HistoryOptions {
        #[cfg(unix)]
        python_parser: None,
        repo: repo.to_path_buf(),
        tip: "HEAD".to_string(),
        selector: Selector::All,
        limit: None,
        out_dir: out.clone(),
        language,
        semantic: false,
        measures,
        twins: false,
    };
    let mut progress = Vec::new();
    let summary = walk_history(&options, &mut progress).expect("walk");
    assert_eq!(summary.commits, 2);
    out
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

/// `commits.ndjson` carries `extract_ms`, the wall clock of each
/// extraction, which no two runs share; every other byte is compared.
fn without_wall_clock(commits: &str) -> String {
    commits
        .lines()
        .map(|line| {
            let mut value: serde_json::Value = serde_json::from_str(line).expect("commit line");
            value.as_object_mut().expect("object").remove("extract_ms");
            value.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_three_streams_are_byte_identical_with_the_switch_on_or_off() {
    let repo = two_commit_repo(Language::Rust);
    let plain = walk(&repo, Language::Rust, false);
    let measured = walk(&repo, Language::Rust, true);
    for name in ["transitions.ndjson", "metrics.ndjson"] {
        assert_eq!(
            read(&plain, name),
            read(&measured, name),
            "{name} must not change under --measures"
        );
    }
    assert_eq!(
        without_wall_clock(&read(&plain, "commits.ndjson")),
        without_wall_clock(&read(&measured, "commits.ndjson")),
        "commits.ndjson must not change under --measures (extract_ms is wall clock)"
    );
    assert!(
        !plain.join("measured.ndjson").exists() && !plain.join("measures").exists(),
        "a plain walk writes nothing new"
    );
}

#[test]
fn a_measured_walk_keeps_one_measured_snapshot_per_commit_with_its_author() {
    let repo = two_commit_repo(Language::Rust);
    let out = walk(&repo, Language::Rust, true);
    let listing = read(&out, "measured.ndjson");
    let lines: Vec<serde_json::Value> = listing
        .lines()
        .map(|l| serde_json::from_str(l).expect("listing line"))
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["seq"], 0);
    assert_eq!(lines[1]["seq"], 1);
    assert_eq!(lines[0]["author"], "Test User");
    assert_eq!(lines[1]["author"], "Second Author");
    assert_eq!(lines[1]["author_email"], "second@example.invalid");
    let policy = measures_policy(Language::Rust).expect("rust policy");
    for line in &lines {
        assert_eq!(line["measures_policy"], policy);
        let rel = line["snapshot"].as_str().expect("snapshot path");
        assert!(rel.starts_with("measures/"), "{rel}");
        let text = read(&out, rel);
        let snapshot: Snapshot = serde_json::from_str(&text).expect("snapshot parses");
        validate(&snapshot).expect("measured snapshot validates");
        assert_eq!(snapshot.identity.measures_policy.as_deref(), Some(policy));
        assert_eq!(snapshot.identity.snapshot_id, line["snapshot_id"]);
        assert_eq!(snapshot.identity.commit_sha, line["commit"]);
        assert!(
            !snapshot.measures.is_empty(),
            "the snapshot carries measures"
        );
    }
    // The second commit added one callable; the file count grew by one.
    let first: Snapshot =
        serde_json::from_str(&read(&out, lines[0]["snapshot"].as_str().unwrap())).unwrap();
    let second: Snapshot =
        serde_json::from_str(&read(&out, lines[1]["snapshot"].as_str().unwrap())).unwrap();
    assert_eq!(second.file_measures.len(), first.file_measures.len() + 1);
    assert_eq!(second.measures.len(), first.measures.len() + 1);
}

#[test]
fn a_measured_walk_is_deterministic() {
    let repo = two_commit_repo(Language::Rust);
    let a = walk(&repo, Language::Rust, true);
    let b = walk(&repo, Language::Rust, true);
    assert_eq!(read(&a, "measured.ndjson"), read(&b, "measured.ndjson"));
    for line in read(&a, "measured.ndjson").lines() {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        let rel = entry["snapshot"].as_str().unwrap();
        assert_eq!(
            read(&a, rel),
            read(&b, rel),
            "{rel} bytes differ between runs"
        );
    }
}

#[test]
fn ruby_history_keeps_a_measured_snapshot_for_each_commit() {
    let repo = two_commit_repo(Language::Ruby);
    let out = walk(&repo, Language::Ruby, true);
    for line in read(&out, "measured.ndjson").lines() {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(entry["measures_policy"], "cc-sloc-clones-form/ruby/1");
        let snapshot: Snapshot =
            serde_json::from_str(&read(&out, entry["snapshot"].as_str().unwrap())).unwrap();
        validate(&snapshot).unwrap();
        assert_ne!(snapshot.measures, [] as [codemap::schema::Measure; 0]);
        assert_eq!(
            snapshot.clone_groups,
            [] as [codemap::schema::CloneGroup; 0]
        );
    }
}
