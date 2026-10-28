//! Captured native calls against an independent complete original-source census.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::extract::ExtractOptions;
use crate::git;
use crate::python_native::{NativeConfig, NativeSession};
use crate::python_parser::{ParserConfig, parse_declared_sources};
use crate::schema::{
    Language, PYTHON_NATIVE_POLICY, PYTHON_PARSER_POLICY, PythonInputs, PythonNativeInputs,
    Snapshot, SnapshotIdentity, ToolVersions, sha256_hex,
};

const ADMISSION_POLICY: &str = "python-original-bodies/full-witness/closest-owner/1";
const INVENTORY_POLICY: &str = "all-py/selected-pyi/captured-tree/compiled-subset/1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonNativeExtractConfig {
    pub parser: ParserConfig,
    pub native: NativeConfig,
    pub package_profile: Value,
    pub native_sources: Vec<String>,
    pub declarations: Vec<String>,
}

pub fn extract_python_native(
    options: &ExtractOptions,
    config: &PythonNativeExtractConfig,
) -> Result<Snapshot> {
    extract(options, config, false)
}

pub fn extract_python_native_worktree(
    options: &ExtractOptions,
    config: &PythonNativeExtractConfig,
) -> Result<Snapshot> {
    extract(options, config, true)
}

fn extract(
    options: &ExtractOptions,
    config: &PythonNativeExtractConfig,
    worktree: bool,
) -> Result<Snapshot> {
    ensure!(
        options.language == Language::Python && options.calls_required,
        "Python native extraction requires --language python --calls required"
    );
    ensure!(
        !options.measures && !options.twins,
        "Python measures, clones and twins have no accepted policy"
    );
    ensure!(
        !config.native_sources.is_empty(),
        "Python native compilation unit must be explicit and nonempty"
    );
    validate_selection(&config.native_sources, "py")?;
    validate_selection(&config.declarations, "pyi")?;
    let mut state = if worktree {
        git::gather_worktree(&options.path, Language::Python)?
    } else {
        git::gather(&options.path, Language::Python)?
    };
    for path in &config.native_sources {
        ensure!(
            state.source_files.binary_search(path).is_ok(),
            "native source absent from Python inventory"
        );
    }
    let root = Path::new(&state.repo_root);
    ensure!(
        state.source_files == git::tracked_files_at(root, &state.tree_sha, Language::Python)?,
        "Python source inventory differs from captured tree"
    );
    git::require_regular_sources(
        root,
        Some(&state.tree_sha),
        &config.declarations,
        "Python declaration",
    )?;
    state
        .source_files
        .extend(config.declarations.iter().cloned());
    state.source_files.sort();
    let sources = crate::python_syntax::read_sources(&state)?;
    if state.worktree {
        let captured = git::read_files_at_bounded(
            root,
            &state.tree_sha,
            &state.source_files,
            8 * 1024 * 1024,
            32 * 1024 * 1024,
        )?;
        ensure!(
            state
                .source_files
                .iter()
                .zip(&captured)
                .all(|(path, raw)| &sources[path] == raw),
            "Python original source bytes differ from captured tree"
        );
    }
    let selected: BTreeMap<_, _> = config
        .native_sources
        .iter()
        .chain(&config.declarations)
        .map(|path| (path.clone(), sources[path].clone()))
        .collect();
    let parsed = parse_declared_sources(&config.parser, root, &sources, &config.declarations)?;
    let session = NativeSession::capture(&config.native, root)?;
    let native = session.query(&selected, &config.package_profile)?;
    let calls = crate::python_resolve::resolve(&native, &parsed, &sources)?;
    let mut tools = ToolVersions::syntax_python();
    tools.mypyc = Some("2.3.1".to_owned());
    let identity = SnapshotIdentity::build(
        Language::Python,
        state.identity.clone(),
        state.commit_sha.clone(),
        state.tree_sha.clone(),
        tools,
        state.dirty,
    )
    .with_python(PythonInputs {
        parser_profile_sha256: parsed.fingerprint,
        policy: PYTHON_PARSER_POLICY.to_owned(),
        native: Some(PythonNativeInputs {
            profile_sha256: sha256_hex(&serde_json::to_vec(
                &json!({"execution":native.fingerprint(),
                "admission":ADMISSION_POLICY,"inventory":INVENTORY_POLICY}),
            )?),
            evidence_sha256: sha256_hex(&serde_json::to_vec(native.data())?),
            policy: PYTHON_NATIVE_POLICY.to_owned(),
        }),
    });
    let snapshot =
        crate::python_syntax::assemble_native(identity, &sources, &parsed.files, &calls)?;
    git::verify_unchanged(&state)?;
    Ok(snapshot)
}

fn validate_selection(paths: &[String], extension: &str) -> Result<()> {
    ensure!(
        paths.len() <= 100_000 && paths.windows(2).all(|pair| pair[0] < pair[1]),
        "Python native source selection must be bounded, sorted and unique"
    );
    ensure!(
        paths.iter().all(|path| {
            !path.contains(['\\', '\0'])
                && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
                && Path::new(path).extension() == Some(OsStr::new(extension))
        }),
        "Python native selection requires normalized source paths of the declared kind"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PythonNativeExtractConfig {
        let captured = json!({
            "roots": {"runtime":"/absent-runtime"},
            "inputs": {"files":{},"directories":{}},
            "runtime": {"files":{},"cache_family":{"directory":"/absent-cache","prefix":"cache"}}
        });
        PythonNativeExtractConfig {
            parser: serde_json::from_value(captured.clone()).unwrap(),
            native: serde_json::from_value(captured).unwrap(),
            package_profile: json!({}),
            native_sources: vec!["main.py".into()],
            declarations: vec![],
        }
    }

    #[test]
    fn incompatible_options_fail_before_source_or_runtime_access() {
        let original = ExtractOptions {
            path: "/absent-source".into(),
            calls_required: true,
            language: Language::Python,
            measures: false,
            twins: false,
        };
        for corruption in 0..4 {
            let mut options = original.clone();
            match corruption {
                0 => options.language = Language::Rust,
                1 => options.calls_required = false,
                2 => options.measures = true,
                3 => options.twins = true,
                _ => unreachable!(),
            }
            let error = extract_python_native(&options, &config()).unwrap_err();
            assert!(error.to_string().contains(if corruption < 2 {
                "requires --language python --calls required"
            } else {
                "no accepted policy"
            }));
        }
        let mut config = config();
        config.native_sources.clear();
        assert!(
            extract_python_native(&original, &config)
                .unwrap_err()
                .to_string()
                .contains("explicit and nonempty")
        );
    }

    #[test]
    fn selections_are_exact_normalized_sorted_and_unique() {
        for paths in [
            vec!["../main.py"],
            vec!["/main.py"],
            vec!["./main.py"],
            vec!["src//main.py"],
            vec!["src\\main.py"],
            vec!["main.py\0"],
            vec!["main.pyi"],
            vec!["main.py", "main.py"],
            vec!["z.py", "a.py"],
        ] {
            let paths = paths.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(validate_selection(&paths, "py").is_err());
        }
        assert!(validate_selection(&["a.py".into(), "src/z.py".into()], "py").is_ok());
        assert!(validate_selection(&["main.pyi".into()], "pyi").is_ok());
        assert!(validate_selection(&["main.py".into()], "pyi").is_err());
    }
}
