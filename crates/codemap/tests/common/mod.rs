#![allow(dead_code)]

//! Shared integration-test helpers for `codemap`.
//!
//! Provides a function that creates a temporary git repository, writes the
//! requested fixture files into it, commits them, and returns the repo path.
//! All integration tests that call [`codemap::extract`] must use a clean
//! git worktree, so this helper is the single place that sets one up.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

/// Create a temporary git repository containing `files`.
///
/// `files` is a list of `(relative_path, contents)`. Parent directories are
/// created as needed. The repository is initialized, a dummy identity is
/// configured, files are added, and a single commit is made. The returned
/// path is the repository root.
pub fn make_repo_with(files: &[(String, String)]) -> Result<PathBuf> {
    let tmp = tempfile::tempdir().context("create tempdir")?;
    let root = tmp.keep();

    initialize_repo(&root, files)?;
    Ok(root)
}

fn initialize_repo(root: &std::path::Path, files: &[(String, String)]) -> Result<()> {
    std::fs::create_dir_all(root).context("create repository root")?;

    git(root, &["init", "--quiet"]).context("git init")?;
    git(root, &["config", "user.email", "test@example.com"]).context("git config user.email")?;
    git(root, &["config", "user.name", "Test User"]).context("git config user.name")?;

    for (rel_path, content) in files {
        let full = root.join(rel_path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create directory for {rel_path}"))?;
        }
        std::fs::write(&full, content).with_context(|| format!("write fixture file {rel_path}"))?;
    }

    git(root, &["add", "-A"]).context("git add")?;
    git(root, &["commit", "-m", "test", "--quiet"]).context("git commit")?;
    Ok(())
}

/// Run a git command in `dir`, returning trimmed stdout.
fn git(dir: &std::path::Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn git {args:?}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "git {:?} failed ({}): {}",
            args,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `crates/codemap/tests/fixtures`, reached through the workspace so the
/// `stele` tests that include this module find it too.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../codemap/tests/fixtures")
}

/// Load all files under `tests/fixtures/corpus/` as `(rel_path, content)`.
///
/// The corpus is intentionally flat: every `.rs` file sits next to the
/// mixed-language `helper.rb`/`helper.ts` files so purity tests can assert
/// that only Rust files produce facts.
pub fn corpus_files() -> Vec<(String, String)> {
    let dir = fixtures_dir().join("corpus");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read corpus dir") {
        let entry = entry.expect("corpus dir entry");
        let path = entry.path();
        if path.is_file() {
            let rel = path
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .to_string();
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read corpus file {rel}: {e}"));
            files.push((rel, content));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

/// Load the tracked portion of the Ruby corpus fixture.
pub fn ruby_corpus_files() -> Vec<(String, String)> {
    let base = fixtures_dir().join("ruby_corpus");
    load_files_recursive(&base, &[])
}

/// Load the tracked portion of the PHP corpus fixture.
pub fn php_corpus_files() -> Vec<(String, String)> {
    let base = fixtures_dir().join("php_corpus");
    load_files_recursive(&base, &[])
}

/// Load the tracked portion of the Go corpus fixture.
pub fn go_corpus_files() -> Vec<(String, String)> {
    let base = fixtures_dir().join("go_corpus");
    load_files_recursive(&base, &[])
}

/// Load the tracked portion of the Go call oracle fixture.
pub fn go_calls_files() -> Vec<(String, String)> {
    let base = fixtures_dir().join("go_calls");
    load_files_recursive(&base, &["expected.json"])
}

/// Create the Go semantic call-impact fixture with a stable commit and
/// origin so snapshot identity is reproducible across independent
/// checkouts.
pub fn make_go_calls_repo() -> Result<PathBuf> {
    make_stable_repo(
        &go_calls_files(),
        "https://example.invalid/fixtures/gocalls.git",
    )
}

/// Create the Go corpus fixture repo with a stable commit and origin so
/// repository identity (and therefore snapshot bytes) is reproducible
/// across independent checkouts.
pub fn make_go_corpus_repo() -> Result<PathBuf> {
    make_stable_repo(
        &go_corpus_files(),
        "https://example.invalid/fixtures/gofix.git",
    )
}

/// A fixture repository whose commit and origin are fixed, so two
/// independent checkouts of the same files carry the same identity.
fn make_stable_repo(files: &[(String, String)], origin: &str) -> Result<PathBuf> {
    let tmp = tempfile::tempdir().context("create fixture tempdir")?;
    let fixture_root = tmp.keep();
    let root = fixture_root.join("project");
    initialize_repo(&root, files)?;
    git(&root, &["remote", "add", "origin", origin]).context("add stable fixture origin")?;

    let status = Command::new("git")
        .current_dir(&root)
        .args([
            "commit",
            "--amend",
            "--reset-author",
            "--no-edit",
            "--quiet",
        ])
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .status()
        .context("stabilize fixture commit")?;
    anyhow::ensure!(status.success(), "stabilize fixture commit failed");

    Ok(root)
}

/// Create the PHP corpus fixture repo with a stable commit and origin so
/// repository identity (and therefore snapshot bytes) is reproducible
/// across independent checkouts.
pub fn make_php_corpus_repo() -> Result<PathBuf> {
    let tmp = tempfile::tempdir().context("create php corpus tempdir")?;
    let fixture_root = tmp.keep();
    let root = fixture_root.join("project");
    initialize_repo(&root, &php_corpus_files())?;
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/fixtures/phpfix.git",
        ],
    )
    .context("add stable fixture origin")?;

    let status = Command::new("git")
        .current_dir(&root)
        .args([
            "commit",
            "--amend",
            "--reset-author",
            "--no-edit",
            "--quiet",
        ])
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .status()
        .context("stabilize fixture commit")?;
    anyhow::ensure!(status.success(), "stabilize fixture commit failed");

    Ok(root)
}

