//! Bind a Darwin resolver's loaded images to explicitly pinned runtime bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};

use crate::runtime::{CapturedRuntime, RuntimeRole};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Image {
    uuid: [u8; 16],
    address: u64,
}

/// Process-local proof, never serialized into a canonical snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedImages {
    catalogue_uuid: [u8; 16],
    images: BTreeMap<PathBuf, Image>,
    cache_slide: u64,
}

pub struct DarwinImageCatalog {
    executable_name: String,
    catalogue_uuid: [u8; 16],
    cached: BTreeMap<PathBuf, Image>,
    operator: BTreeMap<PathBuf, [u8; 16]>,
    sample: PathBuf,
    vmmap: PathBuf,
}

impl DarwinImageCatalog {
    /// Requires the arm64e shared-cache layout and thin arm64 operator images.
    pub fn capture(runtime: &CapturedRuntime) -> Result<Self> {
        let manifest = runtime.manifest();
        let primary = manifest
            .cache_family
            .directory
            .join(&manifest.cache_family.prefix);
        ensure!(
            manifest
                .files
                .values()
                .any(|file| file.path == primary && file.role == RuntimeRole::SharedCache),
            "primary shared cache is not pinned"
        );
        let mut cache = regular_file(&primary)?;
        let size = cache.metadata()?.len();
        let (catalogue_uuid, cached) = cache_catalogue(&mut cache, size)?;
        let mut operator = BTreeMap::new();
        let mut uuids = BTreeSet::new();
        for file in manifest.files.values().filter(|file| {
            matches!(
                file.role,
                RuntimeRole::Clangd
                    | RuntimeRole::Node
                    | RuntimeRole::Python
                    | RuntimeRole::Library
            )
        }) {
            let mut reader = regular_file(&file.path)?;
            let uuid = macho_uuid(&mut reader)?;
            ensure!(uuids.insert(uuid), "operator images have duplicate UUIDs");
            operator.insert(file.path.clone(), uuid);
        }
        let inspector = |basename: &str| -> Result<PathBuf> {
            let matches = manifest
                .files
                .values()
                .filter(|file| {
                    file.role == RuntimeRole::Inspector
                        && file.path.file_name().is_some_and(|name| name == basename)
                })
                .collect::<Vec<_>>();
            ensure!(
                matches.len() == 1,
                "runtime must pin exactly one {basename} inspector"
            );
            Ok(matches[0].path.clone())
        };
        let executable = manifest
            .files
            .values()
            .find(|file| {
                matches!(
                    file.role,
                    RuntimeRole::Clangd | RuntimeRole::Node | RuntimeRole::Python
                )
            })
            .context("runtime executable is missing")?;
        let executable_name = match executable.role {
            RuntimeRole::Clangd => "clangd",
            RuntimeRole::Node => "node",
            RuntimeRole::Python => executable
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .context("Python executable basename is invalid")?,
            _ => unreachable!(),
        }
        .to_owned();
        Ok(Self {
            executable_name,
            catalogue_uuid,
            cached,
            operator,
            sample: inspector("sample")?,
            vmmap: inspector("vmmap")?,
        })
    }

    /// Inspect only the selected live process. The caller compares this proof
    /// before/after queries and re-verifies runtime bytes before publication.
    pub fn inspect(&self, pid: u32) -> Result<LoadedImages> {
        ensure!(
            cfg!(all(target_os = "macos", target_arch = "aarch64")),
            "Darwin runtime inspection requires macOS arm64"
        );
        ensure!(pid != 0, "invalid resolver PID");
        let workspace = tempfile::tempdir()?;
        let report = workspace.path().join("sample.txt");
        let mut sample = Command::new(&self.sample);
        sample
            .args([
                pid.to_string(),
                "1".to_owned(),
                "1".to_owned(),
                "-file".to_owned(),
            ])
            .arg(&report);
        run_inspector(sample, workspace.path())?;
        let sampled = read_report(&report)?;
        let mut vmmap = Command::new(&self.vmmap);
        vmmap.args(["-w".to_owned(), pid.to_string()]);
        let mapped = run_inspector(vmmap, workspace.path())?;
        self.bind_reports(pid, &sampled, &mapped)
    }

