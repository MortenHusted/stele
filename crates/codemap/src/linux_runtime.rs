//! Linux byte pins. This descriptor does not establish process ownership,
//! sandbox enforcement, or loaded-memory identity.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::PathBuf;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::schema::sha256_hex;

#[cfg(target_os = "linux")]
#[path = "linux_file_images.rs"]
mod live;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxArchitecture {
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxRuntimeRole {
    Clangd,
    Node,
    Python,
    Library,
    Loader,
    Data,
}

impl LinuxRuntimeRole {
    const fn is_executable(self) -> bool {
        matches!(self, Self::Clangd | Self::Node | Self::Python)
    }
}

/// An explicit input to later process admission, not permission to launch JITs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnonymousExecutePolicy {
    Deny,
    Allow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxRuntimeFile {
    pub path: PathBuf,
    pub sha256: String,
    pub role: LinuxRuntimeRole,
}

/// This separate schema leaves Darwin manifests and their identities intact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxRuntimeManifest {
    pub architecture: LinuxArchitecture,
    pub anonymous_execute: AnonymousExecutePolicy,
    pub files: BTreeMap<String, LinuxRuntimeFile>,
}

pub struct CapturedLinuxRuntime {
    pub(crate) manifest: LinuxRuntimeManifest,
    fingerprint: String,
    pub(crate) files: BTreeMap<String, PinnedFile>,
    interpreter: Option<(PathBuf, PathBuf)>,
}

pub(crate) struct PinnedFile {
    handle: File,
    pub(crate) stamp: FileStamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileStamp {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileStamp {
    pub(crate) fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

impl CapturedLinuxRuntime {
    pub fn capture(manifest: &LinuxRuntimeManifest) -> Result<Self> {
        if manifest
            .files
            .values()
            .filter(|file| file.role.is_executable())
            .count()
            != 1
        {
            bail!("Linux runtime must pin exactly one executable");
        }
        let mut paths = BTreeSet::new();
        let mut inodes = BTreeSet::new();
        let mut files = BTreeMap::new();
        let mut identity = BTreeMap::new();
        let mut interpreters = BTreeMap::new();
        for (name, pin) in &manifest.files {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
                || name
                    .split('/')
                    .any(|part| part.is_empty() || matches!(part, "." | ".."))
            {
                bail!("invalid Linux runtime logical filename");
            }
            if pin.sha256.len() != 64
                || !pin
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                bail!("Linux runtime pin must be a lowercase SHA-256");
            }
            if !pin.path.is_absolute()
                || fs::canonicalize(&pin.path)? != pin.path
                || !paths.insert(&pin.path)
            {
                bail!("Linux runtime files require unique canonical absolute paths");
            }
            let mut handle = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&pin.path)?;
            let metadata = handle.metadata()?;
            if !metadata.is_file() || !inodes.insert((metadata.dev(), metadata.ino())) {
                bail!("Linux runtime requires distinct regular files");
            }
            let stamp = FileStamp::of(&metadata);
            if digest(&handle, stamp.size)? != pin.sha256 {
                bail!("Linux runtime digest mismatch: {name}");
            }
            let interpreter = verify_elf(&mut handle, stamp.size, pin.role, manifest.architecture)?;
            interpreters.insert(name.clone(), interpreter);
            check_stamp(&handle, pin, stamp)?;
            files.insert(name.clone(), PinnedFile { handle, stamp });
            identity.insert(name, (pin.role, &pin.sha256));
        }
        let loaders = manifest
            .files
            .values()
            .filter(|pin| pin.role == LinuxRuntimeRole::Loader)
            .collect::<Vec<_>>();
        let executable = manifest
            .files
            .iter()
            .find(|(_, pin)| pin.role.is_executable())
            .expect("one executable checked");
        let interpreter = if let Some(path) = &interpreters[executable.0] {
            if loaders.len() != 1 || fs::canonicalize(path)? != loaders[0].path {
                bail!("Linux executable requires its exact pinned interpreter");
            }
            Some((path.clone(), loaders[0].path.clone()))
        } else {
            if !loaders.is_empty() {
                bail!("static Linux executable cannot pin a loader");
            }
            None
        };
        let captured = Self {
            manifest: manifest.clone(),
            fingerprint: sha256_hex(&serde_json::to_vec(&(
                "linux-runtime-pins/2",
                manifest.architecture,
                manifest.anonymous_execute,
                identity,
            ))?),
            files,
            interpreter,
        };
        captured.verify_unchanged()?;
        Ok(captured)
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Retained handles distinguish replacement from identical current bytes.
    pub fn verify_unchanged(&self) -> Result<()> {
        self.verify_interpreter()?;
        for (name, file) in &self.files {
            check_stamp(&file.handle, &self.manifest.files[name], file.stamp)?;
            if digest(&file.handle, file.stamp.size)? != self.manifest.files[name].sha256 {
                bail!("Linux runtime digest changed: {name}");
            }
            check_stamp(&file.handle, &self.manifest.files[name], file.stamp)?;
        }
        for (name, file) in &self.files {
            check_stamp(&file.handle, &self.manifest.files[name], file.stamp)?;
        }
        self.verify_interpreter()?;
        Ok(())
    }

    fn verify_interpreter(&self) -> Result<()> {
        if let Some((path, target)) = &self.interpreter {
            if fs::canonicalize(path)? != *target {
                bail!("Linux interpreter lookup changed after capture");
            }
        }
        Ok(())
    }
}

fn check_stamp(handle: &File, pin: &LinuxRuntimeFile, expected: FileStamp) -> Result<()> {
    if fs::canonicalize(&pin.path)? != pin.path
        || FileStamp::of(&handle.metadata()?) != expected
        || FileStamp::of(&fs::symlink_metadata(&pin.path)?) != expected
    {
        bail!("Linux runtime file changed or was replaced");
    }
    Ok(())
}

fn digest(handle: &File, size: u64) -> Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 65536];
    let mut offset = 0;
    while offset < size {
        let limit = usize::try_from((size - offset).min(buffer.len() as u64))?;
        let count = handle.read_at(&mut buffer[..limit], offset)?;
        if count == 0 {
            bail!("Linux runtime file truncated while hashing");
        }
        hash.update(&buffer[..count]);
        offset += count as u64;
    }
    Ok(format!("{:x}", hash.finalize()))
}

