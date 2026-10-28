//! Captured Python operator bytes and complete inventories; no application execution.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::runtime::{CapturedRuntime, RuntimeRole};
use crate::schema::sha256_hex;

const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
type FileStamp = (u64, u64, u64, i64, i64, i64, i64);

/// Execution profiles must include this policy alongside captured content identity.
pub const MATERIALIZATION_POLICY: &str = "python-operator-copy/mtime-1/1";

/// Execution profiles must include the bounded read and metadata policy.
pub const GUARD_POLICY: &str = "python-captured-reads/1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorRoots {
    pub runtime: PathBuf,
    /// Absent for parser-only captures; native queries require a closed compiler root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler: Option<PathBuf>,
}

impl OperatorRoots {
    fn paths(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.runtime.as_path()).chain(self.compiler.as_deref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
}

/// Every file and directory below both roots must be declared. Missing entries
/// in complete directory listings establish negative lookups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorManifest {
    pub files: BTreeMap<String, String>,
    pub directories: BTreeMap<String, BTreeMap<String, EntryKind>>,
}

pub struct CapturedOperator {
    roots: OperatorRoots,
    source: PathBuf,
    manifest: OperatorManifest,
    files: BTreeMap<String, Vec<u8>>,
    fingerprint: String,
}

impl CapturedOperator {
    pub fn capture(
        manifest: &OperatorManifest,
        roots: &OperatorRoots,
        source: &Path,
    ) -> Result<Self> {
        validate_roots(roots, source)?;
        validate_manifest(manifest, roots)?;
        verify_directories(manifest, roots)?;
        let mut files = BTreeMap::new();
        let mut total = 0;
        for (name, digest) in &manifest.files {
            let (bytes, _) = read_file(&logical_path(name, roots)?)
                .with_context(|| format!("capture Python operator file: {name}"))?;
            total += bytes.len();
            ensure!(
                total <= MAX_TOTAL_BYTES,
                "Python operator byte budget exceeded"
            );
            ensure!(
                sha256_hex(&bytes) == *digest,
                "Python operator content mismatch: {name}"
            );
            files.insert(name.clone(), bytes);
        }
        let fingerprint = sha256_hex(&serde_json::to_vec(&(
            "python-operator-inputs/1",
            manifest,
        ))?);
        let captured = Self {
            roots: roots.clone(),
            source: source.to_owned(),
            manifest: manifest.clone(),
            files,
            fingerprint,
        };
        captured.verify_unchanged()?;
        Ok(captured)
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub const fn manifest(&self) -> &OperatorManifest {
        &self.manifest
    }

    pub fn read(&self, logical: &str) -> Result<&[u8]> {
        Ok(self
            .files
            .get(logical)
            .context("unmanifested Python operator read")?)
    }

    /// Restrict OS reads to captured operator/runtime entries and owned state.
    /// Ancestor directories allow directory access for locating those entries;
    /// their unlisted children do not gain file access.
    /// The launcher must also enforce captured compiler observations and freeze
    /// executable imports before accepting source data.
    pub fn guard_policy(&self, state: &Path, captured_runtime: &CapturedRuntime) -> Result<String> {
        self.verify_unchanged()?;
        captured_runtime.verify_unchanged()?;
        ensure!(
            state.is_dir() && fs::canonicalize(state)? == state,
            "Python state needs a canonical directory"
        );
        for root in self
            .roots
            .paths()
            .chain(std::iter::once(self.source.as_path()))
            .chain(
                captured_runtime
                    .manifest()
                    .files
                    .values()
                    .map(|file| file.path.as_path()),
            )
        {
            ensure!(
                !state.starts_with(root) && !root.starts_with(state),
                "Python state must be outside captured inputs and source roots"
            );
        }
        let executable = captured_runtime
            .manifest()
            .files
            .values()
            .find(|file| file.role == RuntimeRole::Python)
            .context("Python guard needs a captured Python runtime")?;
        let interpreter_pin = &self.manifest.files["{runtime}/bin/python3.14"];
        ensure!(
            executable.path == self.roots.runtime.join("bin/python3.14")
                && executable.sha256 == *interpreter_pin,
            "Python guard must select its captured interpreter"
        );
        let mut reads = self
            .manifest
            .files
            .keys()
            .chain(self.manifest.directories.keys())
            .map(|name| logical_path(name, &self.roots))
            .collect::<Result<BTreeSet<_>>>()?;
        reads.extend(
            captured_runtime
                .manifest()
                .files
                .values()
                .map(|file| file.path.clone()),
        );
        let mut ancestors = BTreeSet::new();
        for path in reads
            .iter()
            .map(PathBuf::as_path)
            .chain(std::iter::once(state))
        {
            ancestors.extend(path.ancestors().skip(1).map(Path::to_path_buf));
        }
        let mut policy = crate::runtime::guard_policy(state, &executable.path)?;
        policy.push_str(" (deny file-read*) (deny process-fork)");
        write!(
            policy,
            " (allow file-read* (subpath {}))",
            serde_json::to_string(state)?
        )?;
        reads.extend(ancestors);
        for path in reads {
            write!(
                policy,
                " (allow file-read* (literal {}))",
                serde_json::to_string(&path)?
            )?;
        }
        ensure!(
            policy.len() <= 8 * 1024 * 1024,
            "Python guard policy budget"
        );
        Ok(policy)
    }

    pub fn verify_unchanged(&self) -> Result<()> {
        validate_roots(&self.roots, &self.source)?;
        verify_directories(&self.manifest, &self.roots)?;
        let mut stamps = Vec::new();
        for (name, bytes) in &self.files {
            let path = logical_path(name, &self.roots)?;
            let (observed, previous) = read_file(&path)?;
            ensure!(observed == *bytes, "Python operator changed: {name}");
            stamps.push((path, previous));
        }
        verify_directories(&self.manifest, &self.roots)?;
        verify_stamps(&stamps)
    }

    /// Create an exclusive private copy from captured bytes, with deterministic
    /// modification times for `CPython`'s source/bytecode selection. The launcher
    /// must keep this copy outside the guard's writable state directory.
    pub fn materialize(&self, destination: &Path) -> Result<OperatorRoots> {
        self.verify_unchanged()?;
        let parent = destination.parent().context("Python copy needs a parent")?;
        ensure!(
            destination.is_absolute() && fs::canonicalize(parent)? == parent,
            "Python copy needs a canonical absolute parent"
        );
        for root in self
            .roots
            .paths()
            .chain(std::iter::once(self.source.as_path()))
        {
            ensure!(
                !destination.starts_with(root) && !root.starts_with(destination),
                "Python copy must be outside original roots"
            );
        }
        fs::create_dir(destination)?;
        let roots = OperatorRoots {
            runtime: destination.join("runtime"),
            compiler: self
                .roots
                .compiler
                .as_ref()
                .map(|_| destination.join("compiler")),
        };
        for name in self.manifest.directories.keys() {
            fs::create_dir_all(logical_path(name, &roots)?)?;
        }
        for (name, bytes) in &self.files {
            let path = logical_path(name, &roots)?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)?;
            std::io::Write::write_all(&mut file, bytes)?;
            file.set_times(
                fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
            )?;
            let mode = if name == "{runtime}/bin/python3.14" {
                0o555
            } else {
                0o444
            };
            fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(mode))?;
        }
        for name in self.manifest.directories.keys().rev() {
            let path = logical_path(name, &roots)?;
            fs::File::open(&path)?.set_times(
                fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
            )?;
        }
        fs::File::open(destination)?.set_times(
            fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
        )?;
        let copied = Self::capture(&self.manifest, &roots, &self.source)?;
        ensure!(
            copied.fingerprint == self.fingerprint,
            "Python copy identity changed"
        );
        self.verify_unchanged()?;
        Ok(roots)
    }
}

