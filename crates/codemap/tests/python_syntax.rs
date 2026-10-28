//! Actual captured parser-to-snapshot controls; target sources are never executed.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use codemap::python_syntax::{extract_python, extract_python_at_commit, extract_python_worktree};
use codemap::schema::{EdgeKind, FactSource, Language, NodeKind};
use codemap::{ExtractOptions, Snapshot};

#[path = "common/python.rs"]
mod python;

fn git(path: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

fn keys(snapshot: &Snapshot) -> BTreeSet<&str> {
    snapshot
        .nodes
        .iter()
        .map(|node| node.symbol_key.as_str())
        .collect()
}

#[test]
#[ignore = "requires the explicit captured Darwin parser runtime and complete operator manifest"]
#[allow(clippy::too_many_lines)] // One committed fixture checks clean, historical and dirty source identities.
fn captured_python_structure_preserves_keys_inventory_and_commit_bytes() {
    let root = tempfile::tempdir().unwrap();
    let fixture: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/python_parser/lexical_ownership.json"
    ))
    .unwrap();
    let raw = fixture["source"].as_str().unwrap();
    fs::write(root.path().join("main.py"), raw).unwrap();
    for path in ["ignored.pyi", "ignored.js", "ignored.cpp", "ignored.PY"] {
        fs::write(root.path().join(path), "not valid Python\n").unwrap();
    }
    git(root.path(), &["init", "-b", "main"]);
    git(root.path(), &["config", "user.name", "Synthetic Fixture"]);
    git(
        root.path(),
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-m", "initial"]);
    let initial = git(root.path(), &["rev-parse", "HEAD"]);
    let options = ExtractOptions {
        path: root.path().to_owned(),
        calls_required: false,
        language: Language::Python,
        measures: false,
        twins: false,
    };
    let config = python::config();
    let first = extract_python(&options, &config).unwrap();
    assert_eq!(first.files.len(), 1);
    assert_eq!(first.files[0].rel_path, "main.py");
    assert_eq!(
        first
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Module)
            .count(),
        1
    );
    assert!(!first.identity.is_semantic());
    assert_eq!(
        first.call_sites.len(),
        fixture["call_owners"].as_object().unwrap().len()
    );
    assert!(first.edges.iter().all(|edge| edge.kind != EdgeKind::Calls));
    assert!(
        first
            .nodes
            .iter()
            .all(|node| node.provenance.fact_source == FactSource::Cpython)
    );
    let method = first
        .nodes
        .iter()
        .find(|node| node.name == "method")
        .unwrap();
    assert_eq!(method.kind, NodeKind::Method);
    assert!(method.python.as_ref().unwrap().is_async);
    let import = first
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::ImportRef)
        .unwrap();
    let parent = first
        .edges
        .iter()
        .find(|edge| edge.kind == EdgeKind::Contains && edge.to == import.id)
        .unwrap();
    assert_eq!(
        first
            .nodes
            .iter()
            .find(|node| node.id == parent.from)
            .unwrap()
            .name,
        "Outer"
    );
    assert_eq!(keys(&first).len(), first.nodes.len());
    fs::write(
        root.path().join("main.py"),
        format!("# unrelated comment\n\n{raw}"),
    )
    .unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-m", "insert comment"]);
    let second = extract_python(&options, &config).unwrap();
    assert_eq!(keys(&first), keys(&second));
    assert_ne!(
        method.id,
        second
            .nodes
            .iter()
            .find(|node| node.name == "method")
            .unwrap()
            .id
    );
    let historical = extract_python_at_commit(root.path(), &initial, &config).unwrap();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(historical).unwrap()
    );
    fs::write(
        root.path().join("dirty.py"),
        "raise RuntimeError('never execute')\n",
    )
    .unwrap();
    let dirty = extract_python_worktree(&options, &config).unwrap();
    assert!(dirty.identity.dirty);
    assert_eq!(dirty.files.len(), 2);
    assert_eq!(dirty.identity.commit_sha, second.identity.commit_sha);
    assert_ne!(dirty.identity.tree_sha, second.identity.tree_sha);
    assert!(extract_python(&options, &config).is_err());
}
