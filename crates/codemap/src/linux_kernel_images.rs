//! Complete Linux executable-image admission. Source closure and query transport
//! still require their own gates before semantic facts can be emitted.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::linux_guard::LINUX_GUARD_POLICY;
use crate::linux_maps::{Mapping, parse_maps};
use crate::linux_process::StoppedLinuxChild;
use crate::linux_runtime::{AnonymousExecutePolicy, CapturedLinuxRuntime, LinuxArchitecture};
use crate::schema::sha256_hex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxVsyscallPolicy {
    Absent,
    FixedEmulated,
}

/// Operator-supplied pins, never learned from the process being admitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxKernelPins {
    pub architecture: LinuxArchitecture,
    pub vdso_size: u64,
    pub vdso_sha256: String,
    pub vsyscall: LinuxVsyscallPolicy,
}

impl LinuxKernelPins {
    pub fn validate(&self) -> Result<()> {
        if !(4096..=1024 * 1024).contains(&self.vdso_size)
            || self.vdso_size % 4096 != 0
            || self.vdso_sha256.len() != 64
            || !self
                .vdso_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self.architecture == LinuxArchitecture::Aarch64
                && self.vsyscall != LinuxVsyscallPolicy::Absent
        {
            bail!("invalid Linux kernel image pins");
        }
        Ok(())
    }

    pub fn fingerprint(&self, runtime: &CapturedLinuxRuntime) -> Result<String> {
        self.validate()?;
        if self.architecture != runtime.manifest.architecture {
            bail!("Linux kernel architecture differs from runtime");
        }
        Ok(sha256_hex(&serde_json::to_vec(&(
            "linux-complete-images/1",
            runtime.fingerprint(),
            LINUX_GUARD_POLICY,
            self,
        ))?))
    }
}

impl CapturedLinuxRuntime {
    /// Bind all file-backed and kernel executable bytes to the stopped guarded
    /// child. No anonymous-execution extension is accepted by this policy.
    pub fn verify_complete_images(
        &self,
        child: &StoppedLinuxChild<'_>,
        kernel: &LinuxKernelPins,
    ) -> Result<()> {
        if child.guard_policy() != Some(LINUX_GUARD_POLICY) {
            bail!("Linux kernel witness requires the prepared process guard");
        }
        if self.manifest.anonymous_execute != AnonymousExecutePolicy::Deny {
            bail!("Linux anonymous image admission extension is not accepted");
        }
        kernel.fingerprint(self)?;
        let report = child.maps()?;
        let auxv = child.auxiliary_vector()?;
        let maps = parse_maps(&report)?;
        let vdso = kernel_mapping(&maps, &auxv, kernel)?;
        let mut bytes = vec![0; usize::try_from(kernel.vdso_size)?];
        child.read_memory(vdso.start, &mut bytes)?;
        verify_vdso_bytes(&bytes, kernel)?;
        self.verify_file_backed_images(child)?;
        if child.maps()? != report || child.auxiliary_vector()? != auxv {
            bail!("Linux kernel witnesses changed during image admission");
        }
        self.verify_unchanged()?;
        child.verify_identity()
    }
}

fn kernel_address(auxv: &[u8]) -> Result<u64> {
    if auxv.is_empty() || auxv.len() > 4096 || auxv.len() % 16 != 0 {
        bail!("incomplete Linux auxiliary vector");
    }
    let mut seen = BTreeSet::new();
    let mut address = None;
    for (index, pair) in auxv.chunks_exact(16).enumerate() {
        let tag = u64::from_le_bytes(pair[..8].try_into()?);
        let value = u64::from_le_bytes(pair[8..].try_into()?);
        if !seen.insert(tag) {
            bail!("duplicate Linux auxiliary vector tag");
        }
        if tag == 0 {
            if value != 0 || index + 1 != auxv.len() / 16 {
                bail!("invalid Linux auxiliary vector terminator");
            }
            return address
                .filter(|address| *address != 0)
                .context("missing Linux vDSO auxiliary address");
        }
        if tag == 33 {
            address = Some(value);
        }
    }
    bail!("missing Linux auxiliary vector terminator")
}

