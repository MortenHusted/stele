//! Captured C++ parse inputs and their restricted LLVM filesystem.
//!
//! This is input preparation, not permission to launch semantic extraction.
//! The launcher must also constrain driver defaults, configuration and tools.

use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::cpp_profile::{ProfileRoots, logical_roots, map_path};
use crate::schema::sha256_hex;

/// Operator-declared logical filenames and expected SHA-256 digests.
///
/// Names use `{source}/src/unit.cpp`, `{build}/generated.hpp`, or another
/// declared root. A manifest never grants directory-wide filesystem access.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputManifest {
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct FileIdentity {
    sha256: String,
    /// First logical filename for the same physical file. Unlike an inode,
    /// this remains stable across equivalent checkouts.
    object: String,
}

struct CapturedFile {
    original: PathBuf,
    copy: PathBuf,
    identity: FileIdentity,
}

/// Owns private copies for one resolution run. Dropping it removes them.
pub struct CapturedInputs {
    workspace: tempfile::TempDir,
    roots: ProfileRoots,
    manifest: InputManifest,
    files: BTreeMap<String, CapturedFile>,
    overlay_sha256: String,
    fingerprint: String,
}

impl CapturedInputs {
    /// Verify every expected digest and build a redirect-only filesystem.
    /// Does not execute any command or write into a declared input root.
    pub fn capture(manifest: &InputManifest, roots: &ProfileRoots) -> Result<Self> {
        if manifest.files.is_empty() {
            bail!("C++ input manifest is empty");
        }
        let named = logical_roots(roots)?;
        let workspace = tempfile::Builder::new()
            .prefix("codemap-cpp-inputs-")
            .tempdir()?;
        let private = fs::canonicalize(workspace.path())?;
        if map_path(&private, &named).is_some() {
            bail!("C++ private workspace must be outside declared input roots");
        }
        let mut objects = BTreeMap::new();
        let mut files = BTreeMap::new();
        let mut mappings = Vec::new();
        for (name, expected) in &manifest.files {
            validate_digest(expected)?;
            let original = input_path(name, &named)?;
            let (bytes, physical) = read_input(&original, &named)?;
            if sha256_hex(&bytes) != *expected {
                bail!("C++ input digest mismatch: {name}");
            }
            let object = objects
                .entry(physical)
                .or_insert_with(|| name.clone())
                .clone();
            let copy = private.join(format!("object-{}", sha256_hex(object.as_bytes())));
            if !copy.exists() {
                fs::write(&copy, bytes)?;
                let mut permissions = fs::metadata(&copy)?.permissions();
                permissions.set_readonly(true);
                fs::set_permissions(&copy, permissions)?;
            }
            mappings.push(json!({"type": "file", "name": original, "external-contents": copy}));
            files.insert(
                name.clone(),
                CapturedFile {
                    original,
                    copy,
                    identity: FileIdentity {
                        sha256: expected.clone(),
                        object,
                    },
                },
            );
        }
        let overlay = serde_json::to_vec(&json!({
            "version": 0, "case-sensitive": true, "use-external-names": false,
            "redirecting-with": "redirect-only", "roots": mappings,
        }))?;
        fs::write(private.join("overlay.json"), &overlay)?;
        let identities = identities(&files);
        let fingerprint = input_fingerprint(&identities)?;
        let captured = Self {
            workspace,
            roots: roots.clone(),
            manifest: manifest.clone(),
            files,
            overlay_sha256: sha256_hex(&overlay),
            fingerprint,
        };
        captured.verify_unchanged()?;
        Ok(captured)
    }