    fn bind_reports(&self, pid: u32, sample: &str, vmmap: &str) -> Result<LoadedImages> {
        verify_process(pid, &self.executable_name, sample)?;
        verify_process(pid, &self.executable_name, vmmap)?;
        let mut mapped = mapped_images(vmmap)?;
        let reported_operator = self.bind_operator_paths(&mut mapped)?;
        let mapped_operator = mapped
            .iter()
            .filter(|(path, _)| !self.cached.contains_key(*path))
            .map(|(path, address)| (path.clone(), *address))
            .collect::<BTreeMap<_, _>>();
        ensure!(
            mapped_operator.keys().eq(self.operator.keys()),
            "loaded operator image paths differ from pins"
        );
        let (_, lines) = sample
            .split_once("Binary Images:\n")
            .context("sample omitted loaded images")?;
        let mut images = BTreeMap::new();
        let mut slides = BTreeSet::new();
        for line in lines.lines().filter(|line| !line.trim().is_empty()) {
            let (sampled_path, Image { uuid, address }) = sampled_image(line)?;
            let path = if self.cached.contains_key(&sampled_path) {
                let expected = self
                    .cached
                    .get(&sampled_path)
                    .context("loaded system image is not in pinned cache")?;
                ensure!(
                    uuid == expected.uuid,
                    "loaded system image UUID differs from pinned cache"
                );
                slides.insert(
                    address
                        .checked_sub(expected.address)
                        .context("invalid cache slide")?,
                );
                sampled_path.clone()
            } else {
                let matches = self
                    .operator
                    .iter()
                    .filter(|(path, expected)| {
                        **expected == uuid
                            && (sample_path_matches(path, &sampled_path)
                                || reported_operator.get(*path).is_some_and(|observed| {
                                    sample_path_matches(observed, &sampled_path)
                                }))
                    })
                    .collect::<Vec<_>>();
                ensure!(
                    matches.len() == 1,
                    "sampled operator UUID/name is not pinned uniquely"
                );
                let path = matches[0].0;
                ensure!(
                    mapped_operator.get(path) == Some(&address),
                    "sample and vmmap operator addresses differ"
                );
                path.clone()
            };
            ensure!(
                images.insert(path, Image { uuid, address }).is_none(),
                "duplicate sampled image"
            );
        }
        let loaded_operator = images
            .keys()
            .filter(|path| !self.cached.contains_key(*path))
            .collect::<BTreeSet<_>>();
        ensure!(
            loaded_operator == self.operator.keys().collect(),
            "sample operator closure differs from pins"
        );
        ensure!(
            images.contains_key(Path::new("/usr/lib/dyld")),
            "loaded-image proof omitted dyld"
        );
        ensure!(
            slides.len() == 1,
            "loaded cache images do not share one slide"
        );
        ensure!(
            images
                .iter()
                .all(|(path, image)| mapped.get(path) == Some(&image.address))
                && images.len() == mapped.len(),
            "sample and vmmap loaded image closures differ"
        );
        Ok(LoadedImages {
            catalogue_uuid: self.catalogue_uuid,
            images,
            cache_slide: *slides.first().context("no cache images")?,
        })
    }

    fn bind_operator_paths(
        &self,
        mapped: &mut BTreeMap<PathBuf, u64>,
    ) -> Result<BTreeMap<PathBuf, PathBuf>> {
        let observed = mapped
            .keys()
            .filter(|path| !self.cached.contains_key(*path))
            .cloned()
            .collect::<Vec<_>>();
        let mut reported = BTreeMap::new();
        for observed in observed {
            let canonical = if self.operator.contains_key(&observed) {
                observed.clone()
            } else {
                ensure!(
                    fs::symlink_metadata(&observed)?.is_file(),
                    "loaded operator file alias refused"
                );
                let canonical = fs::canonicalize(&observed)?;
                ensure!(
                    self.operator.contains_key(&canonical),
                    "loaded operator directory alias is not pinned"
                );
                let address = mapped
                    .remove(&observed)
                    .context("operator mapping is missing")?;
                ensure!(
                    mapped.insert(canonical.clone(), address).is_none(),
                    "duplicate canonical operator image"
                );
                canonical
            };
            reported.insert(canonical, observed);
        }
        Ok(reported)
    }
}

fn verify_process(pid: u32, executable_name: &str, report: &str) -> Result<()> {
    let process = report
        .lines()
        .find_map(|line| line.strip_prefix("Process:"))
        .context("inspector omitted process identity")?
        .trim();
    ensure!(
        process == format!("{executable_name} [{pid}]"),
        "inspector process identity differs from selected resolver"
    );
    ensure!(
        report.lines().any(|line| line
            .strip_prefix("Target Type:")
            .is_some_and(|value| value.trim() == "live task")),
        "inspector did not inspect a live task"
    );

    Ok(())
}

