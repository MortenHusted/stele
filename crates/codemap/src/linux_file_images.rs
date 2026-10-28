//! File-backed image witnesses only. Kernel/anonymous mappings and guards
//! require separate admission before any semantic launcher can use this result.

use super::{CapturedLinuxRuntime, LinuxArchitecture, LinuxRuntimeRole, PinnedFile};
use crate::linux_maps::{Mapping, parse_maps};
use crate::linux_process::StoppedLinuxChild;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::os::unix::fs::FileExt;

struct Load {
    offset: u64,
    address: u64,
    file_end: u64,
    end: u64,
    memory_end: u64,
    flags: u32,
    zero_tail: bool,
}

impl CapturedLinuxRuntime {
    /// Verify the complete file-backed closure and executable bytes while the
    /// exact owned child is stopped. This does not admit anonymous/kernel code,
    /// prove read-input closure, or substitute for production guard enforcement.
    pub fn verify_file_backed_images(&self, child: &StoppedLinuxChild<'_>) -> Result<()> {
        let native = if cfg!(target_arch = "x86_64") {
            LinuxArchitecture::X86_64
        } else if cfg!(target_arch = "aarch64") {
            LinuxArchitecture::Aarch64
        } else {
            bail!("unsupported Linux image architecture");
        };
        if self.manifest.architecture != native {
            bail!("Linux image architecture differs from host");
        }
        self.verify_unchanged()?;
        let report = child.maps()?;
        let maps = parse_maps(&report)?;
        // SAFETY: sysconf has no pointer arguments and queries the host page size.
        let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })?;
        if !page.is_power_of_two() || !(4096..=65536).contains(&page) {
            bail!("unsupported Linux page size");
        }
        let by_path: BTreeMap<_, _> = self
            .manifest
            .files
            .iter()
            .map(|(name, pin)| (&pin.path, (name.as_str(), pin)))
            .collect();
        let mut grouped: BTreeMap<&str, Vec<&Mapping>> = BTreeMap::new();
        for map in &maps {
            let Some(path) = &map.path else {
                continue;
            };
            let &(name, pin) = by_path
                .get(path)
                .context("Linux mapped file outside pinned closure")?;
            let stamp = self.files[name].stamp;
            if map.inode != stamp.inode
                || libc::makedev(map.major, map.minor) != stamp.device
                || fs::canonicalize(path)? != *path
                || child.mapped_file(map.start, map.end)? != *path
            {
                bail!("Linux mapped file does not match retained pin");
            }
            if map.start % page != 0 || map.end % page != 0 || map.offset % page != 0 {
                bail!("Linux mapping is not page aligned");
            }
            if pin.role == LinuxRuntimeRole::Data && map.execute {
                bail!("mapped data is executable");
            }
            grouped.entry(name).or_default().push(map);
        }
        let mut budget = 512 * 1024 * 1024u64;
        let executable = child.executable()?;
        for (name, pin) in &self.manifest.files {
            let image_maps = grouped.get(name.as_str());
            if pin.role == LinuxRuntimeRole::Data {
                continue;
            }
            if pin.role.is_executable() && pin.path != executable {
                bail!("Linux executable differs from runtime pin");
            }
            let image_maps = image_maps.context("required pinned Linux image is not mapped")?;
            let file = &self.files[name];
            verify_elf_mappings(file, child, image_maps, page, &mut budget)?;
        }
        if child.maps()? != report {
            bail!("Linux maps changed during file-image inspection");
        }
        self.verify_unchanged()?;
        child.verify_identity()
    }
}

fn verify_elf_mappings(
    file: &PinnedFile,
    child: &StoppedLinuxChild<'_>,
    image_maps: &[&Mapping],
    page: u64,
    budget: &mut u64,
) -> Result<()> {
    let (fixed, loads) = loads(&file.handle, page)?;
    let mut bias = None;
    for map in image_maps.iter().filter(|map| map.execute) {
        if !map.private || map.write || !map.read {
            bail!("Linux executable mapping must be private and read-only");
        }
        let candidates: Vec<_> = loads
            .iter()
            .filter(|load| load.flags & 1 != 0 && contains(load, map))
            .map(|load| load_bias(load, map))
            .collect::<Result<_>>()?;
        if candidates.len() != 1 {
            bail!("Linux executable map outside unique ELF load");
        }
        if fixed && candidates[0] != 0 || bias.is_some_and(|previous| previous != candidates[0]) {
            bail!("Linux ELF load address is inconsistent");
        }
        bias = Some(candidates[0]);
        let load = loads
            .iter()
            .find(|load| load.flags & 1 != 0 && contains(load, map))
            .unwrap();
        compare_memory(child, &file.handle, file.stamp.size, map, load, budget)?;
    }
    let bias = bias.context("pinned Linux image has no executable map")?;
    for map in image_maps {
        if !loads.iter().any(|load| {
            contains(load, map)
                && load_bias(load, map).ok() == Some(bias)
                && (!map.read || load.flags & 4 != 0)
                && (!map.write || load.flags & 2 != 0)
                && (!map.execute || load.flags & 1 != 0)
        }) && !reserved_gap(&loads, map, bias, fixed)
        {
            bail!("Linux file mapping does not correspond to ELF load");
        }
    }
    for load in loads.iter().filter(|load| load.flags & 1 != 0) {
        let mut covered = load.offset;
        for map in image_maps
            .iter()
            .filter(|map| map.execute && contains(load, map))
        {
            if map.offset != covered {
                bail!("Linux executable ELF load is incompletely mapped");
            }
            covered = covered
                .checked_add(map.end - map.start)
                .context("Linux coverage overflow")?;
        }
        if covered != load.end {
            bail!("Linux executable ELF load is missing pages");
        }
    }
    Ok(())
}

