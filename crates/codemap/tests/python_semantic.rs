//! Actual native snapshots against independently frozen original-source labels.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::fs;
use std::path::Path;
use std::process::Command;

use codemap::ExtractOptions;
use codemap::python_semantic::{
    PythonNativeExtractConfig, extract_python_native, extract_python_native_worktree,
};
use codemap::schema::{CallSiteCategory, EdgeKind, FactSource, Language};
use serde_json::{Value, json};

#[path = "common/native.rs"]
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

fn options(path: &Path) -> ExtractOptions {
    ExtractOptions {
        path: path.to_owned(),
        calls_required: true,
        language: Language::Python,
        measures: false,
        twins: false,
    }
}

#[test]
fn native_selection_refuses_sources_outside_the_captured_git_inventory() {
    let captured = json!({
        "roots":{"runtime":"/absent-runtime"},
        "inputs":{"files":{},"directories":{}},
        "runtime":{"files":{},"cache_family":{"directory":"/absent-cache","prefix":"cache"}}
    });
    let mut config = PythonNativeExtractConfig {
        parser: serde_json::from_value(captured.clone()).unwrap(),
        native: serde_json::from_value(captured).unwrap(),
        package_profile: json!({}),
        native_sources: vec!["main.py".into()],
        declarations: vec![],
    };
    let root = repository(&json!({"files":{
        "main.py":"def main():\n    return 1\n",
        ".gitignore":"ignored.pyi\n"
    }}));
    let opts = options(root.path());
    config.native_sources = vec!["absent.py".into()];
    assert!(
        extract_python_native(&opts, &config)
            .unwrap_err()
            .to_string()
            .contains("native source absent from Python inventory")
    );
    config.native_sources = vec!["main.py".into()];
    fs::write(
        root.path().join("ignored.pyi"),
        "def ignored() -> int: ...\n",
    )
    .unwrap();
    config.declarations = vec!["ignored.pyi".into()];
    assert!(
        extract_python_native(&opts, &config)
            .unwrap_err()
            .to_string()
            .contains("Python declaration")
    );
    assert!(
        extract_python_native_worktree(&opts, &config)
            .unwrap_err()
            .to_string()
            .contains("Python declaration")
    );
    std::os::unix::fs::symlink("main.py", root.path().join("linked.pyi")).unwrap();
    git(root.path(), &["add", "linked.pyi"]);
    git(
        root.path(),
        &["commit", "-m", "synthetic declaration symlink"],
    );
    config.declarations = vec!["linked.pyi".into()];
    assert!(
        extract_python_native(&opts, &config)
            .unwrap_err()
            .to_string()
            .contains("Python declaration")
    );
}

#[test]
#[ignore = "requires explicit captured parser and native compiler/runtime configuration"]
fn native_snapshot_admits_exact_original_bodies() {
    let oracle: Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_oracle.json"
    ))
    .unwrap();
    let case = &oracle["cases"][0];
    let root = repository(case);
    let snapshot = extract_python_native(&options(root.path()), &config(case)).unwrap();
    codemap::validate(&snapshot).unwrap();
    assert!(snapshot.identity.is_semantic());
    let calls: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].provenance.fact_source, FactSource::Mypyc);
    assert_eq!(calls[0].provenance.span.start_line, 4);
    assert_eq!(snapshot.call_sites.len(), 1);
    assert_eq!(snapshot.call_sites[0].category, CallSiteCategory::Resolved);
    let caller = snapshot
        .nodes
        .iter()
        .find(|node| node.id == calls[0].from)
        .unwrap();
    let target = snapshot
        .nodes
        .iter()
        .find(|node| node.id == calls[0].to)
        .unwrap();
    assert_eq!(caller.name, "caller");
    assert_eq!(caller.provenance.span.start_line, 3);
    assert_eq!(target.name, "target");
    assert_eq!(target.provenance.span.start_line, 1);
}

#[test]
#[ignore = "requires explicit captured parser and native compiler/runtime configuration"]
fn native_snapshot_preserves_multibyte_crlf_original_coordinates() {
    let raw = "# π\r\ndef café() -> int:\r\n    return 1\r\ndef caller() -> int:\r\n    return café()\r\n";
    let case = json!({"files":{"main.py":raw}});
    let root = repository(&case);
    let snapshot = extract_python_native(&options(root.path()), &config(&case)).unwrap();
    let calls: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
        .collect();
    assert_eq!(calls.len(), 1);
    let span = calls[0].provenance.span;
    assert_eq!(span.start_byte, raw.rfind("café()").unwrap());
    assert_eq!(span.end_byte, span.start_byte + "café()".len());
    assert_eq!(
        (span.start_line, span.start_col, span.end_line, span.end_col),
        (5, 11, 5, 18)
    );
    let target = snapshot
        .nodes
        .iter()
        .find(|node| node.id == calls[0].to)
        .unwrap();
    assert_eq!(target.name, "café");
    assert_eq!(
        target.provenance.span.start_byte,
        raw.find("def café").unwrap()
    );
    assert_eq!(
        target.provenance.span.end_byte,
        raw.find("return 1").unwrap() + "return 1".len()
    );
    codemap::validate(&snapshot).unwrap();
}