fn validate_roots(roots: &OperatorRoots, source: &Path) -> Result<()> {
    let all = roots
        .paths()
        .chain(std::iter::once(source))
        .collect::<Vec<_>>();
    for (index, root) in all.iter().enumerate() {
        ensure!(
            root.is_absolute()
                && fs::symlink_metadata(root)?.is_dir()
                && fs::canonicalize(root)? == *root,
            "Python roots must be canonical directories"
        );
        for other in &all[..index] {
            ensure!(
                !root.starts_with(other) && !other.starts_with(root),
                "Python operator and application roots must be disjoint"
            );
        }
    }
    Ok(())
}

fn logical_path(name: &str, roots: &OperatorRoots) -> Result<PathBuf> {
    let (root, relative) = name.split_once('/').unwrap_or((name, ""));
    let base = match root {
        "{runtime}" => &roots.runtime,
        "{compiler}" => roots
            .compiler
            .as_ref()
            .context("runtime-only Python operator has no compiler root")?,
        _ => anyhow::bail!("undeclared Python operator root"),
    };
    ensure!(
        relative.is_empty() && !name.contains('/')
            || relative.split('/').all(|part| {
                !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', '\0'])
            }),
        "Python operator logical path must be normalized"
    );
    Ok(if relative.is_empty() {
        base.clone()
    } else {
        base.join(relative)
    })
}