fn reserved_gap(loads: &[Load], map: &Mapping, bias: u64, fixed: bool) -> bool {
    if fixed || !map.private || map.read || map.write || map.execute {
        return false;
    }
    let Some(start) = map.start.checked_sub(bias) else {
        return false;
    };
    let Some(end) = map.end.checked_sub(bias) else {
        return false;
    };
    let Some(first) = loads.iter().min_by_key(|load| load.address) else {
        return false;
    };
    let Some(previous) = loads
        .iter()
        .filter(|load| load.address < start)
        .max_by_key(|load| load.address)
    else {
        return false;
    };
    let Some(next) = loads
        .iter()
        .filter(|load| load.address >= end)
        .min_by_key(|load| load.address)
    else {
        return false;
    };
    previous.memory_end <= start
        && end <= next.address
        && loads
            .iter()
            .all(|load| load.memory_end <= start || load.address >= end)
        && start
            .checked_sub(first.address)
            .and_then(|distance| first.offset.checked_add(distance))
            == Some(map.offset)
}

fn contains(load: &Load, map: &Mapping) -> bool {
    map.offset >= load.offset
        && map
            .offset
            .checked_add(map.end - map.start)
            .is_some_and(|end| end <= load.end)
}

fn load_bias(load: &Load, map: &Mapping) -> Result<u64> {
    let relative = load
        .address
        .checked_add(map.offset - load.offset)
        .context("ELF address overflow")?;
    map.start
        .checked_sub(relative)
        .context("ELF mapping address underflow")
}

fn loads(file: &File, page: u64) -> Result<(bool, Vec<Load>)> {
    let mut header = [0; 64];
    file.read_exact_at(&mut header, 0)?;
    let fixed = u16::from_le_bytes(header[16..18].try_into()?) == 2;
    let table = u64::from_le_bytes(header[32..40].try_into()?);
    let count = u16::from_le_bytes(header[56..58].try_into()?);
    let mut loads = Vec::new();
    for index in 0..count {
        let mut segment = [0; 56];
        file.read_exact_at(&mut segment, table + u64::from(index) * 56)?;
        if u32::from_le_bytes(segment[..4].try_into()?) != 1 {
            continue;
        }
        let flags = u32::from_le_bytes(segment[4..8].try_into()?);
        let offset = u64::from_le_bytes(segment[8..16].try_into()?);
        let address = u64::from_le_bytes(segment[16..24].try_into()?);
        let file_size = u64::from_le_bytes(segment[32..40].try_into()?);
        let memory_size = u64::from_le_bytes(segment[40..48].try_into()?);
        if offset % page != address % page {
            bail!("ELF load offsets are not page congruent");
        }
        if memory_size == 0 {
            continue;
        }
        let file_end = offset
            .checked_add(file_size)
            .context("ELF file load overflow")?;
        let end = if file_size == 0 {
            offset & !(page - 1)
        } else {
            file_end
                .checked_add(page - 1)
                .context("ELF page overflow")?
                & !(page - 1)
        };
        let memory_end = address
            .checked_add(memory_size)
            .and_then(|end| end.checked_add(page - 1))
            .context("ELF memory load overflow")?
            & !(page - 1);
        loads.push(Load {
            offset: offset & !(page - 1),
            address: address & !(page - 1),
            file_end,
            end,
            memory_end,
            flags,
            zero_tail: memory_size > file_size,
        });
    }
    Ok((fixed, loads))
}