#[test]
#[ignore = "requires explicit captured parser and native compiler/runtime configuration"]
fn native_snapshots_preserve_identity_selection_and_never_execute_sources() {
    let oracle: Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_oracle.json"
    ))
    .unwrap();
    let case = &oracle["cases"][0];
    let root = repository(case);
    let config = config(case);
    fs::write(root.path().join("outside.py"),
        "raise RuntimeError('application source executed')\ndef outside():\n    return forbidden()\n").unwrap();
    fs::write(
        root.path().join("unrelated.ts"),
        "const foreign = () => 1;\n",
    )
    .unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-m", "mixed source inventory"]);
    let opts = options(root.path());
    let first = extract_python_native(&opts, &config).unwrap();
    assert!(first.files.iter().any(|file| file.rel_path == "outside.py"));
    assert!(
        !first
            .files
            .iter()
            .any(|file| file.rel_path == "unrelated.ts")
    );
    assert!(first.call_sites.iter().any(
        |site| site.rel_path == "outside.py" && site.category == CallSiteCategory::Unsupported
    ));
    assert!(
        first
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Calls)
            .all(|edge| edge.provenance.rel_path != "outside.py")
    );
    let repeated = extract_python_native(&opts, &config).unwrap();
    assert_eq!(
        codemap::to_json(&first).unwrap(),
        codemap::to_json(&repeated).unwrap()
    );
    let relocated = tempfile::tempdir().unwrap();
    git(
        relocated.path(),
        &["clone", "--quiet", root.path().to_str().unwrap(), "."],
    );
    git(
        relocated.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "https://example.invalid/synthetic-python.git",
        ],
    );
    let clone = extract_python_native(&options(relocated.path()), &config).unwrap();
    assert_eq!(
        codemap::to_json(&first).unwrap(),
        codemap::to_json(&clone).unwrap()
    );
    fs::write(
        root.path().join("unrelated.ts"),
        "const foreign = () => 2;\n",
    )
    .unwrap();
    assert!(extract_python_native(&opts, &config).is_err());
    let dirty = extract_python_native_worktree(&opts, &config).unwrap();
    assert!(dirty.identity.dirty);
    assert_ne!(first.identity.tree_sha, dirty.identity.tree_sha);
    assert_eq!(first.files, dirty.files);
    assert_eq!(first.call_sites, dirty.call_sites);
    let again = extract_python_native_worktree(&opts, &config).unwrap();
    assert_eq!(
        codemap::to_json(&dirty).unwrap(),
        codemap::to_json(&again).unwrap()
    );
}

#[test]
#[ignore = "requires an explicit pinned local query and independently verified native evidence"]
fn native_snapshot_matches_pinned_original_source_bodies() {
    use base64::Engine as _;
    let read_pinned = |name: &str| {
        let raw = fs::read(std::env::var_os(name).unwrap()).unwrap();
        assert!(
            codemap::schema::sha256_hex(&raw) == std::env::var(format!("{name}_SHA256")).unwrap(),
            "pinned input hash mismatch"
        );
        raw
    };
    let request_raw = read_pinned("CODEMAP_PYTHON_NATIVE_QUERY");
    let baseline_raw = read_pinned("CODEMAP_PYTHON_NATIVE_BASELINE");
    let request: Value = serde_json::from_slice(&request_raw).unwrap();
    let baseline: Value = serde_json::from_slice(&baseline_raw).unwrap();
    let mut files = serde_json::Map::new();
    for entry in request["files"].as_array().unwrap() {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(entry["source_base64"].as_str().unwrap())
            .unwrap();
        assert!(
            codemap::schema::sha256_hex(&raw) == entry["sha256"],
            "pinned source hash mismatch"
        );
        assert!(
            files
                .insert(
                    entry["path"].as_str().unwrap().into(),
                    String::from_utf8(raw).unwrap().into()
                )
                .is_none(),
            "duplicate pinned source"
        );
    }
    let case = json!({"files":files,"package_profile":request["package_profile"]});
    let root = repository(&case);
    let snapshot = extract_python_native(&options(root.path()), &config(&case)).unwrap();
    codemap::validate(&snapshot).unwrap();
    let calls: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
        .collect();
    let evidence = baseline["evidence"].as_array().unwrap();
    assert!(
        calls.len() == evidence.len(),
        "complete pinned call count changed"
    );
    for record in evidence {
        let matching: Vec<_> = calls
            .iter()
            .filter(|edge| {
                edge.provenance.rel_path == record["path"]
                    && edge.provenance.span.start_byte as u64
                        == record["call"]["span"]["start_byte"]
                    && edge.provenance.span.end_byte as u64 == record["call"]["span"]["end_byte"]
            })
            .collect();
        assert!(
            matching.len() == 1,
            "pinned original call did not match uniquely"
        );
        let edge = matching[0];
        for (id, path, body) in [
            (&edge.from, &record["path"], &record["caller"]),
            (&edge.to, &record["target_path"], &record["target"]),
        ] {
            let node = snapshot.nodes.iter().find(|node| &node.id == id).unwrap();
            assert!(
                node.provenance.rel_path == *path
                    && node.name == body["name"]
                    && node.provenance.span.start_byte as u64 == body["span"]["start_byte"]
                    && node.provenance.span.end_byte as u64 == body["span"]["end_byte"],
                "pinned original endpoint changed"
            );
        }
    }
    assert!(
        read_pinned("CODEMAP_PYTHON_NATIVE_QUERY") == request_raw,
        "pinned query changed"
    );
    assert!(
        read_pinned("CODEMAP_PYTHON_NATIVE_BASELINE") == baseline_raw,
        "pinned evidence changed"
    );
}

