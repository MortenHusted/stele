//! Explicit operator runtime pins. This verifies bytes and cache-family
//! membership; the launcher must separately attest its loaded images.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::schema::sha256_hex;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRole {
    Clangd,
    Node,
    Python,
    Library,
    SharedCache,
    Inspector,
    Sandbox,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeFile {
    pub path: PathBuf,
    pub sha256: String,
    pub role: RuntimeRole,
}

/// A family is the entire set of directory entries starting with `prefix`.
/// Each entry must also have its own `SharedCache` file pin.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheFamily {
    pub directory: PathBuf,
    pub prefix: String,
}

/// Keys are stable logical filenames, independent of installation paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeManifest {
    pub files: BTreeMap<String, RuntimeFile>,
    pub cache_family: CacheFamily,
}

pub struct CapturedRuntime {
    manifest: RuntimeManifest,
    fingerprint: String,
}

impl CapturedRuntime {
    pub(crate) const fn manifest(&self) -> &RuntimeManifest {
        &self.manifest
    }
    pub fn capture(manifest: &RuntimeManifest) -> Result<Self> {
        Self::capture_executable(manifest, &RuntimeRole::Clangd, "cpp-runtime-pins/1")
    }

    pub fn capture_node(manifest: &RuntimeManifest) -> Result<Self> {
        Self::capture_executable(manifest, &RuntimeRole::Node, "js-ts-runtime-pins/1")
    }

    pub fn capture_python(manifest: &RuntimeManifest) -> Result<Self> {
        Self::capture_executable(manifest, &RuntimeRole::Python, "python-runtime-pins/1")
    }

    fn capture_executable(
        manifest: &RuntimeManifest,
        executable: &RuntimeRole,
        domain: &str,
    ) -> Result<Self> {
        let executables = manifest
            .files
            .values()
            .filter(|file| {
                matches!(
                    file.role,
                    RuntimeRole::Clangd | RuntimeRole::Node | RuntimeRole::Python
                )
            })
            .collect::<Vec<_>>();
        if executables.len() != 1 || &executables[0].role != executable {
            bail!("runtime must pin exactly one {executable:?} executable");
        }
        let mut paths = BTreeSet::new();
        let mut identity = BTreeMap::new();
        for (name, file) in &manifest.files {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
                || name
                    .split('/')
                    .any(|part| part.is_empty() || matches!(part, "." | ".."))
            {
                bail!("invalid runtime logical filename");
            }
            if file.sha256.len() != 64
                || !file
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                bail!("runtime pin must be a lowercase SHA-256");
            }
            if !file.path.is_absolute()
                || fs::canonicalize(&file.path)? != file.path
                || !paths.insert(file.path.clone())
            {
                bail!("runtime files must have unique canonical absolute paths");
            }
            identity.insert(name, (&file.role, &file.sha256));
        }
        let captured = Self {
            manifest: manifest.clone(),
            fingerprint: sha256_hex(&serde_json::to_vec(&(
                domain,
                &manifest.cache_family.prefix,
                &identity,
            ))?),
        };
        captured.verify_unchanged()?;
        Ok(captured)
    }

    /// File roles and logical names enter identity; physical paths do not.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn verify_unchanged(&self) -> Result<()> {
        self.verify_family()?;
        let mut stamps = Vec::new();
        for (name, file) in &self.manifest.files {
            if fs::canonicalize(&file.path)? != file.path {
                bail!("runtime pin path is no longer canonical: {name}");
            }
            let (digest, stamp) = file_digest(&file.path)?;
            if digest != file.sha256 {
                bail!("runtime digest mismatch: {name}");
            }
            stamps.push((&file.path, stamp));
        }
        for (path, stamp) in stamps {
            if file_stamp(&fs::symlink_metadata(path)?) != stamp {
                bail!("runtime input changed during verification");
            }
        }
        self.verify_family()
    }

    fn verify_family(&self) -> Result<()> {
        let family = &self.manifest.cache_family;
        if family.prefix.is_empty()
            || family.prefix.contains(['/', '\0'])
            || !family.directory.is_absolute()
            || fs::canonicalize(&family.directory)? != family.directory
        {
            bail!("runtime cache family requires a canonical directory and filename prefix");
        }
        let expected = self
            .manifest
            .files
            .values()
            .filter(|file| file.role == RuntimeRole::SharedCache)
            .map(|file| file.path.clone())
            .collect::<BTreeSet<_>>();
        let mut actual = BTreeSet::new();
        for entry in fs::read_dir(&family.directory)? {
            let entry = entry?;
            if entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(family.prefix.as_bytes())
            {
                actual.insert(entry.path());
            }
        }
        if expected.is_empty() || expected != actual {
            bail!("runtime shared-cache family membership differs from its pins");
        }
        Ok(())
    }
}

