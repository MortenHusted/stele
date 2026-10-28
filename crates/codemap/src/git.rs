//! Git provenance: clean-worktree enforcement, repository/commit/tree
//! identity, and tracked source-file inventory.
//!
//! Owns Git commands only. Never derives facts. Default extraction refuses a
//! dirty worktree so commit provenance is always honest — see the AGENTS.md
//! truth boundary and plan P0/P1 gates. Worktree extraction is the one
//! deliberate exception: it reads the working tree as it is, names the tree
//! it read by its own content hash, and marks the snapshot `dirty` whenever
//! that tree differs from `HEAD`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail, ensure};

use crate::schema::sha256_hex;
use crate::schema::{Language, RepoIdentity};

/// Git identity + file inventory for one extraction source: a clean commit,
/// an arbitrary commit-ish, or the working tree as it is.
#[derive(Debug, Clone)]
pub struct GitState {
    pub repo_root: String,
    pub identity: RepoIdentity,
    pub commit_sha: String,
    /// Tree identity of what was read: `HEAD^{tree}` for a commit, or the
    /// content hash of the working tree for worktree extraction.
    pub tree_sha: String,
    /// `true` when the inventory and bytes come from the working tree on
    /// disk rather than the commit object store.
    pub worktree: bool,
    /// `true` when the working tree differed from `HEAD` at capture time.
    /// Always `false` for commit-pinned state.
    pub dirty: bool,
    /// Snapshot source language; fixes which inventory glob was used.
    pub language: Language,
    /// Tracked source files of the snapshot language, repo-relative,
    /// forward-slash normalized, sorted.
    pub source_files: Vec<String>,
    /// SHA-256 of a tracked root `Cargo.lock`, when present at this commit.
    pub cargo_lock_sha256: Option<String>,
}

/// Git reads use local objects without configured monitors, hooks or fetches.
fn base_command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .args([
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "log.showSignature=false",
            "-C",
        ])
        .arg(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_ALLOW_PROTOCOL", "");
    command
}

fn data_command(dir: &Path) -> Result<Command> {
    let config = base_command(dir)
        .args([
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^filter\..*\.(clean|smudge|process|required)$",
        ])
        .output()
        .context("read Git filter configuration")?;
    ensure!(
        config.status.success() || config.status.code() == Some(1),
        "cannot read Git filter configuration"
    );
    let names = String::from_utf8(config.stdout).context("Git filter keys must be UTF-8")?;
    let mut command = base_command(dir);
    for key in names.split('\0').filter(|key| !key.is_empty()) {
        ensure!(
            !key.contains('='),
            "Git filter key cannot be disabled safely"
        );
        let value = if key.ends_with(".required") {
            "false"
        } else {
            ""
        };
        command.args(["-c", &format!("{key}={value}")]);
    }
    Ok(command)
}

/// Run a data-only Git command and return its trimmed stdout.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = data_command(dir)?
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "git {:?} failed ({}): {}",
            args,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run `git` in `dir` against a private index file instead of the user's
/// `.git/index`, so working-tree identity can be computed without staging
/// anything on the user's behalf.
fn git_with_index(dir: &Path, index: &Path, args: &[&str]) -> Result<String> {
    let out = data_command(dir)?
        .env("GIT_INDEX_FILE", index)
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn git {args:?} with private index"))?;
    if !out.status.success() {
        bail!(
            "git {:?} (private index) failed ({}): {}",
            args,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run a read-only git command in `dir` and return its trimmed stdout. The
/// history walk needs commit selection (`git log`) that no other caller wants;
/// this exposes the same spawn-and-check path rather than duplicating it.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    git(dir, args)
}

/// Materialize `commitish` in a throwaway detached worktree at `path`.
///
/// `--detach` leaves the user's checkout, `HEAD`, index, and branches alone: the
/// new tree points straight at the commit with no branch created. Semantic
/// extraction needs this because rust-analyzer reads a workspace from disk.
pub fn add_detached_worktree(repo_root: &Path, path: &Path, commitish: &str) -> Result<()> {
    let path_arg = path
        .to_str()
        .with_context(|| format!("worktree path is not valid UTF-8: {}", path.display()))?;
    git(
        repo_root,
        &["worktree", "add", "--detach", path_arg, commitish],
    )
    .with_context(|| {
        format!(
            "add detached worktree at {} for {commitish}",
            path.display()
        )
    })?;
    Ok(())
}

/// Remove a worktree created by [`add_detached_worktree`] and drop its
/// administrative entry, so a repository is not left accumulating stale
/// registrations after many walks.
pub fn remove_worktree(repo_root: &Path, path: &Path) -> Result<()> {
    let path_arg = path
        .to_str()
        .with_context(|| format!("worktree path is not valid UTF-8: {}", path.display()))?;
    git(repo_root, &["worktree", "remove", "--force", path_arg])
        .with_context(|| format!("remove worktree at {}", path.display()))?;
    // Extraction leaves build artifacts behind, so `remove` can succeed while
    // the registration lingers; pruning keeps `git worktree list` honest.
    git(repo_root, &["worktree", "prune"])?;
    Ok(())
}

/// Determine the repository root for `path` (which may be inside it).
pub fn repo_root(path: &Path) -> Result<String> {
    git(path, &["rev-parse", "--show-toplevel"])
}

/// Refuse extraction unless the worktree is clean. Returns the `git status
/// --porcelain` output (empty on clean) so callers can surface details.
pub fn require_clean(repo_root: &Path) -> Result<()> {
    let status = git(repo_root, &["status", "--porcelain"])?;
    if !status.is_empty() {
        bail!(
            "worktree is dirty, and extraction reads clean commits. Commit or stash the \
             changes, or extract the working tree with `stele code extract --dirty` and \
             check that snapshot with `stele check --snapshot <file> --allow-dirty`.\n\
             Uncommitted changes:\n{status}"
        );
    }
    Ok(())
}

/// Whether the working tree differs from `HEAD` in any tracked or untracked,
/// non-ignored file.
pub fn is_dirty(repo_root: &Path) -> Result<bool> {
    Ok(!git(repo_root, &["status", "--porcelain"])?.is_empty())
}

/// Content identity of the working tree: the tree object git would write for
/// `HEAD` plus every uncommitted change, computed in a private index so the
/// user's index is never touched. Respects `.gitignore`, keeps tracked-but-
/// ignored files, and equals `HEAD^{tree}` exactly when the tree is clean.
pub fn worktree_tree_sha(repo_root: &Path) -> Result<String> {
    let index_path = private_index_path();
    let result = (|| {
        git_with_index(repo_root, &index_path, &["read-tree", "HEAD"])
            .context("seed private index from HEAD")?;
        git_with_index(repo_root, &index_path, &["add", "-A", "--", "."])
            .context("stage working tree into private index")?;
        git_with_index(repo_root, &index_path, &["write-tree"]).context("write working-tree object")
    })();
    let _ = std::fs::remove_file(&index_path);
    result
}

/// Hands out a distinct number to every private index this process names.
static PRIVATE_INDEX_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A path no other private index shares: named for this process, the
/// instant, and a process-wide sequence number, so concurrent extractions
/// in one process (the wall clock ticks in microseconds on macOS, and two
/// threads easily land in the same one) never seed, stage into, or remove
/// each other's index. Git wants either a real index or no file at all, so
/// the path is only ever created by git itself.
fn private_index_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let sequence = PRIVATE_INDEX_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "codemap-index-{}-{nanos}-{sequence}",
        std::process::id()
    ))
}