#[test]
#[ignore = "requires explicit captured parser and native compiler/runtime configuration"]
fn native_snapshots_match_complete_independent_oracle() {
    let oracle: Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_oracle.json"
    ))
    .unwrap();
    let observations: Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_observation_oracle.json"
    ))
    .unwrap();
    let locations: Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_evidence_oracle.json"
    ))
    .unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        let root = repository(case);
        let snapshot = extract_python_native(&options(root.path()), &config(case)).unwrap();
        codemap::validate(&snapshot).unwrap();
        let name = case["name"].as_str().unwrap();
        let observed = observations["cases"][name].as_array().unwrap();
        assert_eq!(snapshot.call_sites.len(), observed.len(), "{name}");
        for (site, label) in snapshot.call_sites.iter().zip(observed) {
            assert_eq!(site.rel_path, label["path"], "{name}");
            assert_eq!(
                site.span.start_byte as u64, label["span"]["start_byte"],
                "{name}"
            );
            assert_eq!(
                site.span.end_byte as u64, label["span"]["end_byte"],
                "{name}"
            );
            assert_eq!(
                site.category,
                if label["status"] == "eligible_candidate" {
                    CallSiteCategory::Resolved
                } else {
                    CallSiteCategory::Unsupported
                },
                "{name}"
            );
        }
        let mut calls: Vec<_> = snapshot
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Calls)
            .collect();
        calls.sort_by_key(|edge| {
            (
                edge.provenance.rel_path.as_str(),
                edge.provenance.span.start_byte,
                edge.provenance.span.end_byte,
            )
        });
        let coordinates = locations["locations"][name].as_array();
        assert_eq!(
            calls.len(),
            case["expected_calls"].as_array().unwrap().len(),
            "{name}"
        );
        assert_eq!(calls.len(), coordinates.map_or(0, Vec::len), "{name}");
        for ((edge, location), legacy) in calls
            .iter()
            .zip(coordinates.into_iter().flatten())
            .zip(case["expected_calls"].as_array().unwrap())
        {
            let point = location.as_array().unwrap();
            assert_eq!(edge.provenance.fact_source, FactSource::Mypyc);
            assert_eq!(edge.provenance.rel_path, legacy[0]);
            let caller = snapshot
                .nodes
                .iter()
                .find(|node| node.id == edge.from)
                .unwrap();
            let target = snapshot
                .nodes
                .iter()
                .find(|node| node.id == edge.to)
                .unwrap();
            assert_eq!(caller.provenance.rel_path, legacy[0]);
            assert_eq!(target.provenance.rel_path, point[8]);
            for (actual, expected) in [
                (edge.provenance.span.start_line, &point[0]),
                (edge.provenance.span.start_col, &point[1]),
                (edge.provenance.span.end_col, &point[2]),
                (caller.provenance.span.start_line, &point[4]),
                (caller.provenance.span.start_col, &point[5]),
                (caller.provenance.span.end_line, &point[6]),
                (caller.provenance.span.end_col, &point[7]),
                (target.provenance.span.start_line, &point[9]),
                (target.provenance.span.end_line, &point[10]),
                (target.provenance.span.end_col, &point[11]),
            ] {
                assert_eq!(u64::from(actual), expected.as_u64().unwrap(), "{name}");
            }
            assert_eq!(
                target.name,
                legacy[4].as_str().unwrap().rsplit('.').next().unwrap()
            );
        }
        eprintln!("Canonical native snapshot oracle case completed: {name}");
    }
}
