//! Canonical calls must match the independently labelled source oracle.
use codemap::schema::{EdgeKind, FactSource, Language};
use codemap::{ExtractOptions, JsTsExtractConfig, extract_js_ts};
use std::collections::BTreeMap;
use std::fs;

mod common;
#[path = "support/js_ts_prepared.rs"]
mod prepared_inputs;

#[test]
#[ignore = "requires explicit pinned Darwin runtime and JS/TS compiler bundle"]
fn canonical_calls_match_frozen_pairs() {
    let root = fs::canonicalize(common::make_js_ts_calls_repo().unwrap()).unwrap();
    let config = prepared_inputs::prepared(&root);
    let environment = JsTsExtractConfig {
        roots: config.roots,
        inputs: config.inputs,
        runtime: config.runtime,
        bundle: config.bundle,
    };
    let snapshot = extract_js_ts(
        &ExtractOptions {
            path: root.clone(),
            calls_required: true,
            language: Language::JsTs,
            ..ExtractOptions::default()
        },
        "tsconfig.json",
        &environment,
    )
    .unwrap();
    let expected: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("expected.json")).unwrap()).unwrap();
    let mut actual = BTreeMap::new();
    assert_eq!(
        snapshot
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Calls)
            .count(),
        13
    );
    for edge in snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
    {
        assert_eq!(edge.provenance.fact_source, FactSource::TypeScript);
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
        let text = fs::read_to_string(root.join(&edge.provenance.rel_path)).unwrap();
        let line = text
            .lines()
            .nth((edge.provenance.span.start_line - 1) as usize)
            .unwrap();
        let label = line.split_once("// site:").unwrap().1.trim();
        actual.insert(
            label.to_owned(),
            [
                edge.provenance.rel_path.clone(),
                caller.name.clone(),
                target.provenance.rel_path.clone(),
                target.name.clone(),
            ],
        );
    }
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        expected["supported_calls"]
    );
    assert!(snapshot.identity.is_semantic());
    assert_semantic_contract(&snapshot);
    codemap::validate::validate(&snapshot).unwrap();
    if let Some(output) = std::env::var_os("CODEMAP_JS_TS_CANONICAL_SNAPSHOT") {
        let output = std::path::PathBuf::from(output);
        let parent = fs::canonicalize(output.parent().unwrap()).unwrap();
        assert!(
            !parent.starts_with(&root),
            "export stays outside analyzed source"
        );
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)
            .unwrap();
        std::io::Write::write_all(&mut file, codemap::to_json(&snapshot).unwrap().as_bytes())
            .unwrap();
    }
}

fn assert_semantic_contract(snapshot: &codemap::Snapshot) {
    assert!(
        !snapshot
            .files
            .iter()
            .any(|file| file.rel_path.to_ascii_lowercase().ends_with(".py"))
    );
    let mut invalid = snapshot.clone();
    invalid
        .identity
        .js_ts_semantic
        .as_mut()
        .unwrap()
        .evidence_sha256
        .clear();
    invalid.identity.snapshot_id = invalid.identity.compute_snapshot_id();
    assert!(codemap::validate::validate(&invalid).is_err());
    let mut invalid = snapshot.clone();
    let call = invalid
        .edges
        .iter_mut()
        .find(|edge| edge.kind == EdgeKind::Calls)
        .unwrap();
    call.provenance.fact_source = FactSource::TreeSitter;
    assert!(
        codemap::validate::validate(&invalid)
            .unwrap_err()
            .to_string()
            .contains("non-authoritative source")
    );
    let mut invalid = snapshot.clone();
    invalid
        .call_sites
        .iter_mut()
        .find(|site| site.category == codemap::schema::CallSiteCategory::Resolved)
        .unwrap()
        .category = codemap::schema::CallSiteCategory::Unsupported;
    assert!(
        codemap::validate::validate(&invalid)
            .unwrap_err()
            .to_string()
            .contains("contradicts its exact resolved call site")
    );
}

#[test]
#[ignore = "requires explicit pinned Darwin runtime and JS/TS compiler bundle"]
fn semantic_identity_is_clone_stable_and_dirty_content_addressed() {
    let mut snapshots = Vec::new();
    for _ in 0..2 {
        let root = fs::canonicalize(common::make_js_ts_calls_repo().unwrap()).unwrap();
        let helper = prepared_inputs::prepared(&root);
        let config = JsTsExtractConfig {
            roots: helper.roots,
            inputs: helper.inputs,
            runtime: helper.runtime,
            bundle: helper.bundle,
        };
        let options = ExtractOptions {
            path: root.clone(),
            language: Language::JsTs,
            calls_required: true,
            ..ExtractOptions::default()
        };
        snapshots.push(
            codemap::to_json(&extract_js_ts(&options, "tsconfig.json", &config).unwrap()).unwrap(),
        );
    }
    assert_eq!(snapshots[0], snapshots[1]);
    let root = fs::canonicalize(common::make_js_ts_calls_repo().unwrap()).unwrap();
    fs::write(
        root.join("dirty.ts"),
        "export function dirtyLeaf() {}\nexport function dirtyCaller() { dirtyLeaf(); }\n",
    )
    .unwrap();
    fs::write(
        root.join("outside-project.ts"),
        "export function outsideLeaf() {}\nexport function outsideCaller() { outsideLeaf(); }\n",
    )
    .unwrap();
    let mut project: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("tsconfig.json")).unwrap()).unwrap();
    project["exclude"] = serde_json::json!(["outside-project.ts"]);
    fs::write(
        root.join("tsconfig.json"),
        serde_json::to_vec(&project).unwrap(),
    )
    .unwrap();
    let helper = prepared_inputs::prepared(&root);
    let config = JsTsExtractConfig {
        roots: helper.roots,
        inputs: helper.inputs,
        runtime: helper.runtime,
        bundle: helper.bundle,
    };
    let options = ExtractOptions {
        path: root,
        language: Language::JsTs,
        calls_required: true,
        ..ExtractOptions::default()
    };
    let mut missing = config.clone();
    missing.inputs.files.remove("{source}/outside-project.ts");
    let error = codemap::extract_js_ts_worktree(&options, "tsconfig.json", &missing).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("inventoried JS/TS source is missing or stale")
    );
    assert!(extract_js_ts(&options, "tsconfig.json", &config).is_err());
    let dirty = codemap::extract_js_ts_worktree(&options, "tsconfig.json", &config).unwrap();
    assert!(
        dirty
            .call_sites
            .iter()
            .any(|site| site.rel_path == "outside-project.ts"
                && site.category == codemap::schema::CallSiteCategory::Unprofiled)
    );
    assert!(dirty.identity.dirty);
    assert_ne!(codemap::to_json(&dirty).unwrap(), snapshots[0]);
    let node = dirty
        .nodes
        .iter()
        .find(|node| node.name == "dirtyCaller")
        .unwrap();
    assert!(
        dirty
            .edges
            .iter()
            .any(|edge| edge.kind == EdgeKind::Calls && edge.from == node.id)
    );
    fs::write(
        options.path.join("dirty.ts"),
        "export function changed() {}\n",
    )
    .unwrap();
    assert!(codemap::extract_js_ts_worktree(&options, "tsconfig.json", &config).is_err());
}