fn mapped_images(vmmap: &str) -> Result<BTreeMap<PathBuf, u64>> {
    let mut mapped = BTreeMap::new();
    for line in vmmap
        .split("==== Legend")
        .next()
        .context("vmmap regions missing")?
        .lines()
        .filter(|line| line.starts_with("__TEXT "))
    {
        let (prefix, path) = line.split_once(" /").context("vmmap omitted image path")?;
        let (start, end) = prefix
            .split_whitespace()
            .nth(1)
            .and_then(|range| range.split_once('-'))
            .context("vmmap omitted image range")?;
        let address = image_address(start, end)?;
        ensure!(
            mapped
                .insert(PathBuf::from(format!("/{path}")), address)
                .is_none(),
            "duplicate vmmap image path"
        );
    }
    Ok(mapped)
}

fn sampled_image(line: &str) -> Result<(PathBuf, Image)> {
    let (prefix, suffix) = line.split_once('<').context("malformed sampled image")?;
    let (uuid, path) = suffix
        .split_once("> ")
        .context("malformed sampled image UUID/path")?;
    let mut range = prefix.split_whitespace();
    let start = range.next().context("sample omitted image address")?;
    ensure!(
        range.next() == Some("-"),
        "sample omitted image range separator"
    );
    let end = range.next().context("sample omitted image end address")?;
    let address = image_address(start, end)?;
    let path = PathBuf::from(path.trim());
    ensure!(path.is_absolute(), "sample image path is not absolute");
    Ok((
        path,
        Image {
            uuid: parse_uuid(uuid)?,
            address,
        },
    ))
}

fn image_address(start: &str, end: &str) -> Result<u64> {
    let decode = |value: &str| u64::from_str_radix(value.strip_prefix("0x").unwrap_or(value), 16);
    let start = decode(start)?;
    ensure!(decode(end)? > start, "invalid runtime image address range");
    Ok(start)
}

fn sample_path_matches(expected: &Path, observed: &Path) -> bool {
    let (Some(expected), Some(observed)) = (expected.to_str(), observed.to_str()) else {
        return false;
    };
    if let Some((prefix, suffix)) = observed.split_once("/*/") {
        !suffix.contains('*')
            && expected.starts_with(&format!("{prefix}/"))
            && expected.ends_with(&format!("/{suffix}"))
    } else {
        expected == observed
    }
}

#[cfg(unix)]
fn regular_file(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "runtime catalogue input must be a regular file"
    );
    Ok(file)
}

#[cfg(not(unix))]
fn regular_file(_path: &Path) -> Result<fs::File> {
    anyhow::bail!("unsupported runtime catalogue host")
}

fn run_inspector(mut command: Command, workspace: &Path) -> Result<String> {
    let stdout = workspace.join("stdout.txt");
    let stderr = workspace.join("stderr.txt");
    let mut child = command
        .env_clear()
        .env("HOME", workspace)
        .env("TMPDIR", workspace)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout)?)
        .stderr(fs::File::create(stderr)?)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            outcome => {
                let _ = child.kill();
                let _ = child.wait();
                return match outcome {
                    Err(error) => Err(error.into()),
                    _ => Err(anyhow::anyhow!("runtime inspector timed out")),
                };
            }
        }
    };
    ensure!(status.success(), "runtime inspector failed");
    read_report(&stdout)
}

fn read_report(path: &Path) -> Result<String> {
    let mut reader = regular_file(path)?.take(16 * 1024 * 1024 + 1);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "runtime inspector report too large"
    );
    Ok(String::from_utf8(bytes)?)
}

fn cache_catalogue(
    reader: &mut (impl Read + Seek),
    size: u64,
) -> Result<([u8; 16], BTreeMap<PathBuf, Image>)> {
    let mut header = [0; 152];
    reader.read_exact(&mut header)?;
    ensure!(
        &header[..16] == b"dyld_v1  arm64e\0",
        "unsupported shared-cache format"
    );
    let catalogue_uuid: [u8; 16] = header[88..104].try_into()?;
    ensure!(
        catalogue_uuid != [0; 16],
        "shared-cache header omitted UUID"
    );
    let offset = u64::from_le_bytes(header[136..144].try_into()?);
    let count = u64::from_le_bytes(header[144..152].try_into()?);
    ensure!(
        count > 0
            && count <= 100_000
            && offset >= 152
            && offset
                .checked_add(count * 32)
                .is_some_and(|end| end <= size),
        "invalid shared-cache image table"
    );
    reader.seek(SeekFrom::Start(offset))?;
    let mut entries = vec![0; usize::try_from(count * 32)?];
    reader.read_exact(&mut entries)?;
    let mut images = BTreeMap::new();
    for entry in entries.chunks_exact(32) {
        let uuid = entry[..16].try_into()?;
        let address = u64::from_le_bytes(entry[16..24].try_into()?);
        let length = u32::from_le_bytes(entry[24..28].try_into()?);
        let path_offset = u64::from(u32::from_le_bytes(entry[28..32].try_into()?));
        ensure!(
            path_offset < size && length > 0 && address.checked_add(u64::from(length)).is_some(),
            "invalid shared-cache image record"
        );
        reader.seek(SeekFrom::Start(path_offset))?;
        let mut path = Vec::new();
        for _ in 0..4096 {
            let mut byte = [0];
            reader.read_exact(&mut byte)?;
            if byte[0] == 0 {
                break;
            }
            path.push(byte[0]);
        }
        ensure!(path.len() < 4096, "unterminated shared-cache image path");
        let path = PathBuf::from(String::from_utf8(path)?);
        ensure!(
            path.is_absolute()
                && !path.components().any(|part| matches!(
                    part,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )),
            "invalid shared-cache image path"
        );
        ensure!(
            images.insert(path, Image { uuid, address }).is_none(),
            "duplicate shared-cache image path"
        );
    }
    Ok((catalogue_uuid, images))
}

