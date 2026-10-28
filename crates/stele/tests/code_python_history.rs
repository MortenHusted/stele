//! Captured CLI/history controls against independently frozen original-byte labels.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::fs;
use std::path::Path;
use std::process::Command;

use base64::Engine as _;
use codemap::schema::{EdgeKind, Language, NodeKind, sha256_hex};

#[path = "../../codemap/tests/common/python.rs"]
mod python;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn cli(binary: &Path, args: &[&str]) -> std::process::Output {
    Command::new(binary)
        .arg("code")
        .args(args)
        .output()
        .unwrap()
}

fn extract(binary: &Path, root: &Path, profile: &Path, dirty: bool) -> codemap::Snapshot {
    let mut args = vec![
        "extract",
        "--language",
        "python",
        "--path",
        root.to_str().unwrap(),
        "--python-parser",
        profile.to_str().unwrap(),
    ];
    if dirty {
        args.push("--dirty");
    }
    let output = cli(binary, &args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot = serde_json::from_slice(&output.stdout).unwrap();
    codemap::validate(&snapshot).unwrap();
    snapshot
}

#[test]
#[ignore = "requires the explicit captured Darwin parser runtime and complete operator manifest"]
#[allow(clippy::too_many_lines)] // One fixed repository lifecycle proves original bytes and every CLI/history source mode.
fn captured_cli_history_preserves_encoding_and_equivalent_checkout_identity() {
    let oracle: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/python_parser/source_encoding.json"
    ))
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    for file in oracle["files"].as_array().unwrap() {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(file["source_base64"].as_str().unwrap())
            .unwrap();
        fs::write(root.path().join(file["path"].as_str().unwrap()), raw).unwrap();
    }
    fs::write(root.path().join("empty.py"), b"").unwrap();
    fs::write(
        root.path().join("noexec.py"),
        "raise RuntimeError('never execute')\n",
    )
    .unwrap();
    for file in ["ignored.pyi", "ignored.ts", "ignored.cpp"] {
        fs::write(root.path().join(file), "invalid Python syntax\n").unwrap();
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
            "https://example.invalid/python-fixture.git",
        ],
    );
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "encoding fixture"]);
    let initial = git(root.path(), &["rev-parse", "HEAD"]);
    let private = tempfile::tempdir().unwrap();
    let binary = private.path().join("stele");
    fs::copy(env!("CARGO_BIN_EXE_stele"), &binary).unwrap();
    let config = python::config();
    let profile = private.path().join("parser.json");
    fs::write(&profile, serde_json::to_vec(&config).unwrap()).unwrap();
    let first = extract(&binary, root.path(), &profile, false);
    assert_eq!(first.identity.language, Language::Python);
    assert_eq!(first.files.len(), 7);
    assert_eq!(
        first
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Module)
            .count(),
        7
    );
    assert_eq!(first.call_sites.len(), 6);
    assert!(first.edges.iter().all(|edge| edge.kind != EdgeKind::Calls));
    for file in oracle["files"].as_array().unwrap() {
        let path = file["path"].as_str().unwrap();
        let method = first
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Method && node.provenance.rel_path == path)
            .unwrap();
        let span = method.provenance.span;
        assert_eq!(method.name, file["method_name"].as_str().unwrap());
        assert!(method.python.as_ref().unwrap().is_async);
        assert_eq!(
            span.start_byte,
            usize::try_from(file["method_start_byte"].as_u64().unwrap()).unwrap()
        );
        assert_eq!(
            span.end_byte,
            usize::try_from(file["method_end_byte"].as_u64().unwrap()).unwrap()
        );
        assert_eq!(
            u64::from(span.start_line),
            file["start_line"].as_u64().unwrap()
        );
        assert_eq!(
            u64::from(span.start_col),
            file["start_col"].as_u64().unwrap()
        );
        assert_eq!(u64::from(span.end_line), file["end_line"].as_u64().unwrap());
        assert_eq!(u64::from(span.end_col), file["end_col"].as_u64().unwrap());
        assert_eq!(
            method.provenance.file_hash,
            sha256_hex(&fs::read(root.path().join(path)).unwrap())
        );
    }
    let copy = private.path().join("clone");
    git(
        private.path(),
        &[
            "clone",
            "--no-local",
            root.path().to_str().unwrap(),
            copy.to_str().unwrap(),
        ],
    );
    git(
        &copy,
        &[
            "remote",
            "set-url",
            "origin",
            "https://example.invalid/python-fixture.git",
        ],
    );
    let equivalent = extract(&binary, &copy, &profile, false);
    assert_eq!(
        codemap::to_json(&first).unwrap(),
        codemap::to_json(&equivalent).unwrap()
    );
    let source = root.path().join("utf8-lf.py");
    fs::write(
        &source,
        format!(
            "# unrelated comment\n{}",
            fs::read_to_string(&source).unwrap()
        ),
    )
    .unwrap();
    git(root.path(), &["commit", "-qam", "line insertion"]);
    let changed = git(root.path(), &["rev-parse", "HEAD"]);
    fs::write(root.path().join("ignored.ts"), "still foreign syntax\n").unwrap();
    git(root.path(), &["commit", "-qam", "foreign change"]);
    let output = private.path().join("history");
    let result = cli(
        &binary,
        &[
            "history",
            "--language",
            "python",
            "--path",
            root.path().to_str().unwrap(),
            "--out",
            output.to_str().unwrap(),
            "--python-parser",
            profile.to_str().unwrap(),
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let commits: Vec<serde_json::Value> = fs::read_to_string(output.join("commits.ndjson"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0]["commit"], initial);
    assert_eq!(commits[1]["commit"], changed);
    assert_eq!(commits[1]["transitions"], 0);
    let transitions = fs::read_to_string(output.join("transitions.ndjson")).unwrap();
    assert!(
        transitions.lines().all(
            |line| serde_json::from_str::<serde_json::Value>(line).unwrap()["commit"] == initial
        )
    );
    assert_eq!(
        codemap::to_json(&first).unwrap(),
        codemap::to_json(
            &codemap::python_syntax::extract_python_at_commit(root.path(), &initial, &config)
                .unwrap()
        )
        .unwrap()
    );
    fs::write(
        root.path().join("untracked.py"),
        "raise RuntimeError('never execute dirty source')\n",
    )
    .unwrap();
    let dirty = extract(&binary, root.path(), &profile, true);
    assert!(dirty.identity.dirty);
    assert_eq!(dirty.files.len(), 8);
    assert!(
        !cli(
            &binary,
            &[
                "extract",
                "--language",
                "python",
                "--path",
                root.path().to_str().unwrap(),
                "--python-parser",
                profile.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    let mut missing = config;
    missing.roots.runtime = private.path().join("absent-runtime");
    fs::write(&profile, serde_json::to_vec(&missing).unwrap()).unwrap();
    let absent = private.path().join("must-not-create");
    assert!(
        !cli(
            &binary,
            &[
                "history",
                "--language",
                "python",
                "--path",
                root.path().to_str().unwrap(),
                "--out",
                absent.to_str().unwrap(),
                "--python-parser",
                profile.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    assert!(!absent.exists());
}