fn compare_memory(
    child: &StoppedLinuxChild<'_>,
    file: &File,
    size: u64,
    map: &Mapping,
    load: &Load,
    budget: &mut u64,
) -> Result<()> {
    let length = map.end - map.start;
    *budget = budget
        .checked_sub(length)
        .context("Linux executable-memory witness exceeds bound")?;
    let mut memory = vec![0; 65536];
    let mut expected = vec![0; memory.len()];
    let mut position = 0;
    while position < length {
        let count = usize::try_from((length - position).min(memory.len() as u64))?;
        let offset = map
            .offset
            .checked_add(position)
            .context("Linux file offset overflow")?;
        expected[..count].fill(0);
        let limit = if load.zero_tail {
            size.min(load.file_end)
        } else {
            size
        };
        let available = usize::try_from(limit.saturating_sub(offset).min(count as u64))?;
        file.read_exact_at(&mut expected[..available], offset)?;
        child.read_memory(map.start + position, &mut memory[..count])?;
        if memory[..count] != expected[..count] {
            bail!("Linux executable memory differs from pinned file bytes");
        }
        position += count as u64;
    }
    Ok(())
}

#[cfg(test)]
mod reservation_tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn frozen_reserved_gap_controls() {
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../toolchain/linux/interpreter_expectations.json"
        ))
        .unwrap();
        let mut observed = BTreeMap::new();
        for label in oracle["reserved_gap"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
        {
            let mut loads = vec![
                Load {
                    offset: 0,
                    address: 0,
                    file_end: 0x2000,
                    end: 0x2000,
                    memory_end: 0x2000,
                    flags: 5,
                    zero_tail: false,
                },
                Load {
                    offset: 0x2000,
                    address: 0x6000,
                    file_end: 0x3000,
                    end: 0x3000,
                    memory_end: 0x7000,
                    flags: 6,
                    zero_tail: false,
                },
            ];
            let mut map = parse_maps("102000-106000 ---p 00002000 08:01 7 /runtime/library\n")
                .unwrap()
                .remove(0);
            let mut bias = 0x0010_0000;
            let mut fixed = false;
            match label {
                "exact_private_reserved_gap" => {}
                "split_private_reserved_gap" => {
                    map.start = 0x0010_3000;
                    map.end = 0x0010_5000;
                    map.offset = 0x3000;
                }
                "read_gap_rejected" => map.read = true,
                "write_gap_rejected" => map.write = true,
                "execute_gap_rejected" => map.execute = true,
                "shared_gap_rejected" => map.private = false,
                "wrong_bias_gap_rejected" => bias += 0x1000,
                "wrong_offset_gap_rejected" => map.offset += 0x1000,
                "outside_gap_rejected" => {
                    map.start = 0x0010_7000;
                    map.end = 0x0010_8000;
                    map.offset = 0x7000;
                }
                "load_overlap_gap_rejected" => {
                    map.start = 0x0010_1000;
                    map.offset = 0x1000;
                }
                "bss_overlap_gap_rejected" => loads[0].memory_end = 0x3000,
                "pure_bss_reserved_overlap_rejected" => loads = pure_bss_loads(),
                "fixed_image_gap_rejected" => fixed = true,
                _ => panic!("unimplemented reserved gap control {label}"),
            }
            let accepted = reserved_gap(&loads, &map, bias, fixed);
            assert_eq!(
                accepted,
                matches!(
                    label,
                    "exact_private_reserved_gap" | "split_private_reserved_gap"
                ),
                "{label}"
            );
            assert!(observed.insert(label, accepted).is_none());
        }
        assert_eq!(observed.len(), 13);
    }

    fn pure_bss_loads() -> Vec<Load> {
        let mut bytes = vec![0; 0x3000];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
        let machine = if cfg!(target_arch = "x86_64") {
            62u16
        } else {
            183u16
        };
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&3u16.to_le_bytes());
        for (index, (offset, address, file_size, memory_size)) in [
            (0u64, 0u64, 0x2000u64, 0x2000u64),
            (0, 0x3000, 0, 0x1000),
            (0x2000, 0x6000, 0x1000, 0x1000),
        ]
        .into_iter()
        .enumerate()
        {
            let start = 64 + index * 56;
            bytes[start..start + 4].copy_from_slice(&1u32.to_le_bytes());
            bytes[start + 4..start + 8]
                .copy_from_slice(&if index == 0 { 5u32 } else { 6u32 }.to_le_bytes());
            bytes[start + 8..start + 16].copy_from_slice(&offset.to_le_bytes());
            bytes[start + 16..start + 24].copy_from_slice(&address.to_le_bytes());
            bytes[start + 32..start + 40].copy_from_slice(&file_size.to_le_bytes());
            bytes[start + 40..start + 48].copy_from_slice(&memory_size.to_le_bytes());
        }
        let mut file = tempfile::tempfile().unwrap();
        std::io::Write::write_all(&mut file, &bytes).unwrap();
        let (fixed, loads) = super::loads(&file, 4096).unwrap();
        assert!(!fixed);
        assert_eq!(loads.len(), 3);
        assert_eq!(loads[1].memory_end, 0x4000);
        assert_eq!(loads[1].end, loads[1].offset);
        loads
    }
}