fn macho_uuid(reader: &mut impl Read) -> Result<[u8; 16]> {
    let mut header = [0; 32];
    reader.read_exact(&mut header)?;
    ensure!(
        u32::from_le_bytes(header[..4].try_into()?) == 0xfeed_facf
            && u32::from_le_bytes(header[4..8].try_into()?) == 0x0100_000c,
        "operator image must be thin arm64 Mach-O"
    );
    let count = u32::from_le_bytes(header[16..20].try_into()?);
    let size = u32::from_le_bytes(header[20..24].try_into()?);
    ensure!(
        count > 0 && count <= 10_000 && size <= 4 * 1024 * 1024,
        "invalid Mach-O load-command table"
    );
    let mut commands = vec![0; usize::try_from(size)?];
    reader.read_exact(&mut commands)?;
    let mut offset = 0;
    let mut uuid = None;
    for _ in 0..count {
        let command = commands
            .get(offset..offset + 8)
            .context("truncated Mach-O load command")?;
        let kind = u32::from_le_bytes(command[..4].try_into()?);
        let length = usize::try_from(u32::from_le_bytes(command[4..8].try_into()?))?;
        ensure!(
            length >= 8 && length % 8 == 0,
            "invalid Mach-O load-command size"
        );
        let end = offset
            .checked_add(length)
            .context("Mach-O load-command overflow")?;
        let data = commands
            .get(offset..end)
            .context("truncated Mach-O load-command bytes")?;
        if kind == 0x1b {
            ensure!(
                length == 24 && uuid.is_none(),
                "invalid or duplicate Mach-O UUID command"
            );
            uuid = Some(data[8..24].try_into()?);
        }
        offset = end;
    }
    ensure!(
        offset == commands.len(),
        "Mach-O load-command count/size mismatch"
    );
    uuid.context("operator image omitted UUID")
}