// Only ELF64 little-endian executable/loadable images are admitted. This checks
// file structure, not loader behavior or correspondence to executable memory.
fn verify_elf(
    handle: &mut File,
    size: u64,
    role: LinuxRuntimeRole,
    architecture: LinuxArchitecture,
) -> Result<Option<PathBuf>> {
    handle.seek(SeekFrom::Start(0))?;
    let mut magic = [0; 4];
    let count = handle.read(&mut magic)?;
    if role == LinuxRuntimeRole::Data {
        if count == 4 && magic == *b"\x7fELF" {
            bail!("ELF image cannot be pinned as data");
        }
        return Ok(None);
    }
    handle.seek(SeekFrom::Start(0))?;
    let mut header = [0; 64];
    handle.read_exact(&mut header)?;
    let kind = u16::from_le_bytes(header[16..18].try_into()?);
    let machine = u16::from_le_bytes(header[18..20].try_into()?);
    let expected = match architecture {
        LinuxArchitecture::X86_64 => 62,
        LinuxArchitecture::Aarch64 => 183,
    };
    if header[..7] != *b"\x7fELF\x02\x01\x01"
        || machine != expected
        || !matches!(kind, 2 | 3)
        || (matches!(role, LinuxRuntimeRole::Library | LinuxRuntimeRole::Loader) && kind != 3)
        || u32::from_le_bytes(header[20..24].try_into()?) != 1
        || u16::from_le_bytes(header[52..54].try_into()?) != 64
        || u16::from_le_bytes(header[54..56].try_into()?) != 56
    {
        bail!("unsupported or malformed Linux ELF header");
    }
    let offset = u64::from_le_bytes(header[32..40].try_into()?);
    let count = u16::from_le_bytes(header[56..58].try_into()?);
    if count == 0
        || count > 1024
        || offset < 64
        || offset
            .checked_add(u64::from(count) * 56)
            .is_none_or(|end| end > size)
    {
        bail!("invalid Linux ELF program table");
    }
    handle.seek(SeekFrom::Start(offset))?;
    let mut executable_load = false;
    let mut interpreter = None;
    for _ in 0..count {
        let mut segment = [0; 56];
        handle.read_exact(&mut segment)?;
        let kind = u32::from_le_bytes(segment[..4].try_into()?);
        let flags = u32::from_le_bytes(segment[4..8].try_into()?);
        let offset = u64::from_le_bytes(segment[8..16].try_into()?);
        let file_size = u64::from_le_bytes(segment[32..40].try_into()?);
        let memory_size = u64::from_le_bytes(segment[40..48].try_into()?);
        if offset.checked_add(file_size).is_none_or(|end| end > size)
            || (kind == 1 && file_size > memory_size)
        {
            bail!("Linux ELF segment lies outside file or memory");
        }
        executable_load |= kind == 1 && flags & 1 != 0 && file_size != 0;
        if kind == 3 {
            if interpreter.is_some()
                || role == LinuxRuntimeRole::Loader
                || !(2..=4096).contains(&file_size)
            {
                bail!("invalid Linux ELF interpreter segment");
            }
            let mut bytes = vec![0; usize::try_from(file_size)?];
            handle.read_exact_at(&mut bytes, offset)?;
            if bytes.pop() != Some(0) || bytes.contains(&0) {
                bail!("Linux ELF interpreter requires one terminal NUL");
            }
            let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
            if !path.is_absolute() {
                bail!("Linux ELF interpreter must be absolute");
            }
            interpreter = Some(path);
        }
    }
    if !executable_load {
        bail!("Linux ELF image has no executable load segment");
    }
    Ok(interpreter)
}