fn kernel_mapping<'a>(
    maps: &'a [Mapping],
    auxv: &[u8],
    pins: &LinuxKernelPins,
) -> Result<&'a Mapping> {
    pins.validate()?;
    let address = kernel_address(auxv)?;
    let mut vdso = None;
    let mut vsyscall = false;
    for map in maps.iter().filter(|map| map.path.is_none()) {
        match map.label.as_deref() {
            Some("[vdso]") => {
                if vdso.is_some()
                    || map.start != address
                    || map.end - map.start != pins.vdso_size
                    || !map.read
                    || map.write
                    || !map.execute
                    || !map.private
                    || map.offset != 0
                    || map.major != 0
                    || map.minor != 0
                    || map.inode != 0
                {
                    bail!("invalid Linux vDSO mapping");
                }
                vdso = Some(map);
            }
            Some("[vsyscall]") => {
                if vsyscall
                    || pins.architecture != LinuxArchitecture::X86_64
                    || pins.vsyscall != LinuxVsyscallPolicy::FixedEmulated
                    || map.start != 0xffff_ffff_ff60_0000
                    || map.end != 0xffff_ffff_ff60_1000
                    || map.read
                    || map.write
                    || !map.execute
                    || !map.private
                    || map.offset != 0
                    || map.major != 0
                    || map.minor != 0
                    || map.inode != 0
                {
                    bail!("invalid Linux vsyscall mapping");
                }
                vsyscall = true;
            }
            _ if map.execute => bail!("unapproved Linux anonymous executable mapping"),
            _ => {}
        }
    }
    if vsyscall != (pins.vsyscall == LinuxVsyscallPolicy::FixedEmulated) {
        bail!("missing selected Linux vsyscall mapping");
    }
    vdso.context("missing Linux vDSO mapping")
}

fn verify_vdso_bytes(bytes: &[u8], pins: &LinuxKernelPins) -> Result<()> {
    if bytes.len() as u64 != pins.vdso_size || sha256_hex(bytes) != pins.vdso_sha256 {
        bail!("Linux vDSO memory differs from operator byte pins");
    }
    let header = bytes.get(..64).context("short Linux vDSO ELF header")?;
    let machine = match pins.architecture {
        LinuxArchitecture::X86_64 => 62,
        LinuxArchitecture::Aarch64 => 183,
    };
    if header[..7] != *b"\x7fELF\x02\x01\x01"
        || u16::from_le_bytes(header[16..18].try_into()?) != 3
        || u16::from_le_bytes(header[18..20].try_into()?) != machine
        || u32::from_le_bytes(header[20..24].try_into()?) != 1
        || u16::from_le_bytes(header[52..54].try_into()?) != 64
        || u16::from_le_bytes(header[54..56].try_into()?) != 56
    {
        bail!("unsupported Linux vDSO ELF header");
    }
    let offset = usize::try_from(u64::from_le_bytes(header[32..40].try_into()?))?;
    let count = usize::from(u16::from_le_bytes(header[56..58].try_into()?));
    let end = offset
        .checked_add(count * 56)
        .context("Linux vDSO program table overflow")?;
    if count == 0 || count > 1024 || offset < 64 || end > bytes.len() {
        bail!("invalid Linux vDSO program table");
    }
    let mut executable = false;
    for segment in bytes[offset..end].chunks_exact(56) {
        if u32::from_le_bytes(segment[..4].try_into()?) != 1 {
            continue;
        }
        let flags = u32::from_le_bytes(segment[4..8].try_into()?);
        let file_offset = u64::from_le_bytes(segment[8..16].try_into()?);
        let address = u64::from_le_bytes(segment[16..24].try_into()?);
        let size = u64::from_le_bytes(segment[32..40].try_into()?);
        let memory = u64::from_le_bytes(segment[40..48].try_into()?);
        if size > memory
            || file_offset
                .checked_add(size)
                .is_none_or(|end| end > pins.vdso_size)
            || address
                .checked_add(memory)
                .is_none_or(|end| end > pins.vdso_size)
            || flags & 3 == 3
        {
            bail!("invalid Linux vDSO load segment");
        }
        executable |= flags & 1 != 0;
    }
    if !executable {
        bail!("Linux vDSO has no executable load");
    }
    Ok(())
}

#[cfg(test)]
#[path = "linux_kernel_tests.rs"]
mod tests;