    /// Identity covers content, logical lookup names and physical aliases.
    /// Private paths and platform inode numbers never enter this identity.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn overlay_path(&self) -> PathBuf {
        self.workspace.path().join("overlay.json")
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn captured_read_paths(&self) -> Result<Vec<PathBuf>> {
        self.verify_unchanged()?;
        let mut paths = self
            .files
            .values()
            .map(|file| file.copy.clone())
            .collect::<Vec<_>>();
        paths.push(self.overlay_path());
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    /// Read the captured bytes, rather than the possibly changed original.
    pub fn read(&self, logical: &str) -> Result<Vec<u8>> {
        self.read_copy(logical).map(|(bytes, _)| bytes)
    }

    fn read_copy(&self, logical: &str) -> Result<(Vec<u8>, (u64, u64))> {
        let file = self.files.get(logical).context("unmanifested C++ input")?;
        let (bytes, physical) = read_input(&file.copy, &[("capture", self.workspace.path())])?;
        if sha256_hex(&bytes) != file.identity.sha256 {
            bail!("captured C++ input changed: {logical}");
        }
        Ok((bytes, physical))
    }

    /// Required before and after resolution. Input content, alias drift, or
    /// private-overlay corruption invalidates the entire run.
    pub fn verify_unchanged(&self) -> Result<()> {
        let named = logical_roots(&self.roots)?;
        let mut objects = BTreeMap::new();
        let mut current = BTreeMap::new();
        let mut copy_objects = BTreeMap::new();
        let mut copies = BTreeMap::new();
        for (name, file) in &self.files {
            let original = input_path(name, &named)?;
            if original != file.original {
                bail!("C++ input root changed: {name}");
            }
            let (bytes, physical) = read_input(&original, &named)?;
            let digest = sha256_hex(&bytes);
            if self.manifest.files[name] != digest {
                bail!("C++ input changed: {name}");
            }
            let object = objects
                .entry(physical)
                .or_insert_with(|| name.clone())
                .clone();
            current.insert(
                name.clone(),
                FileIdentity {
                    sha256: digest,
                    object,
                },
            );
            let (_, copy_physical) = self.read_copy(name)?;
            let copy_object = copy_objects
                .entry(copy_physical)
                .or_insert_with(|| name.clone())
                .clone();
            copies.insert(
                name.clone(),
                FileIdentity {
                    sha256: file.identity.sha256.clone(),
                    object: copy_object,
                },
            );
        }
        if input_fingerprint(&copies)? != self.fingerprint {
            bail!("captured C++ input aliases changed");
        }
        if input_fingerprint(&current)? != self.fingerprint {
            bail!("C++ input aliases changed");
        }
        let (overlay, _) = read_input(&self.overlay_path(), &[("capture", self.workspace.path())])?;
        if sha256_hex(&overlay) != self.overlay_sha256 {
            bail!("C++ input overlay changed");
        }
        Ok(())
    }
}

fn identities(files: &BTreeMap<String, CapturedFile>) -> BTreeMap<&str, &FileIdentity> {
    files
        .iter()
        .map(|(name, file)| (name.as_str(), &file.identity))
        .collect()
}

fn input_fingerprint<T: Serialize>(files: &T) -> Result<String> {
    Ok(sha256_hex(&serde_json::to_vec(
        &json!({"policy": "cpp-inputs/1", "files": files}),
    )?))
}

fn validate_digest(digest: &str) -> Result<()> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("C++ input digest must be a lowercase SHA-256");
    }
    Ok(())
}

fn input_path(name: &str, roots: &[(&str, &Path)]) -> Result<PathBuf> {
    let (root, relative) = name
        .strip_prefix('{')
        .and_then(|rest| rest.split_once("}/"))
        .context("C++ input must name a logical root and relative file")?;
    let path = Path::new(relative);
    if relative.is_empty()
        || relative.contains('\0')
        || relative
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        bail!("C++ input has an invalid relative path: {name}");
    }
    let (_, directory) = roots
        .iter()
        .find(|(candidate, _)| *candidate == root)
        .context("C++ input names an undeclared root")?;
    Ok(fs::canonicalize(directory)?.join(path))
}

