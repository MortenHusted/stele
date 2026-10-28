//! Source identity: SHA-256 over the sorted path, mode and git blob ID of
//! every tracked file, leaving out the two ledgers beside the catalog root.
//! Working-tree blob IDs come from `git hash-object`, which applies the
//! repository's line-ending rules, so a clean checkout has exactly its
//! commit tree's identity on every platform.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
};

use crate::{APPROVALS_FILE, RUNS_FILE, WorkingTree, digest::sha256};

/// The mode git gives a submodule's entry, whose object is a commit.
const GITLINK: &str = "160000";

/// The git work tree holding a catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub root: PathBuf,
    /// The catalog directory's path from `root`, ending in `/` unless empty.
    catalog_prefix: String,
    /// The two ledgers, by their paths from `root`.
    ledgers: [String; 2],
}

impl Repo {
    /// The work tree holding the catalog directory `catalog`.
    pub fn of_catalog(catalog: &Path) -> Result<Self, String> {
        let output = git(catalog, &["rev-parse", "--show-toplevel", "--show-prefix"])?;
        let mut lines = output.lines();
        let root = lines.next().unwrap_or_default();
        let prefix = lines.next().unwrap_or_default();
        Ok(Self {
            root: root.into(),
            catalog_prefix: prefix.into(),
            ledgers: [RUNS_FILE, APPROVALS_FILE].map(|file| format!("{prefix}{file}")),
        })
    }

    /// The file `name` in the catalog directory, such as its approval log,
    /// as `commit` has it; `None` when that commit has no such file.
    pub fn catalog_file_at(&self, commit: &str, name: &str) -> Result<Option<String>, String> {
        let path = format!("{}{name}", self.catalog_prefix);
        let listed = git(&self.root, &["ls-tree", "--name-only", commit, "--", &path])?;
        if listed.is_empty() {
            return Ok(None);
        }
        git(&self.root, &["show", &format!("{commit}:{path}")]).map(Some)
    }