fn parent_entry<'a>(name: &str, manifest: &'a OperatorManifest) -> Result<&'a EntryKind> {
    let (parent, basename) = name
        .rsplit_once('/')
        .context("Python entry needs a relative name")?;
    manifest
        .directories
        .get(parent)
        .and_then(|entries| entries.get(basename))
        .context("Python operator parent entry is missing")
}

fn validate_manifest(manifest: &OperatorManifest, roots: &OperatorRoots) -> Result<()> {
    let entries = manifest
        .directories
        .values()
        .map(BTreeMap::len)
        .sum::<usize>();
    ensure!(
        !manifest.files.is_empty()
            && manifest.files.len() <= MAX_ENTRIES
            && manifest.directories.len() <= MAX_ENTRIES
            && entries <= MAX_ENTRIES,
        "Python operator inventory budget exceeded"
    );
    for root in std::iter::once("{runtime}").chain(roots.compiler.as_ref().map(|_| "{compiler}")) {
        ensure!(
            manifest.directories.contains_key(root),
            "missing Python root inventory"
        );
    }
    ensure!(
        manifest.files.contains_key("{runtime}/bin/python3.14")
            && manifest
                .directories
                .contains_key("{runtime}/lib/python3.14"),
        "Python operator requires the selected interpreter and stdlib inventory"
    );
    for absent in [
        "{runtime}/lib/python314.zip",
        "{runtime}/lib/python3.14/site-packages",
    ] {
        ensure!(
            !manifest.files.contains_key(absent) && !manifest.directories.contains_key(absent),
            "ambient Python ZIP or site-package entry refused"
        );
    }
    if roots.compiler.is_some() {
        ensure!(
            manifest.directories["{compiler}"]
                == BTreeMap::from([
                    ("mypy".to_owned(), EntryKind::Directory),
                    ("mypyc".to_owned(), EntryKind::Directory),
                    ("dependencies".to_owned(), EntryKind::Directory),
                ]),
            "Python compiler root must contain only mypy, mypyc, and dependencies"
        );
    }
    for (name, digest) in &manifest.files {
        logical_path(name, roots)?;
        ensure!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "Python operator pin must be a lowercase SHA-256"
        );
        ensure!(
            parent_entry(name, manifest)? == &EntryKind::File,
            "Python file inventory mismatch"
        );
    }
    for (name, entries) in &manifest.directories {
        logical_path(name, roots)?;
        if name.contains('/') {
            ensure!(
                parent_entry(name, manifest)? == &EntryKind::Directory,
                "Python directory inventory mismatch"
            );
        }
        for (basename, kind) in entries {
            let child = format!("{name}/{basename}");
            logical_path(&child, roots)?;
            ensure!(
                !basename.contains('/'),
                "Python directory entry must be a basename"
            );
            let declared = match kind {
                EntryKind::File => manifest.files.contains_key(&child),
                EntryKind::Directory => manifest.directories.contains_key(&child),
            };
            ensure!(declared, "incomplete Python operator inventory: {child}");
        }
    }
    Ok(())
}

