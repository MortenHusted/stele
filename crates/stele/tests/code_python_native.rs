//! An actual native snapshot through `stele code extract`, against
//! independently frozen original-source labels.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::fs;
use std::path::Path;
use std::process::Command;

use codemap::python_semantic::PythonNativeExtractConfig;
use codemap::schema::{CallSiteCategory, EdgeKind};
use serde_json::{Value, json};

#[path = "../../codemap/tests/common/native.rs"]
mod native;

fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "synthetic repository command failed"
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repository(case: &Value) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for (path, raw) in case["files"].as_object().unwrap() {
        let file = root.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, raw.as_str().unwrap()).unwrap();
    }
    git(root.path(), &["init", "-b", "main"]);
    git(root.path(), &["config", "user.name", "Synthetic Fixture"]);
    git(
        root.path(),
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(
        root.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/synthetic-python.git",
        ],
    );
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-m", "synthetic source"]);
    root
}

fn config(case: &Value) -> PythonNativeExtractConfig {
    let mut sources = Vec::new();
    let mut declarations = Vec::new();
    for path in case["files"].as_object().unwrap().keys() {
        if Path::new(path).extension() == Some(std::ffi::OsStr::new("pyi")) {
            declarations.push(path.clone());
        } else {
            sources.push(path.clone());
        }
    }
    PythonNativeExtractConfig {
        parser: serde_json::from_slice(
            &fs::read(std::env::var_os("CODEMAP_PYTHON_PARSER_CONFIG").unwrap()).unwrap(),
        )
        .unwrap(),
        native: native::config(),
        package_profile: case.get("package_profile").cloned().unwrap_or_else(
            || json!({"source_roots":["."],"namespace_packages":true,"external_stub_roots":[]}),
        ),
        native_sources: sources,
        declarations,
    }
}

#[test]
#[ignore = "requires explicit captured parser and native compiler/runtime configuration"]
fn native_cli_emits_a_validated_canonical_snapshot() {
    let oracle: Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_oracle.json"
    ))
    .unwrap();
    let case = &oracle["cases"][0];
    let root = repository(case);
    let prepared = tempfile::tempdir().unwrap();
    let config_path = prepared.path().join("native.json");
    fs::write(&config_path, serde_json::to_vec(&config(case)).unwrap()).unwrap();
    let binary = prepared.path().join("stele");
    fs::copy(env!("CARGO_BIN_EXE_stele"), &binary).unwrap();
    let out = prepared.path().join("snapshot.json");
    let result = Command::new(&binary)
        .args([
            "code",
            "extract",
            "--language",
            "python",
            "--calls",
            "required",
        ])
        .arg("--path")
        .arg(root.path())
        .arg("--python-native")
        .arg(&config_path)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "actual Python native CLI extraction failed"
    );
    assert_eq!(result.stdout, [] as [u8; 0]);
    let snapshot: codemap::Snapshot = serde_json::from_slice(&fs::read(&out).unwrap()).unwrap();
    codemap::validate(&snapshot).unwrap();
    assert!(snapshot.identity.is_semantic());
    assert_eq!(
        snapshot
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Calls)
            .count(),
        1
    );
    assert_eq!(snapshot.call_sites[0].category, CallSiteCategory::Resolved);
    assert!(
        Command::new(&binary)
            .args(["code", "validate"])
            .arg("--file")
            .arg(out)
            .status()
            .unwrap()
            .success()
    );
}