/// Resolve repository identity. Prefers `origin` remote URL; falls back to
/// a stable local identifier derived from the absolute repo root so the
/// fallback is repeatable across runs in the same checkout without being
/// mistaken for a canonical remote.
pub fn repository_identity(repo_root: &Path) -> Result<RepoIdentity> {
    let origin = git(repo_root, &["config", "--get", "remote.origin.url"]).ok();
    origin.filter(|s| !s.is_empty()).map_or_else(
        || {
            let abs = repo_root
                .canonicalize()
                .unwrap_or_else(|_| repo_root.to_path_buf());
            let digest = sha256_hex(abs.to_string_lossy().as_bytes());
            Ok(RepoIdentity {
                id: format!("local:{}", &digest[..16]),
                origin: None,
                is_canonical: false,
            })
        },
        |url| {
            let sanitized_origin = sanitize_remote_url(&url);
            Ok(RepoIdentity {
                id: normalize_url(&url),
                origin: Some(sanitized_origin),
                is_canonical: true,
            })
        },
    )
}

/// Remove URL userinfo before repository metadata is serialized. SCP-style
/// SSH remotes such as `git@github.com:org/repo.git` carry a conventional
/// username rather than an embedded credential and are preserved verbatim.
fn sanitize_remote_url(url: &str) -> String {
    let trimmed = url.trim();
    let Some((scheme, authority_and_path)) = trimmed.split_once("://") else {
        return trimmed.to_string();
    };
    let authority_end = authority_and_path
        .find(['/', '?', '#'])
        .unwrap_or(authority_and_path.len());
    let (authority, suffix) = authority_and_path.split_at(authority_end);
    let sanitized_authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{scheme}://{sanitized_authority}{suffix}")
}

/// Normalize a git URL into a stable identifier: strip credentials and
/// trailing `.git`.
fn normalize_url(url: &str) -> String {
    let sanitized = sanitize_remote_url(url);
    let s = sanitized.trim_end_matches(".git");
    if !s.contains("://") {
        if let Some((_, rest)) = s.rsplit_once('@') {
            return rest.to_string();
        }
    }
    s.to_string()
}

/// Commit and tree SHAs for `HEAD`.
pub fn head_identity(repo_root: &Path) -> Result<(String, String)> {
    let commit = git(repo_root, &["rev-parse", "HEAD"])?;
    // `^{tree}` is git rev-parse syntax for the tree object at HEAD; the
    // braces are not Rust format args.
    #[allow(clippy::literal_string_with_formatting_args)]
    let tree_ref = "HEAD^{tree}";
    let tree = git(repo_root, &["rev-parse", tree_ref])?;
    Ok((commit, tree))
}

/// Commit and tree SHAs for an arbitrary commit-ish (SHA, branch, tag, `HEAD~3`).
pub fn commit_identity(repo_root: &Path, commitish: &str) -> Result<(String, String)> {
    let commit = git(repo_root, &["rev-parse", commitish])?;
    #[allow(clippy::literal_string_with_formatting_args)]
    let tree_ref = format!("{commitish}^{{tree}}");
    let tree = git(repo_root, &["rev-parse", &tree_ref])?;
    Ok((commit, tree))
}

/// Commit time in seconds since epoch for `commitish`.
#[allow(dead_code)]
pub fn commit_time_seconds(repo_root: &Path, commitish: &str) -> Result<i64> {
    let s = git(repo_root, &["show", "-s", "--format=%ct", commitish])?;
    Ok(s.parse().unwrap_or(0))
}

/// The author label a commit carries, verbatim: `(name, email)`. What the
/// label means is the operator's call; codemap records it and never
/// interprets it.
pub fn commit_author(repo_root: &Path, commitish: &str) -> Result<(String, String)> {
    let s = git(repo_root, &["show", "-s", "--format=%an%x1f%ae", commitish])?;
    let (name, email) = s.split_once('\u{1f}').unwrap_or((s.as_str(), ""));
    Ok((name.trim().to_string(), email.trim().to_string()))
}