/// Device/inode is only a capture-time alias key, never canonical output.
#[cfg(unix)]
fn read_input(path: &Path, roots: &[(&str, &Path)]) -> Result<(Vec<u8>, (u64, u64))> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let target =
        fs::canonicalize(path).with_context(|| format!("missing C++ input: {}", path.display()))?;
    if map_path(&target, roots).is_none() {
        bail!("C++ input escapes declared roots: {}", path.display());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(&target)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("C++ input must be a regular file: {}", path.display());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok((bytes, (metadata.dev(), metadata.ino())))
}

#[cfg(not(unix))]
fn read_input(_path: &Path, _roots: &[(&str, &Path)]) -> Result<(Vec<u8>, (u64, u64))> {
    bail!("C++ semantic inputs require a supported Unix host")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn roots(parent: &Path) -> ProfileRoots {
        for name in ["source", "build", "sdk", "dependency"] {
            fs::create_dir(parent.join(name)).expect("root");
        }
        ProfileRoots {
            source: parent.join("source"),
            build: Some(parent.join("build")),
            sysroot: Some(parent.join("sdk")),
            dependencies: vec![("dep".to_string(), parent.join("dependency"))],
        }
    }

    fn manifest(entries: &[(&str, &str)]) -> InputManifest {
        InputManifest {
            files: entries
                .iter()
                .map(|(name, text)| ((*name).to_string(), sha256_hex(text.as_bytes())))
                .collect(),
        }
    }

    fn write_inputs(parent: &Path) -> InputManifest {
        let entries = [
            (
                "{source}/unit.cpp",
                "#include <config.hpp>\nint run() { return VALUE; }\n",
            ),
            ("{build}/config.hpp", "#define VALUE 1\n"),
            ("{sysroot}/system.hpp", "#define SYSTEM 1\n"),
            ("{dep}/library.hpp", "int library();\n"),
        ];
        for (directory, (_, text)) in [
            "source/unit.cpp",
            "build/config.hpp",
            "sdk/system.hpp",
            "dependency/library.hpp",
        ]
        .into_iter()
        .zip(entries)
        {
            fs::write(parent.join(directory), text).expect("input");
        }
        manifest(&entries)
    }

    #[test]
    fn relocation_preserves_input_identity_and_private_copies_leave_originals_unchanged() {
        let first = tempfile::tempdir().expect("first");
        let second = tempfile::tempdir().expect("second");
        let first_roots = roots(first.path());
        let second_roots = roots(second.path());
        let expected = write_inputs(first.path());
        write_inputs(second.path());
        let a = CapturedInputs::capture(&expected, &first_roots).expect("first capture");
        let b = CapturedInputs::capture(&expected, &second_roots).expect("second capture");
        assert_eq!(a.fingerprint(), b.fingerprint());
        for (name, file) in &a.files {
            assert_eq!(
                a.read(name).expect("captured"),
                fs::read(&file.original).expect("original")
            );
            assert_ne!(file.copy, file.original);
        }
        a.verify_unchanged().expect("originals unchanged");
        let private = a.workspace.path().to_path_buf();
        drop(a);
        assert!(
            !private.exists(),
            "capture must clean its private workspace"
        );
        assert!(first.path().join("source/unit.cpp").exists());
    }

    #[test]
    fn generated_sysroot_and_dependency_changes_invalidate_the_run_and_change_identity() {
        for (path, logical) in [
            ("build/config.hpp", "{build}/config.hpp"),
            ("sdk/system.hpp", "{sysroot}/system.hpp"),
            ("dependency/library.hpp", "{dep}/library.hpp"),
        ] {
            let parent = tempfile::tempdir().expect("parent");
            let roots = roots(parent.path());
            let mut expected = write_inputs(parent.path());
            let before = CapturedInputs::capture(&expected, &roots).expect("before");
            fs::write(parent.path().join(path), "// changed\n").expect("change");
            assert!(before.verify_unchanged().is_err(), "{logical}");
            assert_ne!(
                before.read(logical).expect("captured bytes"),
                b"// changed\n"
            );
            assert!(
                CapturedInputs::capture(&expected, &roots).is_err(),
                "stale manifest"
            );
            expected
                .files
                .insert(logical.to_string(), sha256_hex(b"// changed\n"));
            let after = CapturedInputs::capture(&expected, &roots).expect("updated manifest");
            assert_ne!(before.fingerprint(), after.fingerprint(), "{logical}");
        }
    }

    #[test]
    fn aliases_are_shared_but_identical_distinct_files_keep_their_identity() {
        use std::os::unix::fs::symlink;
        let parent = tempfile::tempdir().expect("parent");
        let roots = roots(parent.path());
        fs::write(roots.source.join("one.hpp"), "#pragma once\n").expect("one");
        fs::hard_link(roots.source.join("one.hpp"), roots.source.join("hard.hpp"))
            .expect("hard link");
        symlink("one.hpp", roots.source.join("soft.hpp")).expect("symbolic link");
        fs::write(roots.source.join("distinct.hpp"), "#pragma once\n").expect("distinct");
        let expected = manifest(&[
            ("{source}/one.hpp", "#pragma once\n"),
            ("{source}/hard.hpp", "#pragma once\n"),
            ("{source}/soft.hpp", "#pragma once\n"),
            ("{source}/distinct.hpp", "#pragma once\n"),
        ]);
        let captured = CapturedInputs::capture(&expected, &roots).expect("capture");
        assert_eq!(
            captured.files["{source}/one.hpp"].copy,
            captured.files["{source}/hard.hpp"].copy
        );
        assert_eq!(
            captured.files["{source}/one.hpp"].copy,
            captured.files["{source}/soft.hpp"].copy
        );
        assert_ne!(
            captured.files["{source}/one.hpp"].copy,
            captured.files["{source}/distinct.hpp"].copy
        );
        fs::remove_file(roots.source.join("soft.hpp")).expect("unlink alias");
        symlink("distinct.hpp", roots.source.join("soft.hpp")).expect("retarget alias");
        assert!(
            captured
                .verify_unchanged()
                .expect_err("alias drift")
                .to_string()
                .contains("aliases changed")
        );
        let changed = CapturedInputs::capture(&expected, &roots).expect("new aliases");
        assert_ne!(captured.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn invalid_manifest_paths_missing_files_and_root_escapes_fail_closed() {
        use std::os::unix::fs::symlink;
        let parent = tempfile::tempdir().expect("parent");
        let roots = roots(parent.path());
        fs::write(parent.path().join("outside.hpp"), "outside").expect("outside");
        symlink(
            parent.path().join("outside.hpp"),
            roots.source.join("escape.hpp"),
        )
        .expect("escape");
        for name in [
            "{source}/../outside.hpp",
            "{source}/./unit.cpp",
            "{source}//unit.cpp",
            "{source}//etc/passwd",
            "{unknown}/unit.cpp",
            "{source}/missing.cpp",
            "{source}/escape.hpp",
        ] {
            assert!(
                CapturedInputs::capture(&manifest(&[(name, "outside")]), &roots).is_err(),
                "{name}"
            );
        }
        assert!(CapturedInputs::capture(&manifest(&[]), &roots).is_err());
        assert!(
            CapturedInputs::capture(
                &InputManifest {
                    files: BTreeMap::from([(
                        "{source}/unit.cpp".to_string(),
                        "not-a-digest".to_string()
                    )])
                },
                &roots
            )
            .is_err()
        );
        fs::create_dir(roots.source.join("directory.hpp")).expect("directory");
        assert!(
            CapturedInputs::capture(&manifest(&[("{source}/directory.hpp", "")]), &roots).is_err()
        );
    }

    #[test]
    fn captured_copy_and_overlay_corruption_abort_verification() {
        let parent = tempfile::tempdir().expect("parent");
        let roots = roots(parent.path());
        let expected = write_inputs(parent.path());
        let captured = CapturedInputs::capture(&expected, &roots).expect("capture");
        fs::write(captured.overlay_path(), "{}").expect("corrupt overlay");
        assert!(
            captured
                .verify_unchanged()
                .expect_err("overlay corruption")
                .to_string()
                .contains("overlay changed")
        );
        let copy = &captured.files["{source}/unit.cpp"].copy;
        fs::remove_file(copy).expect("remove private copy");
        fs::write(copy, "changed").expect("replace private copy");
        assert!(captured.read("{source}/unit.cpp").is_err());
        assert!(captured.verify_unchanged().is_err());
        assert!(captured.read("{source}/unmanifested.hpp").is_err());
    }

    #[test]
    fn private_alias_merging_and_copy_symlink_escapes_are_refused() {
        use std::os::unix::fs::symlink;
        let parent = tempfile::tempdir().expect("parent");
        let roots = roots(parent.path());
        for name in ["one.hpp", "two.hpp"] {
            fs::write(roots.source.join(name), "#pragma once\n").expect("distinct header");
        }
        let expected = manifest(&[
            ("{source}/one.hpp", "#pragma once\n"),
            ("{source}/two.hpp", "#pragma once\n"),
        ]);
        let captured = CapturedInputs::capture(&expected, &roots).expect("capture");
        let one = &captured.files["{source}/one.hpp"].copy;
        let two = &captured.files["{source}/two.hpp"].copy;
        fs::remove_file(two).expect("remove distinct copy");
        fs::hard_link(one, two).expect("merge private identities");
        assert!(
            captured
                .verify_unchanged()
                .expect_err("private alias drift")
                .to_string()
                .contains("captured C++ input aliases changed")
        );
        fs::remove_file(two).expect("unlink copy");
        symlink(roots.source.join("two.hpp"), two).expect("escape private workspace");
        assert!(captured.read("{source}/two.hpp").is_err());
        assert!(captured.verify_unchanged().is_err());
    }

    #[test]
    fn overlay_fifo_is_rejected_without_waiting_for_a_writer() {
        use std::sync::mpsc;
        use std::time::Duration;
        let parent = tempfile::tempdir().expect("parent");
        let roots = roots(parent.path());
        let expected = write_inputs(parent.path());
        let captured = CapturedInputs::capture(&expected, &roots).expect("capture");
        let overlay = captured.overlay_path();
        fs::remove_file(&overlay).expect("remove overlay");
        let mut setup = std::process::Command::new("mkfifo");
        setup.arg(&overlay);
        assert!(
            crate::lsp::bounded_probe(setup, Duration::from_secs(5))
                .expect("create Unix FIFO")
                .status
                .success()
        );
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender
                .send(captured.verify_unchanged())
                .expect("send verification");
        });
        let error = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("verification must not wait for a FIFO writer")
            .expect_err("nonregular overlay");
        assert!(error.to_string().contains("regular file"), "{error:#}");
        worker.join().expect("verification worker");
    }

    #[test]
    fn overlay_cannot_redirect_verification_outside_its_private_workspace() {
        use std::os::unix::fs::symlink;
        let parent = tempfile::tempdir().expect("parent");
        let roots = roots(parent.path());
        let expected = write_inputs(parent.path());
        let captured = CapturedInputs::capture(&expected, &roots).expect("capture");
        let overlay = captured.overlay_path();
        let outside = parent.path().join("outside-overlay.json");
        fs::copy(&overlay, &outside).expect("same bytes outside capture");
        fs::remove_file(&overlay).expect("remove overlay");
        symlink(&outside, &overlay).expect("overlay escape");
        assert!(
            captured
                .verify_unchanged()
                .expect_err("overlay root escape")
                .to_string()
                .contains("escapes declared roots")
        );
    }
}
