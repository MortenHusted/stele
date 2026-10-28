//! Prepared compiler data and directory inventories; no target execution.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::schema::sha256_hex;

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 1_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputRoots {
    pub source: PathBuf,
    pub typescript: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryEntry {
    pub kind: EntryKind,
    /// A symlink's canonical target, expressed in a declared logical root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// Expected content plus complete listings for consulted directories.
/// A listed file is metadata only until its content is separately pinned.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputManifest {
    pub files: BTreeMap<String, String>,
    pub directories: BTreeMap<String, BTreeMap<String, DirectoryEntry>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct FileIdentity {
    sha256: String,
    object: String,
}

struct CapturedFile {
    text: String,
    identity: FileIdentity,
}

pub struct CapturedInputs {
    roots: InputRoots,
    manifest: InputManifest,
    files: BTreeMap<String, CapturedFile>,
    fingerprint: String,
}

impl CapturedInputs {
    pub fn capture(manifest: &InputManifest, roots: &InputRoots) -> Result<Self> {
        validate_roots(roots)?;
        ensure!(!manifest.files.is_empty(), "JS/TS input manifest is empty");
        let entries = manifest
            .directories
            .values()
            .map(BTreeMap::len)
            .sum::<usize>();
        ensure!(
            manifest.files.len() <= MAX_ENTRIES
                && manifest.directories.len() <= MAX_ENTRIES
                && entries <= MAX_ENTRIES,
            "JS/TS input inventory budget exceeded"
        );
        for root in ["{source}", "{typescript}"] {
            ensure!(
                manifest.directories.contains_key(root),
                "missing JS/TS root directory inventory"
            );
        }
        verify_directories(manifest, roots)?;
        let mut objects = BTreeMap::new();
        let mut files = BTreeMap::new();
        let mut total = 0;
        for (name, digest) in &manifest.files {
            ensure!(
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "JS/TS input digest must be a lowercase SHA-256"
            );
            let path = input_path(name, roots)?;
            ensure!(
                path != roots.source && path != roots.typescript,
                "JS/TS file must have a relative name"
            );
            let (parent, basename) = name
                .rsplit_once('/')
                .context("JS/TS file must have a relative name")?;
            let listing = manifest
                .directories
                .get(parent)
                .context("JS/TS file parent inventory is missing")?;
            ensure!(
                listing.get(basename)
                    == Some(&DirectoryEntry {
                        kind: EntryKind::File,
                        target: None
                    }),
                "captured JS/TS file must be regular in its directory inventory"
            );
            let FileRead {
                bytes, physical, ..
            } = read_file(&path)?;
            total += bytes.len();
            ensure!(
                total <= MAX_TOTAL_BYTES,
                "JS/TS captured input budget exceeded"
            );
            ensure!(
                sha256_hex(&bytes) == *digest,
                "JS/TS input digest mismatch: {name}"
            );
            let text = String::from_utf8(bytes).context("JS/TS input must be valid UTF-8")?;
            let object = objects
                .entry(physical)
                .or_insert_with(|| name.clone())
                .clone();
            files.insert(
                name.clone(),
                CapturedFile {
                    text,
                    identity: FileIdentity {
                        sha256: digest.clone(),
                        object,
                    },
                },
            );
        }
        let fingerprint = fingerprint(manifest, &files)?;
        let captured = Self {
            roots: roots.clone(),
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

    pub fn read(&self, logical: &str) -> Result<&str> {
        Ok(&self
            .files
            .get(logical)
            .context("unmanifested JS/TS input")?
            .text)
    }

    /// Logical paths and captured text only; no original filesystem path.
    pub fn helper_data(&self) -> serde_json::Value {
        let files = self
            .files
            .iter()
            .map(|(name, file)| (name, &file.text))
            .collect::<BTreeMap<_, _>>();
        serde_json::json!({"files":files,"directories":self.manifest.directories})
    }

    pub fn verify_unchanged(&self) -> Result<()> {
        self.verify_with_read(read_file)
    }

    fn verify_with_read(&self, mut read: impl FnMut(&Path) -> Result<FileRead>) -> Result<()> {
        validate_roots(&self.roots)?;
        verify_directories(&self.manifest, &self.roots)?;
        let mut objects = BTreeMap::new();
        let mut stamps = Vec::new();
        for (name, captured) in &self.files {
            let path = input_path(name, &self.roots)?;
            let observed = read(&path)?;
            let object = objects
                .entry(observed.physical)
                .or_insert_with(|| name.clone());
            ensure!(
                sha256_hex(&observed.bytes) == captured.identity.sha256
                    && *object == captured.identity.object,
                "JS/TS input content or alias changed: {name}"
            );
            stamps.push((path, observed.stamp));
        }
        verify_directories(&self.manifest, &self.roots)?;
        for (path, previous) in stamps {
            ensure!(
                stamp(&fs::symlink_metadata(&path)?) == previous
                    && fs::canonicalize(&path)? == path,
                "JS/TS input changed during full verification"
            );
        }
        Ok(())
    }
}

fn fingerprint(manifest: &InputManifest, files: &BTreeMap<String, CapturedFile>) -> Result<String> {
    let identities = files
        .iter()
        .map(|(name, file)| (name, &file.identity))
        .collect::<BTreeMap<_, _>>();
    Ok(sha256_hex(&serde_json::to_vec(&serde_json::json!({
        "policy":"js-ts-inputs/1", "files":identities, "directories":manifest.directories
    }))?))
}

fn validate_roots(roots: &InputRoots) -> Result<()> {
    for root in [&roots.source, &roots.typescript] {
        ensure!(
            root.is_absolute() && fs::canonicalize(root)? == *root && root.is_dir(),
            "JS/TS input roots must be canonical absolute directories"
        );
    }
    ensure!(
        !roots.source.starts_with(&roots.typescript)
            && !roots.typescript.starts_with(&roots.source),
        "JS/TS source and compiler roots must be disjoint"
    );
    Ok(())
}

fn input_path(logical: &str, roots: &InputRoots) -> Result<PathBuf> {
    for (name, root) in [
        ("{source}", &roots.source),
        ("{typescript}", &roots.typescript),
    ] {
        if logical == name {
            return Ok(root.clone());
        }
        if let Some(relative) = logical.strip_prefix(&format!("{name}/")) {
            ensure!(
                !(name == "{source}" && relative.split('/').next() == Some(".git")),
                "Git metadata is outside JS/TS compiler input policy"
            );
            ensure!(
                !relative.is_empty() && relative.split('/').all(valid_component),
                "invalid JS/TS logical input path"
            );
            return Ok(root.join(relative));
        }
    }
    bail!("JS/TS input names an undeclared root")
}

fn valid_component(name: &str) -> bool {
    !name.is_empty() && !matches!(name, "." | "..") && !name.contains(['/', '\\', '\0'])
}

fn logical_path(path: &Path, roots: &InputRoots) -> Result<String> {
    for (name, root) in [
        ("{source}", &roots.source),
        ("{typescript}", &roots.typescript),
    ] {
        if let Ok(relative) = path.strip_prefix(root) {
            if relative.as_os_str().is_empty() {
                return Ok(name.to_owned());
            }
            let relative = relative.to_str().context("JS/TS input path is not UTF-8")?;
            ensure!(
                relative.split('/').all(valid_component),
                "invalid JS/TS alias target"
            );
            ensure!(
                !(name == "{source}" && relative.split('/').next() == Some(".git")),
                "JS/TS alias targets excluded Git metadata"
            );
            return Ok(format!("{name}/{relative}"));
        }
    }
    bail!("JS/TS input alias escapes declared roots")
}

fn verify_directories(manifest: &InputManifest, roots: &InputRoots) -> Result<()> {
    let mut stamps = Vec::new();
    for (logical, expected) in &manifest.directories {
        let path = input_path(logical, roots)?;
        ensure!(
            fs::canonicalize(&path)? == path && path.is_dir(),
            "JS/TS directory must be canonical"
        );
        let before = fs::symlink_metadata(&path)?;
        let mut actual = BTreeMap::new();
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("JS/TS directory entry is not UTF-8"))?;
            if logical == "{source}" && name == ".git" {
                continue;
            }
            ensure!(valid_component(&name), "invalid JS/TS directory entry name");
            ensure!(
                actual.len() < MAX_ENTRIES,
                "JS/TS directory inventory budget exceeded"
            );
            let metadata = fs::symlink_metadata(entry.path())?;
            let (kind_metadata, target) = if metadata.file_type().is_symlink() {
                let physical = fs::canonicalize(entry.path())?;
                (
                    fs::metadata(&physical)?,
                    Some(logical_path(&physical, roots)?),
                )
            } else {
                (metadata, None)
            };
            let kind = if kind_metadata.is_file() {
                EntryKind::File
            } else if kind_metadata.is_dir() {
                EntryKind::Directory
            } else {
                bail!("JS/TS input inventory contains a nonregular entry");
            };
            actual.insert(name, DirectoryEntry { kind, target });
        }
        ensure!(
            actual == *expected,
            "JS/TS directory membership or alias differs: {logical}"
        );
        ensure!(
            stamp(&before) == stamp(&fs::symlink_metadata(&path)?)
                && fs::canonicalize(&path)? == path,
            "JS/TS directory changed while capturing"
        );
        stamps.push((path, stamp(&before)));
    }
    for (path, previous) in stamps {
        ensure!(
            stamp(&fs::symlink_metadata(&path)?) == previous && fs::canonicalize(&path)? == path,
            "JS/TS directory changed during full inventory verification"
        );
    }
    Ok(())
}

type FileStamp = (u64, u64, u64, i64, i64, i64, i64);

struct FileRead {
    bytes: Vec<u8>,
    physical: (u64, u64),
    stamp: FileStamp,
}

#[cfg(unix)]
fn read_file(path: &Path) -> Result<FileRead> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    ensure!(
        fs::canonicalize(path)? == path,
        "JS/TS captured file path must be canonical"
    );
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= MAX_FILE_BYTES,
        "JS/TS input must be a bounded regular file"
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_FILE_BYTES,
        "JS/TS input byte budget exceeded"
    );
    ensure!(
        stamp(&before) == stamp(&file.metadata()?)
            && stamp(&before) == stamp(&fs::symlink_metadata(path)?)
            && fs::canonicalize(path)? == path,
        "JS/TS input changed while reading"
    );
    Ok(FileRead {
        bytes,
        physical: (before.dev(), before.ino()),
        stamp: stamp(&before),
    })
}