/// OS guard shared by the supported Darwin semantic launchers.
/// Content-read closure is a separate captured-input responsibility.
pub fn guard_policy(state: &Path, binary: &Path) -> Result<String> {
    Ok(format!(
        "(version 1)(allow default)(deny network*)(deny file-write*)(allow file-write* (subpath {}))(deny process-exec)(allow process-exec (literal {}))",
        serde_json::to_string(state)?,
        serde_json::to_string(binary)?
    ))
}

#[cfg(unix)]
fn file_digest(path: &Path) -> Result<(String, FileStamp)> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file() {
        bail!("runtime input must be a regular file");
    }
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if file_stamp(&before) != file_stamp(&file.metadata()?)
        || file_stamp(&before) != file_stamp(&fs::symlink_metadata(path)?)
    {
        bail!("runtime input changed while hashing");
    }
    Ok((format!("{:x}", hash.finalize()), file_stamp(&before)))
}

type FileStamp = (u64, u64, u64, i64, i64, i64, i64);

#[cfg(unix)]
fn file_stamp(metadata: &fs::Metadata) -> FileStamp {
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
fn file_stamp(_metadata: &fs::Metadata) -> FileStamp {
    (0, 0, 0, 0, 0, 0, 0)
}

#[cfg(not(unix))]
fn file_digest(_path: &Path) -> Result<(String, FileStamp)> {
    bail!("runtime capture requires a supported Unix host")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, RuntimeManifest) {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        let mut files = BTreeMap::new();
        for (name, role) in [
            ("clangd", RuntimeRole::Clangd),
            ("lib", RuntimeRole::Library),
            ("cache", RuntimeRole::SharedCache),
            ("cache.1", RuntimeRole::SharedCache),
        ] {
            let path = root.join(name);
            fs::write(&path, name).unwrap();
            files.insert(
                name.to_owned(),
                RuntimeFile {
                    path,
                    sha256: sha256_hex(name.as_bytes()),
                    role,
                },
            );
        }
        (
            temporary,
            RuntimeManifest {
                files,
                cache_family: CacheFamily {
                    directory: root,
                    prefix: "cache".to_owned(),
                },
            },
        )
    }

    #[test]
    fn cpp_identity_stays_pinned_and_node_requires_its_own_executable() {
        let (_first, manifest) = fixture();
        let cpp = CapturedRuntime::capture(&manifest).unwrap();
        assert_eq!(
            cpp.fingerprint(),
            "ca82b8d1793a1192ce4694aa294468304e98df1fc61327b2b35e862a960ac97c"
        );
        assert!(CapturedRuntime::capture_node(&manifest).is_err());
        let mut node = manifest;
        node.files.get_mut("clangd").unwrap().role = RuntimeRole::Node;
        let captured = CapturedRuntime::capture_node(&node).unwrap();
        assert_eq!(
            captured.fingerprint(),
            "3c0ec915b717d99683150f17156b5b2d3c45dbec72ba1c2e0be518b03fef6794"
        );
        assert_ne!(captured.fingerprint(), cpp.fingerprint());
        assert!(CapturedRuntime::capture(&node).is_err());
        let (_second, mut relocated) = fixture();
        relocated.files.get_mut("clangd").unwrap().role = RuntimeRole::Node;
        assert_eq!(
            captured.fingerprint(),
            CapturedRuntime::capture_node(&relocated)
                .unwrap()
                .fingerprint()
        );
        node.files.get_mut("lib").unwrap().role = RuntimeRole::Clangd;
        assert!(CapturedRuntime::capture_node(&node).is_err());
        assert!(CapturedRuntime::capture(&node).is_err());
        node.files.get_mut("lib").unwrap().role = RuntimeRole::Node;
        assert!(CapturedRuntime::capture_node(&node).is_err());
    }

    #[test]
    fn python_runtime_is_distinct_and_rejects_foreign_or_multiple_executables() {
        let (_first, mut manifest) = fixture();
        assert!(CapturedRuntime::capture_python(&manifest).is_err());
        manifest.files.get_mut("clangd").unwrap().role = RuntimeRole::Python;
        let python = CapturedRuntime::capture_python(&manifest).unwrap();
        assert!(CapturedRuntime::capture(&manifest).is_err());
        assert!(CapturedRuntime::capture_node(&manifest).is_err());
        let (_second, mut relocated) = fixture();
        relocated.files.get_mut("clangd").unwrap().role = RuntimeRole::Python;
        assert_eq!(
            python.fingerprint(),
            CapturedRuntime::capture_python(&relocated)
                .unwrap()
                .fingerprint()
        );
        for role in [RuntimeRole::Python, RuntimeRole::Node, RuntimeRole::Clangd] {
            manifest.files.get_mut("lib").unwrap().role = role;
            assert!(CapturedRuntime::capture_python(&manifest).is_err());
            assert!(CapturedRuntime::capture_node(&manifest).is_err());
            assert!(CapturedRuntime::capture(&manifest).is_err());
        }
    }

    #[test]
    fn relocation_preserves_identity_but_content_and_role_change_it() {
        let (_first, manifest) = fixture();
        let (_second, mut relocated) = fixture();
        let original = CapturedRuntime::capture(&manifest).unwrap();
        assert_eq!(
            original.fingerprint(),
            CapturedRuntime::capture(&relocated).unwrap().fingerprint()
        );
        let library = relocated.files.get_mut("lib").unwrap();
        fs::write(&library.path, "different bytes").unwrap();
        library.sha256 = sha256_hex(b"different bytes");
        let changed = CapturedRuntime::capture(&relocated).unwrap();
        assert_ne!(original.fingerprint(), changed.fingerprint());
        relocated.files.get_mut("lib").unwrap().role = RuntimeRole::Inspector;
        assert_ne!(
            changed.fingerprint(),
            CapturedRuntime::capture(&relocated).unwrap().fingerprint()
        );
    }

    #[test]
    fn rejects_digest_drift_and_cache_family_changes() {
        let (_root, manifest) = fixture();
        let captured = CapturedRuntime::capture(&manifest).unwrap();
        let extra = manifest.cache_family.directory.join("cache.extra");
        fs::write(&extra, "extra").unwrap();
        assert!(captured.verify_unchanged().is_err());
        fs::remove_file(extra).unwrap();
        fs::write(&manifest.files["lib"].path, "drift").unwrap();
        assert!(captured.verify_unchanged().is_err());
        assert!(CapturedRuntime::capture(&manifest).is_err());
        fs::remove_file(&manifest.files["cache.1"].path).unwrap();
        assert!(captured.verify_unchanged().is_err());
    }

    #[test]
    fn rejects_missing_clangd_invalid_pins_and_duplicate_paths() {
        let (_root, manifest) = fixture();
        let mut invalid = manifest.clone();
        invalid.files.remove("clangd");
        assert!(CapturedRuntime::capture(&invalid).is_err());
        invalid = manifest.clone();
        invalid.files.get_mut("lib").unwrap().sha256 = "00".to_owned();
        assert!(CapturedRuntime::capture(&invalid).is_err());
        invalid = manifest.clone();
        invalid
            .files
            .insert("second".to_owned(), manifest.files["clangd"].clone());
        assert!(CapturedRuntime::capture(&invalid).is_err());
        invalid = manifest.clone();
        invalid.cache_family.prefix = String::new();
        assert!(CapturedRuntime::capture(&invalid).is_err());
        invalid = manifest;
        invalid
            .files
            .insert("../escape".to_owned(), invalid.files["lib"].clone());
        assert!(CapturedRuntime::capture(&invalid).is_err());
    }

    #[test]
    fn rejects_symlink_and_nonregular_substitution_without_blocking() {
        use std::os::unix::fs::symlink;
        let (_root, manifest) = fixture();
        let captured = CapturedRuntime::capture(&manifest).unwrap();
        let library = &manifest.files["lib"].path;
        fs::remove_file(library).unwrap();
        symlink(&manifest.files["clangd"].path, library).unwrap();
        assert!(captured.verify_unchanged().is_err());
        fs::remove_file(library).unwrap();
        let path = std::ffi::CString::new(library.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: valid NUL-terminated pathname; creates an isolated fixture FIFO.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || sender.send(captured.verify_unchanged()).unwrap());
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("FIFO verification must not block")
                .is_err()
        );
        worker.join().unwrap();
        fs::remove_file(library).unwrap();
        fs::create_dir(library).unwrap();
        assert!(CapturedRuntime::capture(&manifest).is_err());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn ignores_unrelated_non_utf8_directory_entries() {
        use std::os::unix::ffi::OsStringExt;
        let (_root, manifest) = fixture();
        let path = manifest
            .cache_family
            .directory
            .join(std::ffi::OsString::from_vec(b"unrelated-\xff".to_vec()));
        fs::write(path, "unrelated").unwrap();
        CapturedRuntime::capture(&manifest)
            .unwrap()
            .verify_unchanged()
            .unwrap();
    }

    #[test]
    #[ignore = "requires an explicit operator runtime manifest"]
    fn pinned_runtime_capture_and_postcheck() {
        let path =
            std::env::var("CODEMAP_CPP_RUNTIME_MANIFEST").expect("explicit runtime manifest");
        let manifest: RuntimeManifest = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let captured = CapturedRuntime::capture(&manifest).expect("pinned runtime capture");
        captured
            .verify_unchanged()
            .expect("pinned runtime postcheck");
        assert_eq!(captured.fingerprint().len(), 64);
    }
}
