//! Owned Python facts through navigation and a shared mixed-language Git tree.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;

use codemap::nav::Direction;
use codemap::python_semantic::{
    PythonNativeExtractConfig, extract_python_native, extract_python_native_worktree,
};
use codemap::schema::{EdgeKind, Language, NodeKind, Snapshot};
use codemap::{ExtractOptions, Graph};
use serde_json::json;

#[path = "common/native.rs"]
mod native;

fn git(root: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    );
}

fn options(path: &Path, language: Language) -> ExtractOptions {
    ExtractOptions {
        path: path.into(),
        calls_required: language == Language::Python,
        language,
        measures: false,
        twins: false,
    }
}

fn agree(first: &Snapshot, second: &Snapshot) {
    assert_eq!(first.identity.repository, second.identity.repository);
    assert_eq!(first.identity.commit_sha, second.identity.commit_sha);
    assert_eq!(first.identity.tree_sha, second.identity.tree_sha);
    assert_eq!(first.identity.dirty, second.identity.dirty);
    assert_ne!(first.identity.snapshot_id, second.identity.snapshot_id);
    assert!(
        first
            .nodes
            .iter()
            .all(|node| second.nodes.iter().all(|other| node.id != other.id))
    );
    codemap::validate(first).unwrap();
    codemap::validate(second).unwrap();
}

fn export_new(path: &Path, raw: &[u8]) -> std::io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(raw)
}

#[test]
fn snapshot_export_requires_a_fresh_artifact_and_preserves_input_aliases() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("input");
    fs::write(&input, b"pinned input").unwrap();
    let symlink = root.path().join("symlink");
    let hardlink = root.path().join("hardlink");
    std::os::unix::fs::symlink(&input, &symlink).unwrap();
    fs::hard_link(&input, &hardlink).unwrap();
    for path in [&input, &symlink, &hardlink] {
        assert!(export_new(path, b"snapshot").is_err());
        assert_eq!(fs::read(&input).unwrap(), b"pinned input");
    }
    let output = root.path().join("fresh");
    export_new(&output, b"snapshot").unwrap();
    assert_eq!(fs::read(output).unwrap(), b"snapshot");
}

#[test]
#[ignore = "requires explicit captured parser/native configurations; optional generic snapshot artifact"]
#[allow(clippy::too_many_lines)] // One committed fixture checks navigation and clean/dirty language identities.
fn owned_python_navigation_and_mixed_snapshots_preserve_source_identity() {
    let root = tempfile::tempdir().unwrap();
    for (path, raw) in [
        (
            "main.py",
            "def target() -> int:\n    return 1\ndef caller() -> int:\n    return target()\n",
        ),
        (
            "outside.py",
            "def outside(callback):\n    return callback()\n",
        ),
        ("declarations.pyi", "def declaration() -> int: ...\n"),
        (
            "main.cpp",
            "int target() { return 1; }\nint caller() { return target(); }\n",
        ),
        (
            "main.ts",
            "export function target(): number { return 1; }\nexport function caller(): number { return target(); }\n",
        ),
    ] {
        fs::write(root.path().join(path), raw).unwrap();
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
            "https://example.invalid/mixed-fixture.git",
        ],
    );
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-m", "mixed source fixture"]);
    let config = PythonNativeExtractConfig {
        parser: serde_json::from_slice(
            &fs::read(std::env::var_os("CODEMAP_PYTHON_PARSER_CONFIG").unwrap()).unwrap(),
        )
        .unwrap(),
        native: native::config(),
        package_profile: json!({"source_roots":["."],"namespace_packages":true,"external_stub_roots":[]}),
        native_sources: vec!["main.py".into()],
        declarations: vec!["declarations.pyi".into()],
    };
    let python = extract_python_native(&options(root.path(), Language::Python), &config).unwrap();
    let cpp = codemap::extract(&options(root.path(), Language::Cpp)).unwrap();
    let js = codemap::extract(&options(root.path(), Language::JsTs)).unwrap();
    agree(&python, &cpp);
    agree(&python, &js);
    agree(&cpp, &js);
    assert_eq!(
        cpp.files
            .iter()
            .map(|file| file.rel_path.as_str())
            .collect::<Vec<_>>(),
        ["main.cpp"]
    );
    assert_eq!(
        js.files
            .iter()
            .map(|file| file.rel_path.as_str())
            .collect::<Vec<_>>(),
        ["main.ts"]
    );
    assert_eq!(
        python
            .files
            .iter()
            .map(|file| file.rel_path.as_str())
            .collect::<Vec<_>>(),
        ["declarations.pyi", "main.py", "outside.py"]
    );
    assert!(
        python
            .nodes
            .iter()
            .filter(|node| node.provenance.rel_path == "declarations.pyi")
            .all(|node| matches!(node.kind, NodeKind::File | NodeKind::Module))
    );
    let select = |name: &str| {
        python
            .nodes
            .iter()
            .find(|node| node.name == name && node.kind == NodeKind::Function)
            .unwrap()
    };
    let caller = select("caller");
    let target = select("target");
    let graph = Graph::from_snapshot(&python);
    let found = graph.locate("main.py:4").unwrap();
    assert_eq!(found.definitions.len(), 1);
    assert_eq!(found.definitions[0].id, caller.id);
    let impacted = graph.impact(&target.id, Direction::Upstream, &[EdgeKind::Calls], 8);
    assert_eq!(impacted.len(), 1);
    assert_eq!(impacted[0].node, caller.id);
    assert_eq!(
        graph
            .trace(&caller.id, &target.id, &[EdgeKind::Calls])
            .unwrap(),
        [caller.id.clone(), target.id.clone()]
    );
    let call = python
        .edges
        .iter()
        .find(|edge| edge.kind == EdgeKind::Calls)
        .unwrap();
    assert_eq!(call.provenance.rel_path, "main.py");
    assert_eq!(call.provenance.span.start_line, 4);
    if let Some(path) = std::env::var_os("CODEMAP_PYTHON_CANONICAL_SNAPSHOT") {
        export_new(
            Path::new(&path),
            codemap::to_json(&python).unwrap().as_bytes(),
        )
        .unwrap();
    }
    fs::write(
        root.path().join("main.ts"),
        "export function different(): number { return 2; }\n",
    )
    .unwrap();
    let dirty =
        extract_python_native_worktree(&options(root.path(), Language::Python), &config).unwrap();
    let dirty_cpp = codemap::extract_worktree(&options(root.path(), Language::Cpp)).unwrap();
    let dirty_js = codemap::extract_worktree(&options(root.path(), Language::JsTs)).unwrap();
    agree(&dirty, &dirty_cpp);
    agree(&dirty, &dirty_js);
    assert!(dirty.identity.dirty);
    assert_ne!(dirty.identity.tree_sha, python.identity.tree_sha);
    assert_eq!(dirty.files, python.files);
    assert_eq!(dirty.call_sites, python.call_sites);
    assert_eq!(dirty.edges, python.edges);
}