#[cfg(not(unix))]
fn read_file(_path: &Path) -> Result<FileRead> {
    bail!("JS/TS captured semantic inputs require a supported Unix host")
}

#[cfg(unix)]
fn stamp(metadata: &fs::Metadata) -> FileStamp {
    use std::os::unix::fs::MetadataExt;
    (
        metadata.dev(),
        metadata.ino(),
        metadata.size(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[cfg(not(unix))]
fn stamp(_metadata: &fs::Metadata) -> FileStamp {
    (0, 0, 0, 0, 0, 0, 0)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fixture() -> (tempfile::TempDir, InputRoots, InputManifest) {
        let temporary = tempfile::tempdir().unwrap();
        let parent = fs::canonicalize(temporary.path()).unwrap();
        let roots = InputRoots {
            source: parent.join("source"),
            typescript: parent.join("typescript"),
        };
        fs::create_dir(&roots.source).unwrap();
        fs::create_dir(&roots.typescript).unwrap();
        let contents = [
            ("{source}/tsconfig.json", "{}"),
            ("{source}/leaf.ts", "export function leaf() { return 1; }"),
            ("{source}/package-lock.json", "unchanged lock"),
            ("{typescript}/lib.d.ts", "declare const library: number;"),
        ];
        let mut manifest = InputManifest {
            files: BTreeMap::new(),
            directories: BTreeMap::from([
                ("{source}".to_owned(), BTreeMap::new()),
                ("{typescript}".to_owned(), BTreeMap::new()),
            ]),
        };
        for (name, content) in contents {
            fs::write(input_path(name, &roots).unwrap(), content).unwrap();
            manifest
                .files
                .insert(name.to_owned(), sha256_hex(content.as_bytes()));
            let (root, file) = name.rsplit_once('/').unwrap();
            manifest.directories.get_mut(root).unwrap().insert(
                file.to_owned(),
                DirectoryEntry {
                    kind: EntryKind::File,
                    target: None,
                },
            );
        }
        (temporary, roots, manifest)
    }

    #[test]
    fn captures_immutable_text_and_relocation_preserves_full_helper_data() {
        let (_first, roots, manifest) = fixture();
        let captured = CapturedInputs::capture(&manifest, &roots).unwrap();
        let (_second, relocated_roots, relocated_manifest) = fixture();
        let relocated = CapturedInputs::capture(&relocated_manifest, &relocated_roots).unwrap();
        assert_eq!(captured.fingerprint(), relocated.fingerprint());
        assert_eq!(captured.helper_data(), relocated.helper_data());
        assert!(
            !captured
                .helper_data()
                .to_string()
                .contains(roots.source.to_str().unwrap())
        );
        fs::write(roots.source.join("leaf.ts"), "changed").unwrap();
        assert_eq!(
            captured.read("{source}/leaf.ts").unwrap(),
            "export function leaf() { return 1; }"
        );
        assert!(captured.verify_unchanged().is_err());
        assert!(CapturedInputs::capture(&manifest, &roots).is_err());
    }

    #[test]
    fn earlier_file_drift_during_later_reads_invalidates_the_whole_scan() {
        let (_temporary, roots, manifest) = fixture();
        let captured = CapturedInputs::capture(&manifest, &roots).unwrap();
        let directory_before = stamp(&fs::metadata(&roots.source).unwrap());
        let mut changed = false;
        let result = captured.verify_with_read(|path| {
            let observed = read_file(path)?;
            if path == roots.typescript.join("lib.d.ts") {
                fs::write(roots.source.join("leaf.ts"), "changed during a later read")?;
                changed = true;
                assert_eq!(
                    directory_before,
                    stamp(&fs::metadata(&roots.source).unwrap())
                );
            }
            Ok(observed)
        });
        assert!(changed);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("during full verification")
        );
    }

    #[test]
    fn dependency_content_changes_identity_even_when_lock_is_unchanged() {
        let (_temporary, roots, mut manifest) = fixture();
        let before = CapturedInputs::capture(&manifest, &roots).unwrap();
        let lock = fs::read(roots.source.join("package-lock.json")).unwrap();
        fs::write(
            roots.typescript.join("lib.d.ts"),
            "declare const library: string;",
        )
        .unwrap();
        assert!(before.verify_unchanged().is_err());
        assert!(CapturedInputs::capture(&manifest, &roots).is_err());
        manifest.files.insert(
            "{typescript}/lib.d.ts".into(),
            sha256_hex(b"declare const library: string;"),
        );
        let changed = CapturedInputs::capture(&manifest, &roots).unwrap();
        assert_ne!(before.fingerprint(), changed.fingerprint());
        assert_eq!(
            fs::read(roots.source.join("package-lock.json")).unwrap(),
            lock
        );
    }

    #[test]
    fn adding_an_uncaptured_resolution_candidate_invalidates_negative_lookups() {
        let (_temporary, roots, mut manifest) = fixture();
        let captured = CapturedInputs::capture(&manifest, &roots).unwrap();
        fs::write(
            roots.source.join("previously-missing.d.ts"),
            "declare const extra: number;",
        )
        .unwrap();
        assert!(captured.verify_unchanged().is_err());
        assert!(CapturedInputs::capture(&manifest, &roots).is_err());
        manifest.directories.get_mut("{source}").unwrap().insert(
            "previously-missing.d.ts".into(),
            DirectoryEntry {
                kind: EntryKind::File,
                target: None,
            },
        );
        let changed = CapturedInputs::capture(&manifest, &roots).unwrap();
        assert_ne!(captured.fingerprint(), changed.fingerprint());
        assert!(changed.read("{source}/previously-missing.d.ts").is_err());
    }

    #[test]
    fn canonical_alias_targets_and_hardlink_membership_are_verified() {
        let (_temporary, roots, mut manifest) = fixture();
        fs::hard_link(roots.source.join("leaf.ts"), roots.source.join("alias.ts")).unwrap();
        manifest.files.insert(
            "{source}/alias.ts".into(),
            manifest.files["{source}/leaf.ts"].clone(),
        );
        manifest.directories.get_mut("{source}").unwrap().insert(
            "alias.ts".into(),
            DirectoryEntry {
                kind: EntryKind::File,
                target: None,
            },
        );
        symlink(
            roots.typescript.join("lib.d.ts"),
            roots.source.join("types.d.ts"),
        )
        .unwrap();
        manifest.directories.get_mut("{source}").unwrap().insert(
            "types.d.ts".into(),
            DirectoryEntry {
                kind: EntryKind::File,
                target: Some("{typescript}/lib.d.ts".into()),
            },
        );
        let captured = CapturedInputs::capture(&manifest, &roots).unwrap();
        let bytes = fs::read(roots.source.join("leaf.ts")).unwrap();
        fs::remove_file(roots.source.join("leaf.ts")).unwrap();
        fs::write(roots.source.join("leaf.ts"), bytes).unwrap();
        assert!(captured.verify_unchanged().is_err());
        let independent = CapturedInputs::capture(&manifest, &roots).unwrap();
        assert_ne!(captured.fingerprint(), independent.fingerprint());
        fs::remove_file(roots.source.join("types.d.ts")).unwrap();
        symlink(
            roots.source.join("leaf.ts"),
            roots.source.join("types.d.ts"),
        )
        .unwrap();
        assert!(independent.verify_unchanged().is_err());
    }

    #[test]
    fn refuses_bad_paths_outside_links_nonregular_inputs_and_bad_text() {
        let (_temporary, roots, manifest) = fixture();
        for name in [
            "{source}/../escape",
            "{source}/./leaf.ts",
            "{source}/.git/HEAD",
            "{undeclared}/file",
            "{source}/bad\\name",
        ] {
            let mut invalid = manifest.clone();
            invalid.files.insert(name.into(), sha256_hex(b"x"));
            assert!(CapturedInputs::capture(&invalid, &roots).is_err());
        }
        fs::write(roots.source.join("leaf.ts"), [0xff]).unwrap();
        let mut invalid = manifest.clone();
        invalid
            .files
            .insert("{source}/leaf.ts".into(), sha256_hex(&[0xff]));
        assert!(CapturedInputs::capture(&invalid, &roots).is_err());
        fs::write(
            roots.source.join("leaf.ts"),
            vec![b' '; MAX_FILE_BYTES as usize + 1],
        )
        .unwrap();
        assert!(CapturedInputs::capture(&invalid, &roots).is_err());
        fs::remove_file(roots.source.join("leaf.ts")).unwrap();
        symlink("/usr/bin/false", roots.source.join("leaf.ts")).unwrap();
        assert!(CapturedInputs::capture(&manifest, &roots).is_err());
        fs::remove_file(roots.source.join("leaf.ts")).unwrap();
        let fifo =
            std::ffi::CString::new(roots.source.join("leaf.ts").as_os_str().as_encoded_bytes())
                .unwrap();
        // SAFETY: owned temporary path represented by a NUL-terminated string.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(CapturedInputs::capture(&manifest, &roots).is_err());
    }
    #[test]
    #[ignore = "requires an explicit prepared JS/TS project input manifest"]
    fn prepared_real_project_capture_and_postcheck() {
        let manifest: InputManifest = serde_json::from_slice(
            &fs::read(
                std::env::var("CODEMAP_JS_TS_INPUT_MANIFEST")
                    .expect("explicit prepared input manifest"),
            )
            .unwrap(),
        )
        .unwrap();
        let roots = InputRoots {
            source: fs::canonicalize(
                std::env::var("CODEMAP_JS_TS_SOURCE").expect("explicit source"),
            )
            .unwrap(),
            typescript: fs::canonicalize(
                std::env::var("CODEMAP_JS_TS_TYPESCRIPT_LIB").expect("explicit compiler library"),
            )
            .unwrap(),
        };
        let captured = CapturedInputs::capture(&manifest, &roots).unwrap();
        captured.verify_unchanged().unwrap();
        assert_eq!(captured.fingerprint().len(), 64);
        assert_eq!(
            captured.helper_data()["files"].as_object().unwrap().len(),
            manifest.files.len()
        );
    }
}