fn verify_directories(manifest: &OperatorManifest, roots: &OperatorRoots) -> Result<()> {
    let mut stamps = Vec::new();
    for (name, expected) in &manifest.directories {
        let path = logical_path(name, roots)?;
        let before = fs::symlink_metadata(&path)?;
        ensure!(
            before.is_dir() && fs::canonicalize(&path)? == path,
            "Python directory alias refused"
        );
        let mut observed = BTreeMap::new();
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let kind = if kind.is_file() {
                EntryKind::File
            } else {
                ensure!(kind.is_dir(), "Python symlink or special entry refused");
                EntryKind::Directory
            };
            let basename = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Python entry must have a UTF-8 name"))?;
            observed.insert(basename, kind);
            ensure!(
                observed.len() <= MAX_ENTRIES,
                "Python directory budget exceeded"
            );
        }
        ensure!(
            observed == *expected,
            "Python directory inventory changed: {name}"
        );
        stamps.push((path, stamp(&before)));
    }
    verify_stamps(&stamps)
}

fn verify_stamps(stamps: &[(PathBuf, FileStamp)]) -> Result<()> {
    for (path, previous) in stamps {
        ensure!(
            stamp(&fs::symlink_metadata(path)?) == *previous && fs::canonicalize(path)? == *path,
            "Python operator changed during full verification"
        );
    }
    Ok(())
}

fn read_file(path: &Path) -> Result<(Vec<u8>, FileStamp)> {
    ensure!(fs::canonicalize(path)? == path, "Python file alias refused");
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= MAX_FILE_BYTES,
        "bounded regular Python operator file required"
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_FILE_BYTES,
        "Python operator file grew beyond budget"
    );
    ensure!(
        stamp(&before) == stamp(&file.metadata()?)
            && stamp(&before) == stamp(&fs::symlink_metadata(path)?)
            && fs::canonicalize(path)? == path,
        "Python operator changed while reading"
    );
    Ok((bytes, stamp(&before)))
}

