//! Complete bounded procfs map reports. Parsing is not image admission.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;

#[derive(Debug)]
pub struct Mapping {
    pub start: u64,
    pub end: u64,
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub private: bool,
    pub offset: u64,
    pub major: u32,
    pub minor: u32,
    pub inode: u64,
    pub path: Option<PathBuf>,
    pub label: Option<String>,
}

pub fn parse_maps(report: &str) -> Result<Vec<Mapping>> {
    if report.is_empty() || !report.ends_with('\n') || report.len() > 8 * 1024 * 1024 {
        bail!("incomplete or oversized Linux maps report");
    }
    let mut maps = Vec::new();
    let mut previous_end = 0;
    for line in report.lines() {
        let mut remaining = line;
        let mut field = || -> Result<&str> {
            remaining = remaining.trim_start_matches(' ');
            let length = remaining.find(' ').unwrap_or(remaining.len());
            let value = &remaining[..length];
            remaining = &remaining[length..];
            if value.is_empty() {
                bail!("missing Linux mapping field");
            }
            Ok(value)
        };
        let range = field()?;
        let (start, end) = range.split_once('-').context("malformed Linux map range")?;
        let start = hex(start)?;
        let end = hex(end)?;
        if start >= end || start < previous_end {
            bail!("unordered or overlapping Linux maps");
        }
        let permissions = field()?.as_bytes();
        if permissions.len() != 4
            || !matches!(permissions[0], b'r' | b'-')
            || !matches!(permissions[1], b'w' | b'-')
            || !matches!(permissions[2], b'x' | b'-')
            || !matches!(permissions[3], b'p' | b's')
        {
            bail!("invalid Linux map permissions");
        }
        let offset = hex(field()?)?;
        let device = field()?;
        let (major, minor) = device
            .split_once(':')
            .context("malformed Linux map device")?;
        let major = u32::try_from(hex(major)?)?;
        let minor = u32::try_from(hex(minor)?)?;
        let inode = field()?;
        if !inode.bytes().all(|byte| byte.is_ascii_digit()) {
            bail!("malformed Linux map inode");
        }
        let inode = inode.parse()?;
        let name = remaining.trim_start_matches(' ');
        let path = if name.starts_with('/') {
            if inode == 0
                || name.ends_with(" (deleted)")
                || name.contains('\\')
                || name.chars().any(char::is_control)
            {
                bail!("unverifiable Linux map path");
            }
            Some(PathBuf::from(name))
        } else {
            if inode != 0 || !(name.is_empty() || (name.starts_with('[') && name.ends_with(']'))) {
                bail!("unverifiable Linux anonymous mapping");
            }
            None
        };
        maps.push(Mapping {
            start,
            end,
            read: permissions[0] == b'r',
            write: permissions[1] == b'w',
            execute: permissions[2] == b'x',
            private: permissions[3] == b'p',
            offset,
            major,
            minor,
            inode,
            label: (path.is_none() && !name.is_empty()).then(|| name.to_owned()),
            path,
        });
        if maps.len() > 65536 {
            bail!("Linux mapping count exceeds bound");
        }
        previous_end = end;
    }
    Ok(maps)
}

fn hex(value: &str) -> Result<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("malformed Linux hexadecimal mapping field");
    }
    Ok(u64::from_str_radix(value, 16)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    #[test]
    fn frozen_parser_controls() {
        let ordinary = "1000-2000 r-xp 00000000 08:01 123 /opt/tool\n2000-3000 rw-p 00001000 08:01 123 /opt/tool\n3000-4000 rw-p 00000000 00:00 0 [heap]\n";
        let mut observed = BTreeMap::new();
        let maps = parse_maps(ordinary).unwrap();
        assert_eq!(maps.len(), 3);
        assert_eq!(maps[0].inode, 123);
        observed.insert("ordinary_maps", "accept");
        let spaced = ordinary.replace("/opt/tool", "/opt/a tool");
        assert_eq!(
            parse_maps(&spaced).unwrap()[0].path.as_deref(),
            Some(std::path::Path::new("/opt/a tool"))
        );
        observed.insert("spaces_in_path", "accept");
        for (name, bad) in [
            ("truncated_report", ordinary.trim_end().to_owned()),
            (
                "malformed_range",
                ordinary.replace("1000-2000", "2000-1000"),
            ),
            (
                "overlapping_ranges",
                ordinary.replace("2000-3000", "1fff-3000"),
            ),
            (
                "unsorted_ranges",
                ordinary.lines().rev().collect::<Vec<_>>().join("\n") + "\n",
            ),
            ("bad_permissions", ordinary.replace("r-xp", "r-x?")),
            ("bad_device", ordinary.replace("08:01", "08:q1")),
            ("bad_inode", ordinary.replace("123", "abc")),
            ("file_without_inode", ordinary.replace("123", "0")),
            ("relative_path", ordinary.replace("/opt/tool", "opt/tool")),
            (
                "deleted_path",
                ordinary.replace("/opt/tool", "/opt/tool (deleted)"),
            ),
            (
                "escaped_path",
                ordinary.replace("/opt/tool", "/opt/tool\\012name"),
            ),
            ("oversized_report", " ".repeat(8 * 1024 * 1024) + "\n"),
        ] {
            assert!(parse_maps(&bad).is_err(), "{name}");
            observed.insert(name, "reject");
        }
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../toolchain/linux/file_mapping_expectations.json"
        ))
        .unwrap();
        assert_eq!(serde_json::to_value(observed).unwrap(), oracle["parser"]);
    }
}