    /// The identity of the tracked files as they are in the working tree. A
    /// tracked file missing from the working tree is left out, as a commit
    /// that deletes it would leave it out.
    pub fn working_tree_identity(&self) -> Result<String, String> {
        let listing = git(&self.root, &["ls-files", "-s", "-z"])?;
        let mut modes = BTreeMap::new();
        let mut files = Vec::new();
        let mut blobs = BTreeMap::new();
        for entry in records(&listing) {
            let unexpected = || format!("unexpected `git ls-files -s -z` entry {entry:?}");
            let (meta, path) = entry.split_once('\t').ok_or_else(unexpected)?;
            let mut meta = meta.split(' ');
            let (Some(mode), Some(object)) = (meta.next(), meta.next()) else {
                return Err(unexpected());
            };
            if self.is_ledger(path) {
                continue;
            }
            modes.insert(path, mode);
            if mode == GITLINK {
                if let Some(commit) = self.submodule_commit(path, object)? {
                    blobs.insert(path, commit);
                }
                continue;
            }
            match fs::symlink_metadata(self.root.join(path)) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    blobs.insert(path, self.link_blob(path)?);
                }
                Ok(_) => files.push(path),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("cannot read {path}: {e}")),
            }
        }
        let mut input = String::new();
        for path in &files {
            input.push_str(&c_quoted(path));
            input.push('\n');
        }
        let hashed = git_with_input(
            &self.root,
            &["hash-object", "--stdin-paths"],
            input.into_bytes(),
        )?;
        let hashed: Vec<&str> = hashed.lines().collect();
        if hashed.len() != files.len() {
            return Err(format!(
                "`git hash-object --stdin-paths` hashed {} of {} files",
                hashed.len(),
                files.len()
            ));
        }
        blobs.extend(files.into_iter().zip(hashed.into_iter().map(String::from)));
        Ok(identity(
            blobs
                .iter()
                .map(|(path, blob)| (*path, modes[path], blob.as_str())),
        ))
    }

    /// The files a path anchor may name, tracked or untracked, present and
    /// not ignored, with the working tree's identity.
    pub fn working_tree(&self) -> Result<WorkingTree, String> {
        let listing = git(
            &self.root,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        )?;
        let files = records(&listing)
            .filter(|path| self.root.join(path).is_file())
            .map(String::from)
            .collect();
        Ok(WorkingTree {
            files,
            identity: self.working_tree_identity()?,
        })
    }

    /// The identity of the tracked files as `commit` has them.
    pub fn commit_identity(&self, commit: &str) -> Result<String, String> {
        let listing = git(&self.root, &["ls-tree", "-r", "-z", commit])?;
        let mut entries = Vec::new();
        for entry in records(&listing) {
            let unexpected = || format!("unexpected `git ls-tree -r -z` entry {entry:?}");
            let (meta, path) = entry.split_once('\t').ok_or_else(unexpected)?;
            let mut meta = meta.split(' ');
            let (Some(mode), Some(_), Some(blob)) = (meta.next(), meta.next(), meta.next()) else {
                return Err(unexpected());
            };
            if !self.is_ledger(path) {
                entries.push((path, mode, blob));
            }
        }
        Ok(identity(entries))
    }

    /// The commit HEAD points to.
    pub fn head(&self) -> Result<String, String> {
        Ok(git(&self.root, &["rev-parse", "--verify", "HEAD"])?
            .trim()
            .to_string())
    }

    /// Whether the working tree has HEAD's identity.
    pub fn matches_head(&self) -> Result<bool, String> {
        Ok(self.working_tree_identity()? == self.commit_identity("HEAD")?)
    }

    /// The diff of the tracked files in the working tree against `base`,
    /// without the two ledgers.
    pub fn diff(&self, base: &str) -> Result<String, String> {
        let excluded = self
            .ledgers
            .each_ref()
            .map(|path| format!(":(exclude){path}"));
        let mut args = vec!["diff", "--no-color", "--no-ext-diff", base, "--", "."];
        args.extend(excluded.iter().map(String::as_str));
        git(&self.root, &args)
    }

    fn is_ledger(&self, path: &str) -> bool {
        self.ledgers.iter().any(|ledger| ledger == path)
    }

    /// The commit a submodule has checked out, the commit the index records
    /// for it when it is not checked out, and `None` when its directory is
    /// gone, as for a deleted file.
    fn submodule_commit(&self, path: &str, recorded: &str) -> Result<Option<String>, String> {
        let dir = self.root.join(path);
        if !dir.exists() {
            return Ok(None);
        }
        if !dir.join(".git").exists() {
            return Ok(Some(recorded.into()));
        }
        let head = git(&dir, &["rev-parse", "--verify", "HEAD"])?;
        Ok(Some(head.trim().to_string()))
    }

    /// The blob ID of a symlink's own text, which `git hash-object <path>`
    /// would not give because it follows the link.
    fn link_blob(&self, path: &str) -> Result<String, String> {
        let target = fs::read_link(self.root.join(path))
            .map_err(|e| format!("cannot read the link {path}: {e}"))?;
        #[cfg(unix)]
        let text = std::os::unix::ffi::OsStrExt::as_bytes(target.as_os_str()).to_vec();
        #[cfg(not(unix))]
        let text = target.to_string_lossy().replace('\\', "/").into_bytes();
        Ok(
            git_with_input(&self.root, &["hash-object", "--stdin"], text)?
                .trim()
                .to_string(),
        )
    }
}

fn identity<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>) -> String {
    let lines: BTreeMap<&str, String> = entries
        .into_iter()
        .map(|(path, mode, blob)| (path, format!("{mode} {blob} {path}\n")))
        .collect();
    sha256(&lines.into_values().collect::<String>())
}

fn records(output: &str) -> impl Iterator<Item = &str> {
    output.split('\0').filter(|record| !record.is_empty())
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    run_git(dir, args, None)
}

fn git_with_input(dir: &Path, args: &[&str], input: Vec<u8>) -> Result<String, String> {
    run_git(dir, args, Some(input))
}

fn run_git(dir: &Path, args: &[&str], input: Option<Vec<u8>>) -> Result<String, String> {
    let command = format!("git {}", args.join(" "));
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run `{command}`: {e}"))?;
    // Feed stdin from a thread so a large output cannot block the input.
    let writer = child.stdin.take().zip(input).map(|(mut stdin, input)| {
        thread::spawn(move || {
            let _ = stdin.write_all(&input);
        })
    });
    let output = child
        .wait_with_output()
        .map_err(|e| format!("`{command}` failed: {e}"))?;
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    if !output.status.success() {
        return Err(format!(
            "`{command}` failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `git hash-object --stdin-paths` unquotes a line that starts with `"` and
/// trims a trailing carriage return, so every path is sent C-quoted.
fn c_quoted(path: &str) -> String {
    let mut quoted = String::from("\"");
    for byte in path.bytes() {
        match byte {
            b'"' => quoted.push_str("\\\""),
            b'\\' => quoted.push_str("\\\\"),
            b'\n' => quoted.push_str("\\n"),
            b'\r' => quoted.push_str("\\r"),
            b'\t' => quoted.push_str("\\t"),
            0x20..=0x7e => quoted.push(char::from(byte)),
            _ => {
                let _ = write!(quoted, "\\{byte:03o}");
            }
        }
    }
    quoted.push('"');
    quoted
}
