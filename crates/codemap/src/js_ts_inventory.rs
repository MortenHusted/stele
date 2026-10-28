//! Explicit repository source exclusions, read from the same tree as sources.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::git;

pub const POLICY_PATH: &str = ".codemap-js-ts.json";

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    #[serde(default)]
    exclude_directories: Vec<String>,
}

pub fn dependency_path(path: &str) -> bool {
    path.rsplit_once('/').is_some_and(|(directories, _)| {
        directories
            .split('/')
            .any(|part| part == "node_modules" || part == ".yarn")
    })
}

pub fn filter(root: &Path, commit: Option<&str>, files: &mut Vec<String>) -> Result<()> {
    let modes = commit
        .map(|commit| -> Result<BTreeMap<String, String>> {
            let tree = git::run(root, &["ls-tree", "-r", "-z", commit])?;
            tree.split('\0')
                .filter(|entry| !entry.is_empty())
                .map(|entry| {
                    let (metadata, path) = entry.split_once('\t').context("Git tree entry")?;
                    Ok((
                        path.to_owned(),
                        metadata
                            .split(' ')
                            .next()
                            .context("Git file mode")?
                            .to_owned(),
                    ))
                })
                .collect()
        })
        .transpose()?;
    let bytes = if let Some(modes) = &modes {
        if let Some(mode) = modes.get(POLICY_PATH) {
            ensure!(
                regular_mode(mode),
                "JS/TS source policy must be a regular file"
            );
            Some(git::read_file_at(
                root,
                commit.expect("committed modes"),
                POLICY_PATH,
            )?)
        } else {
            None
        }
    } else {
        let paths = git::run(
            root,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
                "--",
                POLICY_PATH,
            ],
        )?;
        if paths.split('\0').any(|path| path == POLICY_PATH) {
            working_policy(root)?
        } else {
            None
        }
    };
    let policy = if let Some(bytes) = bytes {
        ensure!(bytes.len() <= 64 * 1024, "oversized JS/TS source policy");
        serde_json::from_slice(&bytes).context("parse JS/TS source policy")?
    } else {
        Policy::default()
    };
    for directory in &policy.exclude_directories {
        ensure!(
            !directory.is_empty()
                && !directory.starts_with('/')
                && !directory.contains('\\')
                && directory
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != ".."),
            "JS/TS exclusions must be normalized relative directory paths"
        );
    }
    files.retain(|file| {
        !policy.exclude_directories.iter().any(|directory| {
            file.strip_prefix(directory)
                .is_some_and(|suffix| suffix.starts_with('/'))
        })
    });
    if let Some(modes) = modes {
        for file in files.iter() {
            ensure!(
                modes.get(file).is_some_and(|mode| regular_mode(mode)),
                "JS/TS source must be a regular Git blob: {file}"
            );
        }
    } else {
        let root = root.canonicalize()?;
        for file in files.iter() {
            let path = root.join(file);
            ensure!(
                path.canonicalize()? == path && std::fs::symlink_metadata(&path)?.is_file(),
                "JS/TS source must be a regular file without linked ancestors: {file}"
            );
        }
    }
    Ok(())
}

fn regular_mode(mode: &str) -> bool {
    matches!(mode, "100644" | "100755")
}

fn working_policy(root: &Path) -> Result<Option<Vec<u8>>> {
    let path = root.join(POLICY_PATH);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect JS/TS source policy"),
    };
    ensure!(
        metadata.is_file(),
        "JS/TS source policy must be a regular file"
    );
    Ok(Some(std::fs::read(path)?))
}