fn parse_uuid(value: &str) -> Result<[u8; 16]> {
    ensure!(
        value.len() == 36
            && [8, 13, 18, 23]
                .into_iter()
                .all(|index| value.as_bytes()[index] == b'-'),
        "invalid image UUID"
    );
    let hex = value.replace('-', "");
    ensure!(hex.len() == 32, "invalid image UUID");
    let mut uuid = [0; 16];
    for (index, byte) in uuid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(
            hex.get(index * 2..index * 2 + 2)
                .context("invalid image UUID encoding")?,
            16,
        )?;
    }
    Ok(uuid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn cache_bytes() -> Vec<u8> {
        let mut bytes = vec![0; 152 + 32];
        bytes[..16].copy_from_slice(b"dyld_v1  arm64e\0");
        bytes[88..104].fill(9);
        bytes[136..144].copy_from_slice(&152u64.to_le_bytes());
        bytes[144..152].copy_from_slice(&1u64.to_le_bytes());
        bytes[152..168].fill(1);
        bytes[168..176].copy_from_slice(&0x1000u64.to_le_bytes());
        bytes[176..180].copy_from_slice(&64u32.to_le_bytes());
        bytes[180..184].copy_from_slice(&184u32.to_le_bytes());
        bytes.extend(b"/usr/lib/dyld\0");
        bytes
    }

    #[test]
    fn cache_catalogue_has_bounded_offsets_counts_and_paths() {
        let bytes = cache_bytes();
        let read = |bytes: Vec<u8>| cache_catalogue(&mut Cursor::new(&bytes), bytes.len() as u64);
        assert_eq!(
            read(bytes.clone()).unwrap().1[Path::new("/usr/lib/dyld")].uuid,
            [1; 16]
        );
        let mut invalid = bytes.clone();
        invalid[88..104].fill(0);
        assert!(read(invalid).is_err());
        for length in [0, 151, 180, bytes.len() - 1] {
            assert!(read(bytes[..length].to_vec()).is_err());
        }
        let mut invalid = bytes.clone();
        invalid[136..144].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(read(invalid).is_err());
        let mut invalid = bytes.clone();
        invalid[144..152].copy_from_slice(&100_001u64.to_le_bytes());
        assert!(read(invalid).is_err());
        let mut invalid = bytes;
        invalid[184] = b'.';
        assert!(read(invalid).is_err());
    }

    fn macho_bytes() -> Vec<u8> {
        let mut bytes = vec![0; 56];
        bytes[..4].copy_from_slice(&0xfeed_facfu32.to_le_bytes());
        bytes[4..8].copy_from_slice(&0x0100_000cu32.to_le_bytes());
        bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
        bytes[20..24].copy_from_slice(&24u32.to_le_bytes());
        bytes[32..36].copy_from_slice(&0x1bu32.to_le_bytes());
        bytes[36..40].copy_from_slice(&24u32.to_le_bytes());
        bytes[40..56].fill(7);
        bytes
    }

    #[test]
    fn macho_uuid_requires_one_bounded_arm64_uuid_command() {
        let bytes = macho_bytes();
        let read = |bytes: Vec<u8>| macho_uuid(&mut Cursor::new(bytes));
        assert_eq!(read(bytes.clone()).unwrap(), [7; 16]);
        for length in [0, 31, 55] {
            assert!(read(bytes[..length].to_vec()).is_err());
        }
        let mut invalid = bytes.clone();
        invalid[4..8].fill(0);
        assert!(read(invalid).is_err());
        let mut invalid = bytes.clone();
        invalid[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read(invalid).is_err());
        let mut invalid = bytes.clone();
        invalid[16..20].copy_from_slice(&2u32.to_le_bytes());
        invalid[20..24].copy_from_slice(&48u32.to_le_bytes());
        invalid.extend_from_slice(&bytes[32..]);
        assert!(read(invalid).is_err());
        let mut invalid = bytes;
        invalid[32..36].copy_from_slice(&1u32.to_le_bytes());
        assert!(read(invalid).is_err());
    }

    fn reports() -> (DarwinImageCatalog, String, String) {
        let uuid = "01010101-0101-0101-0101-010101010101";
        let operator = "/opt/tools/clangd";
        let catalog = DarwinImageCatalog {
            executable_name: "clangd".to_owned(),
            catalogue_uuid: [9; 16],
            cached: BTreeMap::from([(
                PathBuf::from("/usr/lib/dyld"),
                Image {
                    uuid: [1; 16],
                    address: 0x1000,
                },
            )]),
            operator: BTreeMap::from([(PathBuf::from(operator), [1; 16])]),
            sample: PathBuf::new(),
            vmmap: PathBuf::new(),
        };
        let header = "Process: clangd [42]\nTarget Type: live task\n";
        let sample = format!(
            "{header}Binary Images:\n0x5000 - 0x5100 clangd <{uuid}> /opt/*/clangd\n0x2000 - 0x2100 dyld <{uuid}> /usr/lib/dyld\n"
        );
        let vmmap = format!(
            "{header}__TEXT 5000-5100 [ 1K ] r-x/r-x {operator}\n__TEXT 2000-2100 [ 1K ] r-x/r-x /usr/lib/dyld\n"
        );
        (catalog, sample, vmmap)
    }

    #[test]
    fn reports_require_exact_pid_uuid_path_address_and_complete_closure() {
        let (catalog, sample, vmmap) = reports();
        assert!(catalog.bind_reports(42, &sample, &vmmap).is_ok());
        assert!(catalog.bind_reports(43, &sample, &vmmap).is_err());
        for invalid in [
            sample.replace("live task", "terminated"),
            sample.replace("01010101-", "02020202-"),
            sample.replace("/usr/lib/dyld", "/usr/lib/unknown"),
            sample.replace("0x5000", "0x6000"),
            sample.replace("/opt/*/clangd", "/opt/*/other"),
            sample.replace("/opt/*/clangd", "/elsewhere/*/clangd"),
            format!(
                "{sample}0x5000 - 0x5100 clangd <01010101-0101-0101-0101-010101010101> /opt/*/clangd\n"
            ),
        ] {
            assert!(catalog.bind_reports(42, &invalid, &vmmap).is_err());
        }
        for invalid in [
            vmmap.replace("/opt/tools/clangd", "/opt/other/clangd"),
            vmmap.replace("5000-5100", "6000-6100"),
            vmmap.replace("2000-2100", "3000-3100"),
            vmmap.replace("__TEXT 2000", "__DATA 2000"),
            vmmap.replace("__TEXT 5000", "__DATA 5000"),
        ] {
            assert!(catalog.bind_reports(42, &sample, &invalid).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn directory_alias_reports_bind_only_the_pinned_regular_image() {
        use std::os::unix::fs::symlink;
        let scratch = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(scratch.path()).unwrap();
        let root = base.join("operator");
        fs::create_dir(&root).unwrap();
        let image = root.join("clangd");
        fs::write(&image, b"owned image fixture").unwrap();
        let alias = base.join("alias");
        symlink(&root, &alias).unwrap();
        let reported = alias.join("clangd");
        let (mut catalog, sample, vmmap) = reports();
        catalog.operator = BTreeMap::from([(image.clone(), [1; 16])]);
        let direct_sample = sample.replace("/opt/*/clangd", image.to_str().unwrap());
        let direct_vmmap = vmmap.replace("/opt/tools/clangd", image.to_str().unwrap());
        let expected = catalog
            .bind_reports(42, &direct_sample, &direct_vmmap)
            .unwrap();
        let alias_sample = sample.replace("/opt/*/clangd", reported.to_str().unwrap());
        let alias_vmmap = vmmap.replace("/opt/tools/clangd", reported.to_str().unwrap());
        assert_eq!(
            catalog
                .bind_reports(42, &alias_sample, &alias_vmmap)
                .unwrap(),
            expected
        );
        assert_eq!(
            catalog
                .bind_reports(42, &direct_sample, &alias_vmmap)
                .unwrap(),
            expected
        );
        assert!(
            catalog
                .bind_reports(
                    42,
                    &alias_sample.replace("01010101-", "02020202-"),
                    &alias_vmmap
                )
                .is_err()
        );
        assert!(
            catalog
                .bind_reports(42, &alias_sample.replace("0x5000", "0x6000"), &alias_vmmap)
                .is_err()
        );
        let duplicate = format!(
            "{alias_vmmap}__TEXT 7000-7100 [ 1K ] r-x/r-x {}\n",
            image.display()
        );
        assert!(catalog.bind_reports(42, &alias_sample, &duplicate).is_err());
        let file_alias = base.join("clangd");
        symlink(&image, &file_alias).unwrap();
        let final_alias = vmmap.replace("/opt/tools/clangd", file_alias.to_str().unwrap());
        assert!(
            catalog
                .bind_reports(42, &alias_sample, &final_alias)
                .is_err()
        );
        fs::remove_file(&alias).unwrap();
        let other = base.join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("clangd"), b"unapproved image").unwrap();
        symlink(&other, &alias).unwrap();
        assert!(
            catalog
                .bind_reports(42, &alias_sample, &alias_vmmap)
                .is_err()
        );
    }

    #[test]
    fn cached_images_require_a_common_slide_and_a_loaded_loader() {
        let (mut catalog, sample, vmmap) = reports();
        catalog.cached.insert(
            PathBuf::from("/usr/lib/second.dylib"),
            Image {
                uuid: [2; 16],
                address: 0x3000,
            },
        );
        let second =
            "0x4000 - 0x4100 second <02020202-0202-0202-0202-020202020202> /usr/lib/second.dylib\n";
        let sample = format!("{sample}{second}");
        let vmmap = format!("{vmmap}__TEXT 4000-4100 [ 1K ] r-x/r-x /usr/lib/second.dylib\n");
        assert!(catalog.bind_reports(42, &sample, &vmmap).is_ok());
        assert!(
            catalog
                .bind_reports(
                    42,
                    &sample.replace("0x4000", "0x6000"),
                    &vmmap.replace("4000-4100", "6000-6100")
                )
                .is_err()
        );
        let sample = sample
            .lines()
            .filter(|line| !line.ends_with("/usr/lib/dyld"))
            .collect::<Vec<_>>()
            .join("\n");
        let vmmap = vmmap
            .lines()
            .filter(|line| !line.ends_with("/usr/lib/dyld"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(catalog.bind_reports(42, &sample, &vmmap).is_err());
    }

    #[test]
    fn node_reports_use_the_selected_executable_identity() {
        let (mut catalog, sample, vmmap) = reports();
        let sample = sample.replace("clangd", "node");
        let vmmap = vmmap.replace("clangd", "node");
        assert!(catalog.bind_reports(42, &sample, &vmmap).is_err());
        catalog.executable_name = "node".to_owned();
        catalog.operator = BTreeMap::from([(PathBuf::from("/opt/tools/node"), [1; 16])]);
        assert!(catalog.bind_reports(42, &sample, &vmmap).is_ok());
        assert!(catalog.bind_reports(43, &sample, &vmmap).is_err());
        assert!(
            catalog
                .bind_reports(
                    42,
                    &sample.replace("Process: node", "Process: clangd"),
                    &vmmap
                )
                .is_err()
        );
    }

    #[test]
    fn python_reports_bind_the_selected_interpreter_name_path_and_uuid() {
        let (mut catalog, sample, vmmap) = reports();
        let sample = sample.replace("clangd", "python3.14");
        let vmmap = vmmap.replace("clangd", "python3.14");
        assert!(catalog.bind_reports(42, &sample, &vmmap).is_err());
        catalog.executable_name = "python3.14".to_owned();
        catalog.operator = BTreeMap::from([(PathBuf::from("/opt/tools/python3.14"), [1; 16])]);
        assert!(catalog.bind_reports(42, &sample, &vmmap).is_ok());
        for invalid in [
            sample.replace("Process: python3.14", "Process: node"),
            sample.replace("/opt/*/python3.14", "/elsewhere/*/python3.14"),
            sample.replace(
                "01010101-0101-0101-0101-010101010101",
                "02020202-0202-0202-0202-020202020202",
            ),
        ] {
            assert_ne!(invalid, sample);
            assert!(catalog.bind_reports(42, &invalid, &vmmap).is_err());
        }
        assert!(catalog.bind_reports(43, &sample, &vmmap).is_err());
    }

    #[test]
    #[ignore = "requires explicit pinned Darwin Node runtime and compiler bundle"]
    #[allow(clippy::too_many_lines)] // One live process binds runtime inspection to all OS guard controls.
    fn pinned_node_images_and_os_guard() {
        use crate::runtime::{RuntimeManifest, guard_policy};
        use std::io::{ErrorKind, Write};
        use std::net::TcpListener;

        struct Process(std::process::Child);
        impl Drop for Process {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let manifest: RuntimeManifest = serde_json::from_slice(
            &fs::read(
                std::env::var("CODEMAP_JS_TS_RUNTIME_MANIFEST")
                    .expect("explicit Node runtime manifest"),
            )
            .unwrap(),
        )
        .unwrap();
        let runtime = CapturedRuntime::capture_node(&manifest).unwrap();
        let catalog = DarwinImageCatalog::capture(&runtime).unwrap();
        let node = manifest
            .files
            .values()
            .find(|file| file.role == RuntimeRole::Node)
            .unwrap();
        let sandboxes = manifest
            .files
            .values()
            .filter(|file| file.role == RuntimeRole::Sandbox)
            .collect::<Vec<_>>();
        assert_eq!(sandboxes.len(), 1);
        let compiler = fs::canonicalize(
            PathBuf::from(
                std::env::var("CODEMAP_JS_TS_PROBE_BUNDLE").expect("explicit compiler bundle"),
            )
            .join("node_modules/typescript/lib/typescript.js"),
        )
        .unwrap();
        let compiler_bytes = fs::read(&compiler).unwrap();
        let pin: serde_json::Value =
            serde_json::from_str(include_str!("../../../toolchain/js_ts/manifest.json")).unwrap();
        assert_eq!(
            crate::schema::sha256_hex(&compiler_bytes),
            pin["compiler_sha256"].as_str().unwrap()
        );
        let temporary = tempfile::Builder::new()
            .prefix("codemap node guard é ")
            .tempdir()
            .unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        let state = root.join("state");
        fs::create_dir(&state).unwrap();
        let outside = root.join("outside");
        fs::write(&outside, "unchanged").unwrap();
        let ready = state.join("ready");
        let done = state.join("done");
        let helper = state.join("guard.mjs");
        fs::write(
            &helper,
            include_str!("../../../tests/fixtures/js_ts_runtime/guard.mjs"),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let request = state.join("request.json");
        fs::write(&request, serde_json::json!({"compiler":compiler,"outside":outside,"ready":ready,"done":done,"port":listener.local_addr().unwrap().port()}).to_string()).unwrap();
        let error = state.join("stderr");
        let mut child = Process(
            Command::new(&sandboxes[0].path)
                .env_clear()
                .env("HOME", &state)
                .env("TMPDIR", &state)
                .current_dir(&state)
                .args(["-p", &guard_policy(&state, &node.path).unwrap()])
                .arg(&node.path)
                .arg("--no-addons")
                .arg(&helper)
                .arg(&request)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(fs::File::create(&error).unwrap())
                .spawn()
                .unwrap(),
        );
        let wait_marker = |child: &mut Process, marker: &Path| {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !marker.is_file() {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "Node exited: {}",
                    fs::read_to_string(&error).unwrap()
                );
                assert!(Instant::now() < deadline, "Node marker deadline exceeded");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        wait_marker(&mut child, &ready);
        let before = catalog.inspect(child.0.id()).unwrap();
        child.0.stdin.as_mut().unwrap().write_all(b"1").unwrap();
        wait_marker(&mut child, &done);
        assert_eq!(before, catalog.inspect(child.0.id()).unwrap());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "unchanged");
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        assert_eq!(
            fs::read_to_string(&done).unwrap(),
            "parsed and denied network, execution and outside write"
        );
        assert_eq!(fs::read(&compiler).unwrap(), compiler_bytes);
        runtime.verify_unchanged().unwrap();
        child.0.stdin.as_mut().unwrap().write_all(b"2").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "Node failed: {}",
                    fs::read_to_string(&error).unwrap()
                );
                break;
            }
            assert!(Instant::now() < deadline, "Node exit deadline exceeded");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    #[ignore = "requires explicit pinned Darwin runtime and live clangd"]
    fn pinned_live_images_before_and_after_parsing() {
        use crate::lsp::{self, Transport};
        use crate::runtime::{RuntimeManifest, RuntimeRole};
        use serde_json::json;
        let path =
            std::env::var("CODEMAP_CPP_RUNTIME_MANIFEST").expect("explicit runtime manifest");
        let manifest: RuntimeManifest = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let runtime = CapturedRuntime::capture(&manifest).unwrap();
        let catalog = DarwinImageCatalog::capture(&runtime).unwrap();
        let binary = manifest
            .files
            .values()
            .find(|file| file.role == RuntimeRole::Clangd)
            .unwrap();
        let sandbox = manifest
            .files
            .values()
            .find(|file| file.role == RuntimeRole::Sandbox)
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let private = fs::canonicalize(root.path()).unwrap();
        let state = private.join("state");
        fs::create_dir(&state).unwrap();
        let source = private.join("fixture.cpp");
        let text = "int helper(int n) { return n + 1; }\nint main() { return helper(1); }\n";
        fs::write(&source, text).unwrap();
        fs::write(private.join("compile_commands.json"), json!([{"directory":private,"file":source,"arguments":["/usr/bin/false","-xc++","-std=c++20",source]}]).to_string()).unwrap();
        let policy = format!(
            "(version 1)(allow default)(deny network*)(deny file-write*)(allow file-write* (subpath {}))(deny process-exec)(allow process-exec (literal {}))",
            serde_json::to_string(&state).unwrap(),
            serde_json::to_string(&binary.path).unwrap()
        );
        let mut command = Command::new(&sandbox.path);
        command
            .env_clear()
            .env("HOME", &state)
            .env("TMPDIR", &state)
            .current_dir(&state)
            .stderr(Stdio::null())
            .args(["-p", &policy])
            .arg(&binary.path)
            .args([
                "--enable-config=false",
                "--background-index=false",
                "--clang-tidy=false",
                "--pch-storage=memory",
                "--skip-preamble-build",
                "--log=error",
            ])
            .arg(format!("--compile-commands-dir={}", private.display()));
        let transport = Transport::spawn("clangd-runtime-probe", &mut command, None).unwrap();
        transport
            .request(
                "initialize",
                &json!({"processId":null,"rootUri":lsp::uri_from_path(&private),"capabilities":{}}),
            )
            .unwrap();
        transport.notify("initialized", &json!({})).unwrap();
        let pid = transport.process_id().unwrap();
        let before = catalog.inspect(pid).unwrap();
        let uri = lsp::uri_from_path(&source);
        transport
            .notify(
                "textDocument/didOpen",
                &json!({"textDocument":{"uri":uri,"languageId":"cpp","version":1,"text":text}}),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let message = transport
                .unsolicited()
                .unwrap()
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == uri
            {
                let diagnostics = message["params"]["diagnostics"].as_array().unwrap();
                assert!(
                    diagnostics
                        .iter()
                        .all(|diagnostic| matches!(diagnostic["severity"].as_u64(), Some(2..=4)))
                );
                break;
            }
        }
        assert_eq!(
            before,
            catalog.inspect(transport.process_id().unwrap()).unwrap()
        );
        runtime.verify_unchanged().unwrap();
        transport.shutdown();
    }
}