/// Tracked files of `language` at `commitish`, repo-relative, sorted. Uses
/// `git ls-tree -r` so the file set is pinned to that commit, not the
/// working tree.
pub fn tracked_files_at(
    repo_root: &Path,
    commitish: &str,
    language: Language,
) -> Result<Vec<String>> {
    let out = git(
        repo_root,
        &["ls-tree", "-r", "-z", "--name-only", commitish],
    )?;
    let mut files: Vec<String> = out
        .split('\0')
        .filter(|s| !s.is_empty() && is_source_path(language, s))
        .map(|s| s.replace('\\', "/"))
        .filter(|rel| !outside_source_tree(language, rel))
        .collect();
    if language == Language::JsTs {
        crate::js_ts_inventory::filter(repo_root, Some(commitish), &mut files)?;
    }
    if language == Language::Python {
        require_regular_sources(repo_root, Some(commitish), &files, "Python")?;
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// Read a tracked file's bytes at `commitish` from the Git object store
/// (not the working tree), so extraction is pinned to a committed state
/// without checking it out.
pub fn read_file_at(repo_root: &Path, commitish: &str, rel_path: &str) -> Result<Vec<u8>> {
    let out = data_command(repo_root)?
        .args(["show", &format!("{commitish}:{rel_path}")])
        .output()
        .with_context(|| format!("git show {commitish}:{rel_path}"))?;
    if !out.status.success() {
        bail!(
            "git show {commitish}:{rel_path} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// Read many blobs from one commit through a single `git cat-file --batch`
/// process, in the order requested.
///
/// [`read_file_at`] spawns a `git show` per file, which is fine for one blob
/// and quietly dominates a whole-tree read: at a few hundred files the process
/// spawns cost far more than the parsing they feed, and a history walk pays it
/// once per commit. Batching makes commit-pinned extraction I/O-shaped like the
/// working-tree path.
///
/// Returns an error if any requested path is missing at that commit —
/// extraction never silently skips a tracked source file.
pub fn read_files_at(
    repo_root: &Path,
    commitish: &str,
    rel_paths: &[String],
) -> Result<Vec<Vec<u8>>> {
    read_files_at_bounded(repo_root, commitish, rel_paths, usize::MAX, usize::MAX)
}

pub fn read_files_at_bounded(
    repo_root: &Path,
    commitish: &str,
    rel_paths: &[String],
    file_limit: usize,
    mut remaining: usize,
) -> Result<Vec<Vec<u8>>> {
    if rel_paths.is_empty() {
        return Ok(Vec::new());
    }

    let mut child = data_command(repo_root)?
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn git cat-file --batch")?;

    // Feed requests from a separate thread: git blocks writing contents once
    // its stdout pipe fills, so a single thread that wrote every request before
    // reading any response would deadlock on a large tree.
    let mut stdin = child
        .stdin
        .take()
        .context("git cat-file stdin unavailable")?;
    let requests: Vec<String> = rel_paths
        .iter()
        .map(|rel_path| format!("{commitish}:{rel_path}\n"))
        .collect();
    let feeder = std::thread::spawn(move || -> std::io::Result<()> {
        for request in requests {
            stdin.write_all(request.as_bytes())?;
        }
        stdin.flush()
    });

    let mut stdout = BufReader::new(
        child
            .stdout
            .take()
            .context("git cat-file stdout unavailable")?,
    );
    let mut blobs = Vec::with_capacity(rel_paths.len());
    let result = (|| -> Result<()> {
        for rel_path in rel_paths {
            let mut header = String::new();
            let read = stdout
                .read_line(&mut header)
                .with_context(|| format!("read git cat-file header for {rel_path}"))?;
            anyhow::ensure!(
                read != 0,
                "git cat-file ended before answering for {commitish}:{rel_path}"
            );
            // `<oid> SP <type> SP <size> LF`, or `<object> SP missing LF`.
            let header = header.trim_end();
            let size: usize = header
                .rsplit(' ')
                .next()
                .and_then(|size| size.parse().ok())
                .with_context(|| {
                    format!("git cat-file could not read {commitish}:{rel_path}: {header}")
                })?;
            ensure!(
                size <= file_limit && size <= remaining,
                "Git source budget exceeded: {rel_path}"
            );
            remaining -= size;
            let mut blob = vec![0u8; size];
            stdout
                .read_exact(&mut blob)
                .with_context(|| format!("read {commitish}:{rel_path}"))?;
            // Each record is terminated by a newline git added, not content.
            let mut terminator = [0u8; 1];
            stdout
                .read_exact(&mut terminator)
                .with_context(|| format!("read record terminator for {rel_path}"))?;
            blobs.push(blob);
        }
        Ok(())
    })();

    if result.is_err() {
        // Release blocked pipe writers before joining the request feeder.
        let _ = child.kill();
    }
    let feed_result = feeder.join();
    let status = child.wait().context("wait for git cat-file")?;
    result?;
    match feed_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => bail!("failed writing git cat-file requests: {error}"),
        Err(_) => bail!("git cat-file request thread panicked"),
    }
    anyhow::ensure!(status.success(), "git cat-file --batch failed ({status})");
    Ok(blobs)
}

fn tracked_file_hash_at(
    repo_root: &Path,
    commitish: &str,
    rel_path: &str,
) -> Result<Option<String>> {
    let tracked = git(
        repo_root,
        &["ls-tree", "--name-only", commitish, "--", rel_path],
    )?;
    if tracked.lines().any(|path| path == rel_path) {
        Ok(Some(sha256_hex(&read_file_at(
            repo_root, commitish, rel_path,
        )?)))
    } else {
        Ok(None)
    }
}

/// Inventory of tracked files of `language`, repo-relative, forward-slash
/// normalized, sorted. Uses `git ls-files` so only *tracked* files enter
/// the snapshot — untracked or ignored files never produce facts.
pub fn tracked_files(repo_root: &Path, language: Language) -> Result<Vec<String>> {
    let mut args = vec!["ls-files", "-z", "--"];
    args.extend_from_slice(source_globs(language));
    let out = git(repo_root, &args)?;
    let mut files: Vec<String> = out
        .split('\0')
        .filter(|s| !s.is_empty() && is_source_path(language, s))
        .map(|s| s.replace('\\', "/"))
        .filter(|rel| !outside_source_tree(language, rel))
        .collect();
    if language == Language::JsTs {
        crate::js_ts_inventory::filter(repo_root, Some("HEAD"), &mut files)?;
    }
    if language == Language::Python {
        require_regular_sources(repo_root, Some("HEAD"), &files, "Python")?;
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// Inventory of the working tree's files of `language`: tracked and untracked,
/// minus ignored, minus files deleted from disk. Repo-relative, forward-slash
/// normalized, sorted.
pub fn worktree_files(repo_root: &Path, language: Language) -> Result<Vec<String>> {
    let mut args = vec![
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
    ];
    args.extend_from_slice(source_globs(language));
    let out = git(repo_root, &args)?;
    let mut files: Vec<String> = out
        .split('\0')
        .filter(|s| !s.is_empty() && is_source_path(language, s))
        .map(|s| s.replace('\\', "/"))
        .filter(|rel| !outside_source_tree(language, rel))
        .filter(|rel| {
            repo_root.join(rel).is_file()
                || (matches!(language, Language::JsTs | Language::Python)
                    && repo_root.join(rel).is_symlink())
        })
        .collect();
    if language == Language::JsTs {
        crate::js_ts_inventory::filter(repo_root, None, &mut files)?;
    }
    if language == Language::Python {
        require_regular_sources(repo_root, None, &files, "Python")?;
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// Python source identity requires regular blobs or unlinked working files.
pub fn require_regular_sources(
    root: &Path,
    commit: Option<&str>,
    files: &[String],
    label: &str,
) -> Result<()> {
    if let Some(commit) = commit {
        let tree = run(root, &["ls-tree", "-r", "-z", commit])?;
        let modes: BTreeMap<_, _> = tree
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .map(|entry| -> Result<_> {
                let (metadata, path) = entry.split_once('\t').context("Git tree entry")?;
                Ok((path, metadata.split(' ').next().context("Git file mode")?))
            })
            .collect::<Result<_>>()?;
        for file in files {
            ensure!(
                modes
                    .get(file.as_str())
                    .is_some_and(|mode| matches!(*mode, "100644" | "100755")),
                "{label} source must be a regular Git blob: {file}"
            );
        }
    } else {
        let root = root.canonicalize()?;
        for file in files {
            let path = root.join(file);
            ensure!(
                path.canonicalize()? == path && std::fs::symlink_metadata(&path)?.is_file(),
                "{label} source must be a regular file without linked ancestors: {file}"
            );
        }
    }
    Ok(())
}

/// Gather git state for the working tree containing `start_path`, as it is
/// now. No clean gate: `dirty` records whether the tree differs from `HEAD`,
/// and `tree_sha` names the tree that was actually read.
pub fn gather_worktree(start_path: &Path, language: Language) -> Result<GitState> {
    let root = repo_root(start_path)?;
    let root_path = Path::new(&root);
    let identity = repository_identity(root_path)?;
    let (commit_sha, _) = head_identity(root_path)?;
    let tree_sha = worktree_tree_sha(root_path)?;
    let dirty = is_dirty(root_path)?;
    let source_files = worktree_files(root_path, language)?;
    let cargo_lock_sha256 = worktree_file_hash(root_path, "Cargo.lock")?;
    Ok(GitState {
        repo_root: root,
        identity,
        commit_sha,
        tree_sha,
        worktree: true,
        dirty,
        language,
        source_files,
        cargo_lock_sha256,
    })
}

fn worktree_file_hash(repo_root: &Path, rel_path: &str) -> Result<Option<String>> {
    let path = repo_root.join(rel_path);
    if path.is_file() {
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        Ok(Some(sha256_hex(&bytes)))
    } else {
        Ok(None)
    }
}

/// Gather complete git state for a clean worktree containing `start_path`.
pub fn gather(start_path: &Path, language: Language) -> Result<GitState> {
    let root = repo_root(start_path)?;
    let root_path = Path::new(&root);
    require_clean(root_path).context("clean-worktree check failed")?;
    let identity = repository_identity(root_path)?;
    let (commit_sha, tree_sha) = head_identity(root_path)?;
    let source_files = tracked_files(root_path, language)?;
    let cargo_lock_sha256 = tracked_file_hash_at(root_path, &commit_sha, "Cargo.lock")?;
    Ok(GitState {
        repo_root: root,
        identity,
        commit_sha,
        tree_sha,
        worktree: false,
        dirty: false,
        language,
        source_files,
        cargo_lock_sha256,
    })
}

/// Gather git state pinned to an arbitrary `commitish` — no clean-worktree
/// check (we read committed blobs, not the working tree). Used by the
/// cross-commit diff engine.
pub fn gather_at(start_path: &Path, commitish: &str, language: Language) -> Result<GitState> {
    let root = repo_root(start_path)?;
    let root_path = Path::new(&root);
    let identity = repository_identity(root_path)?;
    let (commit_sha, tree_sha) = commit_identity(root_path, commitish)?;
    let source_files = tracked_files_at(root_path, commitish, language)?;
    let cargo_lock_sha256 = tracked_file_hash_at(root_path, &commit_sha, "Cargo.lock")?;
    Ok(GitState {
        repo_root: root,
        identity,
        commit_sha,
        tree_sha,
        worktree: false,
        dirty: false,
        language,
        source_files,
        cargo_lock_sha256,
    })
}

/// Pathspecs selecting the snapshot language's sources.
///
/// C++ uses one pathspec per extension. Callers still filter with
/// [`is_source_path`]: on a case-insensitive index, `*.C` can also name
/// a `.c` file, and `.c` is not C++.
pub const fn source_globs(language: Language) -> &'static [&'static str] {
    match language {
        Language::Rust => &["*.rs"],
        Language::Ruby => &["*.rb"],
        Language::Php => &["*.php"],
        Language::Go => &["*.go"],
        Language::Python => &["*.py"],
        Language::JsTs => &[
            crate::js_ts_inventory::POLICY_PATH,
            ":(icase)*.js",
            ":(icase)*.jsx",
            ":(icase)*.mjs",
            ":(icase)*.cjs",
            ":(icase)*.ts",
            ":(icase)*.tsx",
            ":(icase)*.mts",
            ":(icase)*.cts",
        ],
        Language::Cpp => &[
            ":(icase)*.cpp",
            ":(icase)*.cc",
            ":(icase)*.cxx",
            ":(icase)*.c++",
            ":(icase)*.C",
            ":(icase)*.h",
            ":(icase)*.hh",
            ":(icase)*.hpp",
            ":(icase)*.hxx",
            ":(icase)*.h++",
            ":(icase)*.ipp",
            ":(icase)*.inl",
            ":(icase)*.tpp",
        ],
    }
}

/// Whether `rel_path` is a source file of `language`.
///
/// Matching is ASCII case-insensitive, except C++ `.C`. A lowercase `.c`
/// file is C, which this adapter does not inventory. The case is the case
/// git recorded, so a case-insensitive volume does not invent a second file.
pub fn is_source_path(language: Language, rel_path: &str) -> bool {
    let Some(ext) = file_extension(rel_path) else {
        return false;
    };
    match language {
        Language::Rust => ext.eq_ignore_ascii_case("rs"),
        Language::Ruby => ext.eq_ignore_ascii_case("rb"),
        Language::Php => ext.eq_ignore_ascii_case("php"),
        Language::Go => ext.eq_ignore_ascii_case("go"),
        Language::Python => ext == "py",
        Language::JsTs => ["js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts"]
            .iter()
            .any(|candidate| ext.eq_ignore_ascii_case(candidate)),
        Language::Cpp => is_cpp_extension(ext),
    }
}

fn file_extension(rel_path: &str) -> Option<&str> {
    let name = rel_path.rsplit(['/', '\\']).next().unwrap_or(rel_path);
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() { None } else { Some(ext) }
}

/// Headers have no independently admitted translation-unit context.
pub fn is_cpp_header(rel_path: &str) -> bool {
    file_extension(rel_path).is_some_and(|ext| {
        ["h", "hh", "hpp", "hxx", "h++", "ipp", "inl", "tpp"]
            .iter()
            .any(|candidate| ext.eq_ignore_ascii_case(candidate))
    })
}

const CPP_EXTENSIONS: &[&str] = &[
    "cpp", "cc", "cxx", "c++", "h", "hh", "hpp", "hxx", "h++", "ipp", "inl", "tpp",
];

fn is_cpp_extension(ext: &str) -> bool {
    if ext == "C" {
        return true;
    }
    if ext.eq_ignore_ascii_case("c") {
        return false;
    }
    CPP_EXTENSIONS
        .iter()
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
}

/// Whether a file with the language's extension is nevertheless outside
/// the language's own idea of the source tree. The Go toolchain never
/// compiles a `vendor` directory (copies of dependencies) or a `testdata`
/// directory (inputs for tests) as part of `./...`, so neither yields
/// facts: a vendored dependency is an external target, exactly as it
/// would be from the module cache. Every other language inventories every
/// tracked file with its extension.
fn outside_source_tree(language: Language, rel_path: &str) -> bool {
    match language {
        Language::Go => rel_path.rsplit_once('/').is_some_and(|(directories, _)| {
            directories
                .split('/')
                .any(|segment| segment == "vendor" || segment == "testdata")
        }),
        Language::JsTs => crate::js_ts_inventory::dependency_path(rel_path),
        Language::Rust | Language::Ruby | Language::Php | Language::Cpp | Language::Python => false,
    }
}

/// Verify that a worktree still represents the exact commit captured by
/// [`gather`]. This closes the race between the initial clean-worktree check
/// and a potentially long semantic extraction.
pub fn verify_unchanged(state: &GitState) -> Result<()> {
    let root = Path::new(&state.repo_root);
    if state.worktree {
        let (commit_sha, _) = head_identity(root)?;
        let tree_sha = worktree_tree_sha(root)?;
        if commit_sha != state.commit_sha || tree_sha != state.tree_sha {
            bail!(
                "working tree changed during extraction: captured commit {} tree {}, now commit {} tree {}",
                state.commit_sha,
                state.tree_sha,
                commit_sha,
                tree_sha
            );
        }
        return Ok(());
    }
    require_clean(root).context("worktree changed during extraction")?;
    let (commit_sha, tree_sha) = head_identity(root)?;
    if commit_sha != state.commit_sha || tree_sha != state.tree_sha {
        bail!(
            "HEAD changed during extraction: captured commit {} tree {}, now commit {} tree {}",
            state.commit_sha,
            state.tree_sha,
            commit_sha,
            tree_sha
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn python_inventory_refuses_source_links_in_commits_and_worktrees() {
        let repo = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]).unwrap();
        git(repo.path(), &["config", "user.email", "t@example.com"]).unwrap();
        git(repo.path(), &["config", "user.name", "t"]).unwrap();
        let source = external.path().join("outside.py");
        std::fs::write(&source, "def outside(): pass\n").unwrap();
        let link = repo.path().join("linked.py");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert!(worktree_files(repo.path(), Language::Python).is_err());
        std::fs::write(&source, "def changed(): pass\n").unwrap();
        assert!(worktree_files(repo.path(), Language::Python).is_err());
        git(repo.path(), &["add", "linked.py"]).unwrap();
        git(repo.path(), &["commit", "-qm", "source link"]).unwrap();
        assert!(tracked_files_at(repo.path(), "HEAD", Language::Python).is_err());
        assert!(tracked_files(repo.path(), Language::Python).is_err());
        assert!(gather(repo.path(), Language::Python).is_err());
        assert!(gather_at(repo.path(), "HEAD", Language::Python).is_err());
        assert!(gather_worktree(repo.path(), Language::Python).is_err());
        std::fs::remove_file(source).unwrap();
        assert!(worktree_files(repo.path(), Language::Python).is_err());
        std::fs::remove_file(link).unwrap();
        std::fs::create_dir(repo.path().join("package")).unwrap();
        std::fs::write(repo.path().join("package/main.py"), "pass\n").unwrap();
        git(repo.path(), &["add", "-A"]).unwrap();
        git(repo.path(), &["commit", "-qm", "regular source"]).unwrap();
        assert_eq!(
            tracked_files_at(repo.path(), "HEAD", Language::Python).unwrap(),
            vec!["package/main.py"]
        );
        std::fs::remove_dir_all(repo.path().join("package")).unwrap();
        std::fs::write(external.path().join("main.py"), "pass\n").unwrap();
        std::os::unix::fs::symlink(external.path(), repo.path().join("package")).unwrap();
        assert!(worktree_files(repo.path(), Language::Python).is_err());
    }

    #[test]
    fn private_index_paths_named_in_the_same_instant_differ() {
        // Two callers in one microsecond used to receive the same path and
        // then raced on it: one seeded it, the other removed it mid-run.
        let first = private_index_path();
        let second = private_index_path();
        assert_ne!(first, second);
    }

    #[test]
    fn bounded_git_reads_refuse_before_allocating_oversized_blobs() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]).unwrap();
        git(repo.path(), &["config", "user.email", "t@example.com"]).unwrap();
        git(repo.path(), &["config", "user.name", "t"]).unwrap();
        std::fs::write(repo.path().join("large.py"), vec![b' '; 256 * 1024]).unwrap();
        for path in ["a.py", "b.py"] {
            std::fs::write(repo.path().join(path), "pass").unwrap();
        }
        git(repo.path(), &["add", "-A"]).unwrap();
        git(repo.path(), &["commit", "-qm", "bounded source fixture"]).unwrap();
        assert!(read_files_at_bounded(repo.path(), "HEAD", &["large.py".into()], 8, 8).is_err());
        let paths = ["a.py".into(), "b.py".into()];
        assert!(read_files_at_bounded(repo.path(), "HEAD", &paths, 4, 7).is_err());
        assert_eq!(
            read_files_at_bounded(repo.path(), "HEAD", &paths, 4, 8).unwrap(),
            vec![b"pass".to_vec(); 2]
        );
    }

    /// A detached worktree must materialize the commit's files and leave the
    /// original checkout, its HEAD, and its branch list untouched — that
    /// isolation is the whole reason semantic extraction can use it.
    #[test]
    fn detached_worktree_materializes_a_commit_without_disturbing_the_original() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path();
        git(repo, &["init", "-q", "-b", "main"]).expect("init repository");
        git(repo, &["config", "user.email", "t@example.com"]).expect("set email");
        git(repo, &["config", "user.name", "t"]).expect("set name");
        std::fs::write(repo.join("a.rs"), "fn first() {}\n").expect("write source");
        git(repo, &["add", "-A"]).expect("stage");
        git(repo, &["commit", "-qm", "first"]).expect("commit");
        let (first, _) = commit_identity(repo, "HEAD").expect("first commit");

        std::fs::write(repo.join("a.rs"), "fn second() {}\n").expect("rewrite source");
        git(repo, &["commit", "-qam", "second"]).expect("second commit");
        let (second, _) = commit_identity(repo, "HEAD").expect("second commit id");

        // Deliberately outside the repository: a worktree created inside it
        // would register as an untracked directory and fail the clean gate,
        // which is why extraction materializes into the system temp dir.
        let elsewhere = tempfile::tempdir().expect("second tempdir");
        let worktree = elsewhere.path().join("detached");
        add_detached_worktree(repo, &worktree, &first).expect("add worktree");

        assert_eq!(
            std::fs::read_to_string(worktree.join("a.rs")).expect("read worktree source"),
            "fn first() {}\n",
            "the worktree holds the requested commit, not the tip"
        );
        assert_eq!(
            commit_identity(repo, "HEAD").expect("original HEAD").0,
            second,
            "the original checkout still points at its own tip"
        );
        assert!(
            require_clean(repo).is_ok(),
            "the original worktree is undisturbed"
        );
        assert_eq!(
            git(repo, &["branch", "--list"]).expect("list branches"),
            "* main",
            "--detach creates no branch"
        );

        remove_worktree(repo, &worktree).expect("remove worktree");
        assert!(!worktree.exists(), "the worktree directory is gone");
        assert!(
            !git(repo, &["worktree", "list"])
                .expect("list worktrees")
                .contains("detached"),
            "the registration is pruned too"
        );
    }

    #[test]
    fn normalize_url_strips_credentials_and_git_suffix() {
        assert_eq!(
            normalize_url("https://user:token@github.com/org/repo.git"),
            "https://github.com/org/repo"
        );
        assert_eq!(
            normalize_url("git@github.com:org/repo.git"),
            "github.com:org/repo"
        );
        assert_eq!(
            sanitize_remote_url("file:///tmp/repo@local.git"),
            "file:///tmp/repo@local.git"
        );
        assert_eq!(
            normalize_url("https://github.com/org/repo"),
            "https://github.com/org/repo"
        );
    }

    #[test]
    fn repository_identity_never_serializes_remote_credentials() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(tmp.path(), &["init", "-q"]).expect("init repository");
        git(
            tmp.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://agent:super-secret@example.com/org/repo.git",
            ],
        )
        .expect("add origin");

        let identity = repository_identity(tmp.path()).expect("repository identity");
        assert_eq!(identity.id, "https://example.com/org/repo");
        assert_eq!(
            identity.origin.as_deref(),
            Some("https://example.com/org/repo.git")
        );
        let serialized = serde_json::to_string(&identity).expect("serialize identity");
        assert!(!serialized.contains("agent"));
        assert!(!serialized.contains("super-secret"));
    }

    #[test]
    fn inventory_selects_only_the_snapshot_language() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(tmp.path(), &["init", "-q"]).expect("init repository");
        git(tmp.path(), &["config", "user.name", "Codemap Test"]).expect("configure user name");
        git(
            tmp.path(),
            &["config", "user.email", "codemap@example.invalid"],
        )
        .expect("configure user email");
        std::fs::write(tmp.path().join("lib.rs"), "fn f() {}\n").expect("write rust");
        std::fs::write(tmp.path().join("app.rb"), "class User\nend\n").expect("write ruby");
        std::fs::write(tmp.path().join("notes.txt"), "prose\n").expect("write prose");
        for rel in [
            "cmd/app/main.go",
            "pkg/x/x.go",
            "vendor/dep/dep.go",
            "pkg/x/testdata/input.go",
        ] {
            let path = tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, "package p\n").expect("write go");
        }
        git(tmp.path(), &["add", "-A"]).expect("stage files");
        git(tmp.path(), &["commit", "-qm", "fixture"]).expect("commit fixture");

        let rust = gather(tmp.path(), Language::Rust).expect("gather rust state");
        assert_eq!(rust.source_files, vec!["lib.rs".to_string()]);
        assert_eq!(rust.language, Language::Rust);

        let go = gather(tmp.path(), Language::Go).expect("gather go state");
        assert_eq!(
            go.source_files,
            vec!["cmd/app/main.go", "pkg/x/x.go"],
            "vendor and testdata never enter the Go inventory"
        );
        let ruby = gather(tmp.path(), Language::Ruby).expect("gather ruby state");
        assert_eq!(ruby.source_files, vec!["app.rb".to_string()]);
        assert_eq!(ruby.language, Language::Ruby);
    }

    #[test]
    fn cpp_inventory_keeps_headers_and_capital_c_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(tmp.path(), &["init", "-q"]).expect("init repository");
        git(tmp.path(), &["config", "user.name", "Codemap Test"]).expect("configure user name");
        git(
            tmp.path(),
            &["config", "user.email", "codemap@example.invalid"],
        )
        .expect("configure user email");
        for rel in [
            "src/unit.cpp",
            "include/unit.hpp",
            "include/upper.HPP",
            "src/upper.CPP",
            "src/unit.C",
            "src/plain.c",
            "notes.txt",
        ] {
            let path = tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, "int value() { return 1; }\n").expect("write source");
        }
        git(tmp.path(), &["add", "-A"]).expect("stage files");
        git(tmp.path(), &["commit", "-qm", "fixture"]).expect("commit fixture");

        let cpp = gather(tmp.path(), Language::Cpp).expect("gather cpp state");
        assert_eq!(
            cpp.source_files,
            vec![
                "include/unit.hpp".to_string(),
                "include/upper.HPP".to_string(),
                "src/unit.C".to_string(),
                "src/unit.cpp".to_string(),
                "src/upper.CPP".to_string(),
            ]
        );
        assert_eq!(
            gather_worktree(tmp.path(), Language::Cpp)
                .unwrap()
                .source_files,
            cpp.source_files
        );
        assert_eq!(
            tracked_files_at(tmp.path(), "HEAD", Language::Cpp).unwrap(),
            cpp.source_files
        );
        let rust = gather(tmp.path(), Language::Rust).expect("gather rust");
        assert_eq!(rust.source_files.len(), 0);
    }

    #[test]
    fn verify_unchanged_rejects_worktree_drift() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(tmp.path(), &["init", "-q"]).expect("init repository");
        git(tmp.path(), &["config", "user.name", "Codemap Test"]).expect("configure user name");
        git(
            tmp.path(),
            &["config", "user.email", "codemap@example.invalid"],
        )
        .expect("configure user email");
        std::fs::write(tmp.path().join("lib.rs"), "fn committed() {}\n").expect("write source");
        git(tmp.path(), &["add", "lib.rs"]).expect("stage source");
        git(tmp.path(), &["commit", "-qm", "fixture"]).expect("commit fixture");

        let state = gather(tmp.path(), Language::Rust).expect("gather state");
        std::fs::write(tmp.path().join("lib.rs"), "fn drifted() {}\n").expect("drift source");

        let error = verify_unchanged(&state).expect_err("worktree drift must fail");
        assert!(error.to_string().contains("worktree changed"), "{error:#}");
    }

    #[test]
    fn verify_unchanged_rejects_clean_head_drift() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(tmp.path(), &["init", "-q"]).expect("init repository");
        git(tmp.path(), &["config", "user.name", "Codemap Test"]).expect("configure user name");
        git(
            tmp.path(),
            &["config", "user.email", "codemap@example.invalid"],
        )
        .expect("configure user email");
        std::fs::write(tmp.path().join("lib.rs"), "fn original() {}\n").expect("write source");
        git(tmp.path(), &["add", "lib.rs"]).expect("stage source");
        git(tmp.path(), &["commit", "-qm", "original"]).expect("commit original");

        let state = gather(tmp.path(), Language::Rust).expect("gather state");
        std::fs::write(tmp.path().join("lib.rs"), "fn replacement() {}\n").expect("replace source");
        git(tmp.path(), &["add", "lib.rs"]).expect("stage replacement");
        git(tmp.path(), &["commit", "-qm", "replacement"]).expect("commit replacement");

        let error = verify_unchanged(&state).expect_err("clean HEAD drift must fail");
        assert!(
            error.to_string().contains("HEAD changed during extraction"),
            "{error:#}"
        );
    }

    fn fixture_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(tmp.path(), &["init", "-q"]).expect("init repository");
        git(tmp.path(), &["config", "user.name", "Codemap Test"]).expect("configure user name");
        git(
            tmp.path(),
            &["config", "user.email", "codemap@example.invalid"],
        )
        .expect("configure user email");
        std::fs::write(tmp.path().join("lib.rs"), "fn committed() {}\n").expect("write source");
        std::fs::write(tmp.path().join("gone.rs"), "fn gone() {}\n").expect("write doomed source");
        std::fs::write(tmp.path().join(".gitignore"), "ignored.rs\n").expect("write ignore");
        git(tmp.path(), &["add", "-A"]).expect("stage fixture");
        git(tmp.path(), &["commit", "-qm", "fixture"]).expect("commit fixture");
        tmp
    }

    #[cfg(unix)]
    fn sentinel_hook(repo: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = repo.join(".git/hooks").join(name);
        std::fs::write(&path, b"#!/bin/sh\ntouch \"${0%/*}/EXECUTED\"\nexit 99\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    #[cfg(unix)]
    fn reads_disable_filesystem_monitors_and_preserve_the_original_index() {
        let fixture = fixture_repo();
        let repo = fixture.path();
        let original = gather(repo, Language::Rust).unwrap();
        let hook = sentinel_hook(repo, "monitor-fixture");
        git(repo, &["config", "core.fsmonitor", hook.to_str().unwrap()]).unwrap();
        let index = std::fs::read(repo.join(".git/index")).unwrap();
        std::fs::write(repo.join("lib.rs"), "fn committed() {}\n").unwrap();
        let repeated = gather(repo, Language::Rust).unwrap();
        assert_eq!(original.commit_sha, repeated.commit_sha);
        assert_eq!(original.tree_sha, repeated.tree_sha);
        assert_eq!(original.source_files, repeated.source_files);
        assert_eq!(std::fs::read(repo.join(".git/index")).unwrap(), index);
        assert!(!repo.join(".git/hooks/EXECUTED").exists());
        assert!(!is_dirty(repo).unwrap());
        assert!(!repo.join(".git/hooks/EXECUTED").exists());
    }

    #[test]
    #[cfg(unix)]
    fn reads_and_detached_checkout_disable_external_filters_and_hooks() {
        for kind in ["clean", "smudge", "process"] {
            let fixture = fixture_repo();
            let repo = fixture.path();
            std::fs::write(repo.join(".gitattributes"), "* filter=fixture\n").unwrap();
            git(repo, &["add", ".gitattributes"]).unwrap();
            git(repo, &["commit", "-qm", "attributes"]).unwrap();
            let filter = sentinel_hook(repo, "filter-fixture");
            sentinel_hook(repo, "post-checkout");
            git(
                repo,
                &[
                    "config",
                    &format!("filter.fixture.{kind}"),
                    filter.to_str().unwrap(),
                ],
            )
            .unwrap();
            git(repo, &["config", "filter.fixture.required", "true"]).unwrap();
            std::fs::write(repo.join("lib.rs"), "fn committed() {}\n").unwrap();
            require_clean(repo).unwrap();
            assert_eq!(
                read_file_at(repo, "HEAD", "lib.rs").unwrap(),
                b"fn committed() {}\n"
            );
            assert_eq!(
                read_files_at(repo, "HEAD", &["lib.rs".into()]).unwrap(),
                vec![b"fn committed() {}\n".to_vec()]
            );
            let elsewhere = tempfile::tempdir().unwrap();
            let detached = elsewhere.path().join("detached");
            add_detached_worktree(repo, &detached, "HEAD").unwrap();
            assert_eq!(
                std::fs::read(detached.join("lib.rs")).unwrap(),
                b"fn committed() {}\n"
            );
            remove_worktree(repo, &detached).unwrap();
            assert!(!repo.join(".git/hooks/EXECUTED").exists(), "{kind}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn missing_promisor_blobs_fail_without_invoking_the_remote_helper() {
        let fixture = fixture_repo();
        let repo = fixture.path();
        let blob = git(repo, &["rev-parse", "HEAD:lib.rs"]).unwrap();
        let helper = sentinel_hook(repo, "promisor-fixture");
        git(
            repo,
            &[
                "config",
                "remote.origin.url",
                &format!("ext::{}", helper.display()),
            ],
        )
        .unwrap();
        for (key, value) in [
            ("extensions.partialClone", "origin"),
            ("remote.origin.promisor", "true"),
            ("remote.origin.partialclonefilter", "blob:none"),
            ("protocol.ext.allow", "always"),
        ] {
            git(repo, &["config", key, value]).unwrap();
        }
        std::fs::remove_file(repo.join(".git/objects").join(&blob[..2]).join(&blob[2..])).unwrap();
        assert!(read_file_at(repo, "HEAD", "lib.rs").is_err());
        assert!(read_files_at(repo, "HEAD", &["lib.rs".into()]).is_err());
        assert!(!repo.join(".git/hooks/EXECUTED").exists());
    }

    #[test]
    fn worktree_tree_sha_equals_head_tree_when_clean() {
        let repo = fixture_repo();
        let (_, head_tree) = head_identity(repo.path()).expect("head identity");
        assert_eq!(
            worktree_tree_sha(repo.path()).expect("worktree tree"),
            head_tree
        );
        assert!(
            require_clean(repo.path()).is_ok(),
            "computing the tree must not stage anything in the user's index"
        );
    }

    #[test]
    fn worktree_state_reads_the_tree_as_it_is() {
        let repo = fixture_repo();
        let (_, head_tree) = head_identity(repo.path()).expect("head identity");
        std::fs::write(repo.path().join("lib.rs"), "fn edited() {}\n").expect("edit source");
        std::fs::write(repo.path().join("new.rs"), "fn new() {}\n").expect("add source");
        std::fs::write(repo.path().join("ignored.rs"), "fn hidden() {}\n").expect("add ignored");
        std::fs::remove_file(repo.path().join("gone.rs")).expect("delete source");

        let state = gather_worktree(repo.path(), Language::Rust).expect("gather worktree");
        assert!(state.worktree);
        assert!(state.dirty);
        assert_ne!(
            state.tree_sha, head_tree,
            "a dirty tree has its own identity"
        );
        assert_eq!(
            state.source_files,
            vec!["lib.rs".to_string(), "new.rs".to_string()],
            "untracked files enter, deleted and ignored files do not"
        );
        assert_eq!(
            worktree_tree_sha(repo.path()).expect("second tree"),
            state.tree_sha,
            "the working-tree identity is deterministic for the same content"
        );
        assert!(
            verify_unchanged(&state).is_ok(),
            "an unchanged dirty tree still verifies"
        );
        assert!(
            git(repo.path(), &["diff", "--cached", "--name-only"])
                .expect("staged files")
                .is_empty(),
            "the user's index is untouched"
        );

        std::fs::write(repo.path().join("new.rs"), "fn changed() {}\n").expect("drift source");
        let error = verify_unchanged(&state).expect_err("drift must fail");
        assert!(
            error
                .to_string()
                .contains("working tree changed during extraction"),
            "{error:#}"
        );
    }

    #[test]
    fn worktree_state_on_a_clean_tree_matches_commit_state() {
        let repo = fixture_repo();
        let clean = gather(repo.path(), Language::Rust).expect("gather clean");
        let worktree = gather_worktree(repo.path(), Language::Rust).expect("gather worktree");
        assert!(!worktree.dirty);
        assert_eq!(worktree.commit_sha, clean.commit_sha);
        assert_eq!(worktree.tree_sha, clean.tree_sha);
        assert_eq!(worktree.source_files, clean.source_files);
        assert_eq!(worktree.cargo_lock_sha256, clean.cargo_lock_sha256);
    }

    #[test]
    fn local_identity_is_stable_for_same_abspath() {
        // Use a hermetic temp dir that is NOT inside any git repo, so
        // `repository_identity` reliably falls back to the local (non-canonical)
        // identity instead of inheriting a host repo's origin.
        let tmp = tempfile::tempdir().expect("tempdir");
        let a = repository_identity(tmp.path()).unwrap();
        let b = repository_identity(tmp.path()).unwrap();
        assert_eq!(a.id, b.id);
        assert!(!a.is_canonical);
        assert!(a.id.starts_with("local:"));
    }

    #[test]
    fn gather_hashes_tracked_root_cargo_lock() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(tmp.path(), &["init", "-q"]).expect("init repository");
        git(tmp.path(), &["config", "user.name", "Codemap Test"]).expect("configure user name");
        git(
            tmp.path(),
            &["config", "user.email", "codemap@example.invalid"],
        )
        .expect("configure user email");
        std::fs::write(tmp.path().join("lib.rs"), "fn committed() {}\n").expect("source");
        std::fs::write(tmp.path().join("Cargo.lock"), "lock bytes\n").expect("lockfile");
        git(tmp.path(), &["add", "lib.rs", "Cargo.lock"]).expect("stage fixture");
        git(tmp.path(), &["commit", "-qm", "fixture"]).expect("commit fixture");

        let state = gather(tmp.path(), Language::Rust).expect("gather state");
        assert_eq!(
            state.cargo_lock_sha256.as_deref(),
            Some(sha256_hex(b"lock bytes\n").as_str())
        );
    }
}