#[cfg(test)]
#[path = "linux_interpreter_tests.rs"]
mod interpreter_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    // Independent small ELF64 fixtures: one load segment and no sections.
    pub(super) fn elf(machine: u16, kind: u16) -> Vec<u8> {
        let mut bytes = vec![0; 128];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&kind.to_le_bytes());
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
        bytes[64..68].copy_from_slice(&1u32.to_le_bytes());
        bytes[68..72].copy_from_slice(&5u32.to_le_bytes());
        bytes[96..104].copy_from_slice(&128u64.to_le_bytes());
        bytes[104..112].copy_from_slice(&128u64.to_le_bytes());
        bytes
    }

    pub(super) fn fixture(
        architecture: LinuxArchitecture,
    ) -> (tempfile::TempDir, LinuxRuntimeManifest) {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let machine = if architecture == LinuxArchitecture::X86_64 {
            62
        } else {
            183
        };
        let files = [
            ("tool", LinuxRuntimeRole::Clangd, elf(machine, 2)),
            ("library", LinuxRuntimeRole::Library, elf(machine, 3)),
            (
                "locale",
                LinuxRuntimeRole::Data,
                b"mapped resource".to_vec(),
            ),
        ]
        .into_iter()
        .map(|(name, role, bytes)| {
            let path = root.join(name);
            fs::write(&path, &bytes).unwrap();
            (
                name.to_owned(),
                LinuxRuntimeFile {
                    path,
                    role,
                    sha256: sha256_hex(&bytes),
                },
            )
        })
        .collect();
        (
            directory,
            LinuxRuntimeManifest {
                architecture,
                anonymous_execute: AnonymousExecutePolicy::Deny,
                files,
            },
        )
    }

    pub(super) fn replace_bytes(manifest: &mut LinuxRuntimeManifest, name: &str, bytes: &[u8]) {
        let pin = manifest.files.get_mut(name).unwrap();
        fs::write(&pin.path, bytes).unwrap();
        pin.sha256 = sha256_hex(bytes);
    }

    #[test]
    fn frozen_descriptor_controls() {
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../toolchain/linux/runtime_expectations.json"
        ))
        .unwrap();
        let mut observed = BTreeMap::new();
        for (label, architecture) in [
            ("valid_x86_64_elf", LinuxArchitecture::X86_64),
            ("valid_aarch64_elf", LinuxArchitecture::Aarch64),
        ] {
            let (_root, manifest) = fixture(architecture);
            let runtime = CapturedLinuxRuntime::capture(&manifest).unwrap();
            runtime.verify_unchanged().unwrap();
            observed.insert(label, "accept");
        }
        let (_root, original) = fixture(LinuxArchitecture::X86_64);
        let runtime = CapturedLinuxRuntime::capture(&original).unwrap();
        let (_other, relocated) = fixture(LinuxArchitecture::X86_64);
        assert_eq!(
            runtime.fingerprint(),
            CapturedLinuxRuntime::capture(&relocated)
                .unwrap()
                .fingerprint()
        );
        observed.insert("relocated_identical_files", "same_identity");
        for label in ["changed_role", "changed_anonymous_execute_policy"] {
            let mut changed = original.clone();
            if label == "changed_role" {
                changed.files.get_mut("tool").unwrap().role = LinuxRuntimeRole::Node;
            } else {
                changed.anonymous_execute = AnonymousExecutePolicy::Allow;
            }
            assert_ne!(
                runtime.fingerprint(),
                CapturedLinuxRuntime::capture(&changed)
                    .unwrap()
                    .fingerprint()
            );
            observed.insert(label, "different_identity");
        }
        for (label, name) in [
            ("changed_pinned_executable", "tool"),
            ("changed_pinned_library", "library"),
            ("changed_pinned_data", "locale"),
        ] {
            let (_root, manifest) = fixture(LinuxArchitecture::X86_64);
            let runtime = CapturedLinuxRuntime::capture(&manifest).unwrap();
            fs::write(&manifest.files[name].path, b"changed").unwrap();
            assert!(runtime.verify_unchanged().is_err());
            assert!(CapturedLinuxRuntime::capture(&manifest).is_err());
            observed.insert(label, "reject");
        }
        for label in [
            "changed_architecture",
            "missing_executable",
            "multiple_executables",
            "invalid_logical_name",
            "duplicate_physical_path",
            "hardlink_alias",
            "symlink_alias",
            "nonregular_file",
            "noncanonical_path",
            "malformed_digest",
            "elf32_image",
            "big_endian_image",
            "malformed_elf_header",
            "truncated_program_table",
            "segment_outside_file",
            "nonelf_library",
            "elf_disguised_as_data",
            "replaced_file_after_capture",
        ] {
            reject_descriptor_case(label);
            observed.insert(label, "reject");
        }
        assert_eq!(
            serde_json::to_value(observed).unwrap(),
            oracle["descriptor"]
        );
    }

    fn reject_descriptor_case(label: &str) {
        let (_root, mut manifest) = fixture(LinuxArchitecture::X86_64);
        match label {
            "changed_architecture" => manifest.architecture = LinuxArchitecture::Aarch64,
            "missing_executable" => {
                manifest.files.remove("tool");
            }
            "multiple_executables" => {
                manifest.files.get_mut("library").unwrap().role = LinuxRuntimeRole::Python;
            }
            "invalid_logical_name" => {
                let pin = manifest.files.remove("locale").unwrap();
                manifest.files.insert("../escape".to_owned(), pin);
            }
            "duplicate_physical_path" => {
                manifest
                    .files
                    .insert("alias".to_owned(), manifest.files["library"].clone());
            }
            "hardlink_alias" | "symlink_alias" => {
                let pin = manifest.files["library"].clone();
                let alias = pin.path.with_file_name("alias");
                if label == "hardlink_alias" {
                    fs::hard_link(&pin.path, &alias).unwrap();
                } else {
                    symlink(&pin.path, &alias).unwrap();
                }
                manifest.files.insert(
                    "alias".to_owned(),
                    LinuxRuntimeFile {
                        path: alias,
                        role: LinuxRuntimeRole::Library,
                        ..pin
                    },
                );
            }
            "nonregular_file" => {
                let path = &manifest.files["locale"].path;
                fs::remove_file(path).unwrap();
                fs::create_dir(path).unwrap();
            }
            "noncanonical_path" => {
                let pin = manifest.files.get_mut("locale").unwrap();
                pin.path = pin
                    .path
                    .parent()
                    .unwrap()
                    .join("../")
                    .join(pin.path.parent().unwrap().file_name().unwrap())
                    .join("locale");
            }
            "malformed_digest" => {
                manifest.files.get_mut("tool").unwrap().sha256 = "AF".repeat(32);
            }
            "elf32_image"
            | "big_endian_image"
            | "malformed_elf_header"
            | "truncated_program_table"
            | "segment_outside_file" => {
                let mut bytes = elf(62, 2);
                match label {
                    "elf32_image" => bytes[4] = 1,
                    "big_endian_image" => bytes[5] = 2,
                    "malformed_elf_header" => bytes[52] = 0,
                    "truncated_program_table" => bytes[56] = 2,
                    "segment_outside_file" => {
                        bytes[96..104].copy_from_slice(&129u64.to_le_bytes());
                    }
                    _ => unreachable!(),
                }
                replace_bytes(&mut manifest, "tool", &bytes);
            }
            "nonelf_library" => replace_bytes(&mut manifest, "library", b"not an ELF"),
            "elf_disguised_as_data" => {
                manifest.files.get_mut("library").unwrap().role = LinuxRuntimeRole::Data;
            }
            "replaced_file_after_capture" => {
                let captured = CapturedLinuxRuntime::capture(&manifest).unwrap();
                let path = &manifest.files["tool"].path;
                let replacement = path.with_file_name("replacement");
                fs::copy(path, &replacement).unwrap();
                fs::rename(replacement, path).unwrap();
                assert!(captured.verify_unchanged().is_err());
                return;
            }
            _ => unreachable!(),
        }
        let error = CapturedLinuxRuntime::capture(&manifest)
            .err()
            .unwrap_or_else(|| panic!("{label} must reject"));
        match label {
            "duplicate_physical_path" => assert!(error.to_string().contains("unique canonical")),
            "hardlink_alias" => assert!(error.to_string().contains("distinct regular files")),
            _ => {}
        }
    }
}