/// Create the Ruby corpus fixture repo with a stable commit and origin so
/// repository identity (and therefore snapshot bytes) is reproducible
/// across independent checkouts.
pub fn make_ruby_corpus_repo() -> Result<PathBuf> {
    let tmp = tempfile::tempdir().context("create ruby corpus tempdir")?;
    let fixture_root = tmp.keep();
    let root = fixture_root.join("project");
    initialize_repo(&root, &ruby_corpus_files())?;
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/fixtures/rubyfix.git",
        ],
    )
    .context("add stable fixture origin")?;

    let status = Command::new("git")
        .current_dir(&root)
        .args([
            "commit",
            "--amend",
            "--reset-author",
            "--no-edit",
            "--quiet",
        ])
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .status()
        .context("stabilize fixture commit")?;
    anyhow::ensure!(status.success(), "stabilize fixture commit failed");

    Ok(root)
}

/// Create the semantic call-impact fixture with a stable commit and origin.
pub fn make_calls_cargo_repo() -> Result<PathBuf> {
    let tmp = tempfile::tempdir().context("create calls fixture tempdir")?;
    let fixture_root = tmp.keep();
    let root = fixture_root.join("project");
    initialize_repo(&root, &calls_cargo_files())?;
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/fixtures/callfix.git",
        ],
    )
    .context("add stable fixture origin")?;

    let status = Command::new("git")
        .current_dir(&root)
        .args([
            "commit",
            "--amend",
            "--reset-author",
            "--no-edit",
            "--quiet",
        ])
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .status()
        .context("stabilize fixture commit")?;
    anyhow::ensure!(status.success(), "stabilize fixture commit failed");

    Ok(root)
}

/// Create the PHP semantic call-impact fixture with a stable commit and
/// origin so snapshot identity is reproducible across independent
/// checkouts.
pub fn make_php_calls_repo() -> Result<PathBuf> {
    let tmp = tempfile::tempdir().context("create php calls tempdir")?;
    let fixture_root = tmp.keep();
    let root = fixture_root.join("project");
    initialize_repo(&root, &php_calls_files())?;
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/fixtures/phpcalls.git",
        ],
    )
    .context("add stable fixture origin")?;

    let status = Command::new("git")
        .current_dir(&root)
        .args([
            "commit",
            "--amend",
            "--reset-author",
            "--no-edit",
            "--quiet",
        ])
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .status()
        .context("stabilize fixture commit")?;
    anyhow::ensure!(status.success(), "stabilize fixture commit failed");

    Ok(root)
}

/// Load the tracked portion of the PHP call oracle fixture.
pub fn php_calls_files() -> Vec<(String, String)> {
    load_files_recursive(&php_calls_fixture_dir(), &["expected.json"])
}

fn php_calls_fixture_dir() -> PathBuf {
    fixtures_dir().join("php_calls")
}

/// The fixture directory of one language under `tests/fixtures/<kind>/`.
fn language_fixture_dir(kind: &str, language: codemap::schema::Language) -> PathBuf {
    fixtures_dir().join(kind).join(language.tag())
}

/// Load one language's measures oracle fixture (hand-counted sidecar
/// excluded).
pub fn measures_files_for(language: codemap::schema::Language) -> Vec<(String, String)> {
    load_files_recursive(
        &language_fixture_dir("measures", language),
        &["expected.json"],
    )
}

/// Load the Rust measures oracle fixture.
pub fn measures_files() -> Vec<(String, String)> {
    measures_files_for(codemap::schema::Language::Rust)
}