fn stamp(metadata: &fs::Metadata) -> FileStamp {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{CacheFamily, RuntimeFile, RuntimeManifest};
    use std::os::unix::fs::symlink;

    fn fixture() -> (tempfile::TempDir, OperatorRoots, PathBuf, OperatorManifest) {
        let scratch = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(scratch.path()).unwrap();
        let roots = OperatorRoots {
            runtime: base.join("runtime"),
            compiler: Some(base.join("compiler")),
        };
        let source = base.join("application");
        fs::create_dir(&source).unwrap();
        let directories = BTreeMap::from([
            (
                "{runtime}".to_owned(),
                BTreeMap::from([
                    ("bin".to_owned(), EntryKind::Directory),
                    ("lib".to_owned(), EntryKind::Directory),
                ]),
            ),
            (
                "{runtime}/bin".to_owned(),
                BTreeMap::from([("python3.14".to_owned(), EntryKind::File)]),
            ),
            (
                "{runtime}/lib".to_owned(),
                BTreeMap::from([("python3.14".to_owned(), EntryKind::Directory)]),
            ),
            (
                "{runtime}/lib/python3.14".to_owned(),
                BTreeMap::from([
                    ("__pycache__".to_owned(), EntryKind::Directory),
                    ("encoding.py".to_owned(), EntryKind::File),
                ]),
            ),
            (
                "{runtime}/lib/python3.14/__pycache__".to_owned(),
                BTreeMap::from([("encoding.cpython-314.pyc".to_owned(), EntryKind::File)]),
            ),
            (
                "{compiler}".to_owned(),
                BTreeMap::from([
                    ("mypy".to_owned(), EntryKind::Directory),
                    ("mypyc".to_owned(), EntryKind::Directory),
                    ("dependencies".to_owned(), EntryKind::Directory),
                ]),
            ),
            (
                "{compiler}/mypy".to_owned(),
                BTreeMap::from([("entry.py".to_owned(), EntryKind::File)]),
            ),
            ("{compiler}/mypyc".to_owned(), BTreeMap::new()),
            ("{compiler}/dependencies".to_owned(), BTreeMap::new()),
        ]);
        for name in directories.keys() {
            fs::create_dir_all(logical_path(name, &roots).unwrap()).unwrap();
        }
        let contents: [(&str, &[u8]); 4] = [
            ("{runtime}/bin/python3.14", b"owned interpreter"),
            (
                "{runtime}/lib/python3.14/encoding.py",
                b"def encode(x):\n    return x\n",
            ),
            (
                "{runtime}/lib/python3.14/__pycache__/encoding.cpython-314.pyc",
                &[0, 255, 254, 1],
            ),
            (
                "{compiler}/mypy/entry.py",
                b"def compile_data(x):\n    return x\n",
            ),
        ];
        let mut files = BTreeMap::new();
        for (name, bytes) in contents {
            fs::write(logical_path(name, &roots).unwrap(), bytes).unwrap();
            files.insert(name.to_owned(), sha256_hex(bytes));
        }
        (
            scratch,
            roots,
            source,
            OperatorManifest { files, directories },
        )
    }

    #[test]
    fn binary_capture_is_relocatable_and_preserves_pinned_bytes() {
        let (_first, roots, source, manifest) = fixture();
        let captured = CapturedOperator::capture(&manifest, &roots, &source).unwrap();
        assert_eq!(
            captured.fingerprint(),
            "617ef942e9ebf253e078a12eaf804ce83bc2f022f8e90e3e8b4c2c562237f3c5"
        );
        assert_eq!(
            captured
                .read("{runtime}/lib/python3.14/__pycache__/encoding.cpython-314.pyc")
                .unwrap(),
            [0, 255, 254, 1]
        );
        assert!(captured.read("{runtime}/unmanifested.py").is_err());
        let (_second, relocated, source, second_manifest) = fixture();
        assert_eq!(manifest, second_manifest);
        assert_eq!(
            captured.fingerprint(),
            CapturedOperator::capture(&manifest, &relocated, &source)
                .unwrap()
                .fingerprint()
        );
        captured.verify_unchanged().unwrap();
    }

    fn parser_fixture() -> (tempfile::TempDir, OperatorRoots, PathBuf, OperatorManifest) {
        let (scratch, mut roots, source, mut manifest) = fixture();
        roots.compiler = None;
        manifest
            .files
            .retain(|name, _| name.starts_with("{runtime}/"));
        manifest
            .directories
            .retain(|name, _| name == "{runtime}" || name.starts_with("{runtime}/"));
        (scratch, roots, source, manifest)
    }

    #[test]
    fn runtime_only_capture_and_copy_have_no_compiler_dependency() {
        let (scratch, roots, source, manifest) = parser_fixture();
        let captured = CapturedOperator::capture(&manifest, &roots, &source).unwrap();
        assert_eq!(
            captured.fingerprint(),
            "340b6df668c74953ccf7eebabb4da58f046983c44a4ca6c7e2ebe0a5dde7071a"
        );
        assert!(captured.read("{compiler}/mypy/entry.py").is_err());
        let destination = fs::canonicalize(scratch.path())
            .unwrap()
            .join("private-parser");
        let copied = captured.materialize(&destination).unwrap();
        assert!(copied.compiler.is_none());
        assert!(!destination.join("compiler").exists());
        let second = CapturedOperator::capture(&manifest, &copied, &source).unwrap();
        assert_eq!(captured.fingerprint(), second.fingerprint());
        for name in manifest.files.keys() {
            assert_eq!(captured.read(name).unwrap(), second.read(name).unwrap());
            let metadata = fs::metadata(logical_path(name, &copied).unwrap()).unwrap();
            assert_eq!((metadata.mtime(), metadata.mtime_nsec()), (1, 0));
            assert_eq!(metadata.mode() & 0o222, 0);
        }
        captured.verify_unchanged().unwrap();
        fs::write(roots.runtime.join("lib/python3.14/encoding.py"), b"changed").unwrap();
        assert!(captured.verify_unchanged().is_err());
    }

    #[test]
    fn runtime_only_capture_refuses_undeclared_compiler_entries() {
        let (_scratch, roots, source, manifest) = parser_fixture();
        let mut hostile = manifest.clone();
        hostile
            .directories
            .insert("{compiler}".to_owned(), BTreeMap::new());
        assert!(CapturedOperator::capture(&hostile, &roots, &source).is_err());
        hostile = manifest;
        hostile
            .files
            .insert("{compiler}/shadow.py".to_owned(), sha256_hex(b"shadow"));
        assert!(CapturedOperator::capture(&hostile, &roots, &source).is_err());
    }

    #[test]
    fn native_root_serialization_preserves_the_original_shape() {
        let (_scratch, roots, _source, _manifest) = fixture();
        let original = serde_json::json!({"runtime": roots.runtime, "compiler": roots.compiler.as_ref().unwrap()});
        assert_eq!(serde_json::to_value(&roots).unwrap(), original);
        let parsed: OperatorRoots = serde_json::from_value(original).unwrap();
        assert_eq!(parsed.compiler, roots.compiler);
        let parser: OperatorRoots =
            serde_json::from_value(serde_json::json!({"runtime": roots.runtime})).unwrap();
        assert!(parser.compiler.is_none());
    }

    #[test]
    fn runtime_only_guard_excludes_compiler_and_native_profile_refuses_it() {
        let (scratch, roots, source, manifest) = parser_fixture();
        let base = fs::canonicalize(scratch.path()).unwrap();
        let runtime_manifest = runtime_fixture(&base, &roots, &manifest);
        let captured_runtime = CapturedRuntime::capture_python(&runtime_manifest).unwrap();
        let captured = CapturedOperator::capture(&manifest, &roots, &source).unwrap();
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        let policy = captured.guard_policy(&state, &captured_runtime).unwrap();
        assert!(!policy.contains(&serde_json::to_string(&base.join("compiler")).unwrap()));
        assert!(
            captured
                .guard_policy(&roots.runtime, &captured_runtime)
                .is_err()
        );
        assert!(captured.guard_policy(&source, &captured_runtime).is_err());
        let error = crate::python_profile::NativeProfile::for_query(
            &captured,
            &captured_runtime,
            &serde_json::json!({"source_roots":["."], "namespace_packages":true, "external_stub_roots":[]}),
            &BTreeMap::new(),
        ).unwrap_err();
        assert_eq!(
            error.to_string(),
            "native profile requires a captured compiler root"
        );
    }

    fn runtime_fixture(
        base: &Path,
        roots: &OperatorRoots,
        manifest: &OperatorManifest,
    ) -> RuntimeManifest {
        let cache = base.join("cache");
        fs::write(&cache, b"owned cache").unwrap();
        RuntimeManifest {
            files: BTreeMap::from([
                (
                    "python".to_owned(),
                    RuntimeFile {
                        path: roots.runtime.join("bin/python3.14"),
                        sha256: manifest.files["{runtime}/bin/python3.14"].clone(),
                        role: RuntimeRole::Python,
                    },
                ),
                (
                    "cache".to_owned(),
                    RuntimeFile {
                        path: cache,
                        sha256: sha256_hex(b"owned cache"),
                        role: RuntimeRole::SharedCache,
                    },
                ),
            ]),
            cache_family: CacheFamily {
                directory: base.to_path_buf(),
                prefix: "cache".to_owned(),
            },
        }
    }

    #[test]
    fn guard_requires_disjoint_state_matching_interpreter_and_unchanged_inputs() {
        let (scratch, roots, source, manifest) = fixture();
        let base = fs::canonicalize(scratch.path()).unwrap();
        let cache = base.join("cache");
        let runtime_manifest = runtime_fixture(&base, &roots, &manifest);
        let captured = CapturedOperator::capture(&manifest, &roots, &source).unwrap();
        let captured_runtime = CapturedRuntime::capture_python(&runtime_manifest).unwrap();
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        captured.guard_policy(&state, &captured_runtime).unwrap();
        for invalid in [
            &base,
            &source,
            &roots.runtime,
            roots.compiler.as_ref().unwrap(),
            &base.join("absent"),
        ] {
            assert!(captured.guard_policy(invalid, &captured_runtime).is_err());
        }
        let alias = base.join("state-alias");
        symlink(&state, &alias).unwrap();
        assert!(captured.guard_policy(&alias, &captured_runtime).is_err());
        let mutable_library = state.join("library");
        fs::write(&mutable_library, b"owned library").unwrap();
        let mut overlapping = runtime_manifest.clone();
        overlapping.files.insert(
            "library".to_owned(),
            RuntimeFile {
                path: mutable_library,
                sha256: sha256_hex(b"owned library"),
                role: RuntimeRole::Library,
            },
        );
        let writable_runtime = CapturedRuntime::capture_python(&overlapping).unwrap();
        assert!(captured.guard_policy(&state, &writable_runtime).is_err());
        let mut foreign = runtime_manifest.clone();
        foreign.files.get_mut("python").unwrap().role = RuntimeRole::Node;
        let node = CapturedRuntime::capture_node(&foreign).unwrap();
        assert!(captured.guard_policy(&state, &node).is_err());
        let other = base.join("other-python");
        fs::write(&other, b"owned interpreter").unwrap();
        foreign = runtime_manifest;
        foreign.files.get_mut("python").unwrap().path = other;
        let wrong_interpreter = CapturedRuntime::capture_python(&foreign).unwrap();
        assert!(captured.guard_policy(&state, &wrong_interpreter).is_err());
        fs::write(&cache, b"changed cache").unwrap();
        assert!(captured.guard_policy(&state, &captured_runtime).is_err());
        fs::write(&cache, b"owned cache").unwrap();
        fs::write(
            roots.compiler.as_ref().unwrap().join("mypy/entry.py"),
            b"changed source",
        )
        .unwrap();
        assert!(captured.guard_policy(&state, &captured_runtime).is_err());
    }

    #[test]
    fn private_copies_preserve_bytes_and_normalize_import_mtimes() {
        let (scratch, roots, source, manifest) = fixture();
        let captured = CapturedOperator::capture(&manifest, &roots, &source).unwrap();
        let destination = fs::canonicalize(scratch.path())
            .unwrap()
            .join("private-operator");
        let copied = captured.materialize(&destination).unwrap();
        for name in manifest.files.keys() {
            let path = logical_path(name, &copied).unwrap();
            assert_eq!(fs::read(&path).unwrap(), captured.read(name).unwrap());
            let metadata = fs::metadata(&path).unwrap();
            assert_eq!((metadata.mtime(), metadata.mtime_nsec()), (1, 0));
            assert_eq!(metadata.mode() & 0o222, 0);
            let original = fs::metadata(logical_path(name, &roots).unwrap()).unwrap();
            assert_ne!(
                (metadata.dev(), metadata.ino()),
                (original.dev(), original.ino())
            );
        }
        for name in manifest.directories.keys() {
            let metadata = fs::metadata(logical_path(name, &copied).unwrap()).unwrap();
            assert_eq!((metadata.mtime(), metadata.mtime_nsec()), (1, 0));
        }
        assert_eq!(
            CapturedOperator::capture(&manifest, &copied, &source)
                .unwrap()
                .fingerprint(),
            captured.fingerprint()
        );
        assert!(captured.materialize(&destination).is_err());
        captured.verify_unchanged().unwrap();
        scratch.close().unwrap();
    }

    #[test]
    fn copy_refuses_original_roots_aliases_and_input_drift() {
        let (scratch, roots, source, manifest) = fixture();
        let captured = CapturedOperator::capture(&manifest, &roots, &source).unwrap();
        for destination in [
            roots.runtime.join("copy"),
            roots.compiler.as_ref().unwrap().join("copy"),
            source.join("copy"),
        ] {
            assert!(captured.materialize(&destination).is_err());
            assert!(!destination.exists());
        }
        let base = fs::canonicalize(scratch.path()).unwrap();
        let alias = base.join("alias");
        symlink(&base, &alias).unwrap();
        assert!(captured.materialize(&alias.join("copy")).is_err());
        assert!(!base.join("copy").exists());
        fs::write(roots.runtime.join("bin/python3.14"), b"changed").unwrap();
        assert!(captured.materialize(&base.join("copy")).is_err());
        assert!(!base.join("copy").exists());
    }

    #[test]
    fn runtime_or_compiler_content_drift_cannot_change_captured_bytes() {
        for name in [
            "{runtime}/lib/python3.14/__pycache__/encoding.cpython-314.pyc",
            "{compiler}/mypy/entry.py",
        ] {
            let (_scratch, roots, source, manifest) = fixture();
            let captured = CapturedOperator::capture(&manifest, &roots, &source).unwrap();
            let original = captured.read(name).unwrap().to_vec();
            fs::write(logical_path(name, &roots).unwrap(), b"changed").unwrap();
            assert!(captured.verify_unchanged().is_err());
            assert_eq!(captured.read(name).unwrap(), original);
            assert!(CapturedOperator::capture(&manifest, &roots, &source).is_err());
        }
    }

    #[test]
    fn content_and_negative_lookup_inventories_change_identity() {
        let (_scratch, roots, source, mut manifest) = fixture();
        let original = CapturedOperator::capture(&manifest, &roots, &source).unwrap();
        let name = "{runtime}/lib/python3.14/__pycache__/encoding.cpython-314.pyc";
        fs::write(logical_path(name, &roots).unwrap(), [0, 255, 254, 2]).unwrap();
        manifest
            .files
            .insert(name.to_owned(), sha256_hex(&[0, 255, 254, 2]));
        let changed = CapturedOperator::capture(&manifest, &roots, &source).unwrap();
        assert_ne!(original.fingerprint(), changed.fingerprint());
        let name = "{runtime}/lib/python3.14/empty";
        fs::create_dir(logical_path(name, &roots).unwrap()).unwrap();
        manifest
            .directories
            .get_mut("{runtime}/lib/python3.14")
            .unwrap()
            .insert("empty".to_owned(), EntryKind::Directory);
        manifest
            .directories
            .insert(name.to_owned(), BTreeMap::new());
        let inventoried = CapturedOperator::capture(&manifest, &roots, &source).unwrap();
        assert_ne!(changed.fingerprint(), inventoried.fingerprint());
    }

    #[test]
    fn added_entries_and_missing_inventory_refuse() {
        for name in [
            "{runtime}/lib/python314.zip",
            "{runtime}/lib/python3.14/site-packages",
            "{compiler}/shadow.py",
        ] {
            let (_scratch, roots, source, manifest) = fixture();
            let captured = CapturedOperator::capture(&manifest, &roots, &source).unwrap();
            fs::write(logical_path(name, &roots).unwrap(), b"ambient").unwrap();
            assert!(captured.verify_unchanged().is_err());
            assert!(CapturedOperator::capture(&manifest, &roots, &source).is_err());
        }
        let (_scratch, roots, source, manifest) = fixture();
        let mut incomplete = manifest.clone();
        incomplete.files.remove("{compiler}/mypy/entry.py");
        assert!(CapturedOperator::capture(&incomplete, &roots, &source).is_err());
        let mut incomplete = manifest.clone();
        incomplete
            .directories
            .remove("{runtime}/lib/python3.14/__pycache__");
        assert!(CapturedOperator::capture(&incomplete, &roots, &source).is_err());
        let mut malformed = manifest;
        malformed
            .files
            .insert("{runtime}/../escape".to_owned(), "0".repeat(64));
        assert!(CapturedOperator::capture(&malformed, &roots, &source).is_err());
    }

    #[test]
    fn aliases_special_files_and_overlapping_roots_refuse() {
        let (_scratch, roots, source, manifest) = fixture();
        let name = "{compiler}/mypy/entry.py";
        let file = logical_path(name, &roots).unwrap();
        fs::remove_file(&file).unwrap();
        symlink(roots.runtime.join("bin/python3.14"), &file).unwrap();
        assert!(CapturedOperator::capture(&manifest, &roots, &source).is_err());
        fs::remove_file(&file).unwrap();
        let fifo = std::ffi::CString::new(file.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(CapturedOperator::capture(&manifest, &roots, &source).is_err());
        let (_scratch, mut roots, source, manifest) = fixture();
        roots.compiler = Some(roots.runtime.clone());
        assert!(CapturedOperator::capture(&manifest, &roots, &source).is_err());
        let (_scratch, roots, _source, manifest) = fixture();
        assert!(CapturedOperator::capture(&manifest, &roots, &roots.runtime).is_err());
        assert!(
            CapturedOperator::capture(&manifest, &roots, roots.runtime.parent().unwrap()).is_err()
        );
    }

    #[test]
    fn bounded_reads_and_declared_ambient_layout_refuse() {
        let (_scratch, roots, source, mut manifest) = fixture();
        let file = roots.runtime.join("bin/python3.14");
        fs::File::create(&file)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(CapturedOperator::capture(&manifest, &roots, &source).is_err());
        let (_scratch, roots, source, valid) = fixture();
        manifest = valid;
        let name = "{runtime}/lib/python314.zip";
        manifest
            .files
            .insert(name.to_owned(), sha256_hex(b"ambient"));
        manifest
            .directories
            .get_mut("{runtime}/lib")
            .unwrap()
            .insert("python314.zip".to_owned(), EntryKind::File);
        fs::write(logical_path(name, &roots).unwrap(), b"ambient").unwrap();
        assert!(CapturedOperator::capture(&manifest, &roots, &source).is_err());
    }
}