/// Load one language's clone oracle fixture (hand-derived sidecar
/// excluded).
pub fn clones_files_for(language: codemap::schema::Language) -> Vec<(String, String)> {
    load_files_recursive(
        &language_fixture_dir("clones", language),
        &["expected.json"],
    )
}

/// Load the Rust clone oracle fixture.
pub fn clones_files() -> Vec<(String, String)> {
    clones_files_for(codemap::schema::Language::Rust)
}

/// One language's measures fixture as a stable repo.
pub fn make_measures_repo_for(language: codemap::schema::Language) -> Result<PathBuf> {
    make_stable_repo(
        &measures_files_for(language),
        &format!(
            "https://example.invalid/fixtures/{}-measures.git",
            language.tag()
        ),
    )
}

/// The Rust measures fixture as a stable repo.
pub fn make_measures_repo() -> Result<PathBuf> {
    make_stable_repo(
        &measures_files(),
        "https://example.invalid/fixtures/measures.git",
    )
}

/// One language's clone fixture as a stable repo.
pub fn make_clones_repo_for(language: codemap::schema::Language) -> Result<PathBuf> {
    make_stable_repo(
        &clones_files_for(language),
        &format!(
            "https://example.invalid/fixtures/{}-clones.git",
            language.tag()
        ),
    )
}

/// One language's twin oracle fixture as a stable repo.
pub fn make_twins_repo_for(language: codemap::schema::Language) -> Result<PathBuf> {
    make_stable_repo(
        &load_files_recursive(&language_fixture_dir("twins", language), &["expected.json"]),
        &format!(
            "https://example.invalid/fixtures/{}-twins.git",
            language.tag()
        ),
    )
}

/// The Rust clone fixture as a stable repo.
pub fn make_clones_repo() -> Result<PathBuf> {
    make_stable_repo(
        &clones_files(),
        "https://example.invalid/fixtures/clones.git",
    )
}

/// Create the Ruby semantic call-impact fixture with a stable commit and
/// origin so snapshot identity is reproducible across independent
/// checkouts.
pub fn make_ruby_calls_repo() -> Result<PathBuf> {
    let tmp = tempfile::tempdir().context("create ruby calls tempdir")?;
    let fixture_root = tmp.keep();
    let root = fixture_root.join("project");
    initialize_repo(&root, &ruby_calls_files())?;
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/fixtures/rubycalls.git",
        ],
    )
    .context("add stable fixture origin")?;

    let status = Command::new("git")
        .current_dir(&root)
        .args([
            "commit",
            "--amend",
            "--reset-author",
            "--no-edit",
            "--quiet",
        ])
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .status()
        .context("stabilize fixture commit")?;
    anyhow::ensure!(status.success(), "stabilize fixture commit failed");

    Ok(root)
}

/// Load the tracked portion of the Ruby call oracle fixture.
pub fn ruby_calls_files() -> Vec<(String, String)> {
    load_files_recursive(&ruby_calls_fixture_dir(), &["expected.json"])
}

fn ruby_calls_fixture_dir() -> PathBuf {
    fixtures_dir().join("ruby_calls")
}

/// Load the tracked portion of the Cargo workspace fixture.
pub fn calls_cargo_files() -> Vec<(String, String)> {
    load_files_recursive(&calls_cargo_fixture_dir(), &["expected.json", "target"])
}

fn calls_cargo_fixture_dir() -> PathBuf {
    fixtures_dir().join("calls_cargo")
}

fn load_files_recursive(base: &std::path::Path, excluded: &[&str]) -> Vec<(String, String)> {
    fn visit(
        base: &std::path::Path,
        dir: &std::path::Path,
        excluded: &[&str],
        files: &mut Vec<(String, String)>,
    ) {
        for entry in std::fs::read_dir(dir).expect("read fixture directory") {
            let entry = entry.expect("fixture directory entry");
            let path = entry.path();
            let rel = path.strip_prefix(base).expect("fixture path under base");
            let first = rel.components().next().expect("non-empty fixture path");
            if excluded
                .iter()
                .any(|name| first.as_os_str() == std::ffi::OsStr::new(name))
            {
                continue;
            }
            if path.is_dir() {
                visit(base, &path, excluded, files);
            } else {
                let rel = rel.to_string_lossy().replace('\\', "/");
                let content = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {rel}: {error}"));
                files.push((rel, content));
            }
        }
    }

    let mut files = Vec::new();
    visit(base, base, excluded, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

/// Stable mixed JS/TS call fixture; sidecar labels are kept as inert data.
pub fn make_js_ts_calls_repo() -> Result<PathBuf> {
    let base = fixtures_dir().join("js_ts_calls");
    make_stable_repo(
        &load_files_recursive(&base, &[]),
        "https://example.invalid/fixtures/js-ts-calls.git",
    )
}
