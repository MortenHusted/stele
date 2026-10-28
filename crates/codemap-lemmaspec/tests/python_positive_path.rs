//! Python consumer contracts; handbuilt fixtures do not attest extraction.
mod common;

use codemap_graph::schema::{CallSiteCategory, EdgeKind, NodeKind, Snapshot};
use codemap_lemmaspec::positive_path::{Options, project};

fn selection(snapshot: &Snapshot, path: &str) -> Options {
    let select = |name: &str| {
        let nodes: Vec<_> = snapshot
            .nodes
            .iter()
            .filter(|node| {
                node.name == name
                    && node.provenance.rel_path == path
                    && node.kind == NodeKind::Function
            })
            .collect();
        assert_eq!(nodes.len(), 1);
        nodes[0].id.clone()
    };
    Options {
        entry_node: select("caller"),
        target_node: select("target"),
        allow_dirty: false,
    }
}

#[test]
fn positive_python_paths_preserve_provenance_and_explicit_selections() {
    let snapshot = common::python_snapshot();
    let options = selection(&snapshot, "src/main.py");
    let output = project(&snapshot, &options).unwrap();
    for expected in [
        "python-positive-path/v1",
        "codemap_python_positive_path",
        "basis: snapshot",
        "basis: reviewer_declared",
        "runtime reachability and whole-program completeness remain unknown",
        "src/main.py:1",
        &snapshot.identity.snapshot_id,
        &options.entry_node,
        &options.target_node,
    ] {
        assert!(output.contains(expected), "missing {expected}");
    }
    assert_eq!(output, project(&snapshot, &options).unwrap());
    let categories: Vec<_> =
        codemap_lemmaspec::coverage::categories(snapshot.identity.language).collect();
    assert!(categories.contains(&CallSiteCategory::Unsupported));
    assert!(!categories.contains(&CallSiteCategory::Inactive));
    assert!(!categories.contains(&CallSiteCategory::Unprofiled));
}

#[test]
fn python_paths_refuse_syntax_dirty_stale_and_contradictory_inputs() {
    let snapshot = common::python_snapshot();
    let options = selection(&snapshot, "src/main.py");
    for id in ["caller", "stale-id", snapshot.nodes[0].id.as_str()] {
        let invalid = Options {
            entry_node: id.into(),
            ..options.clone()
        };
        assert!(
            project(&snapshot, &invalid)
                .unwrap_err()
                .to_string()
                .contains("exact callable node ID")
        );
    }
    let mut dirty = snapshot.clone();
    dirty.identity.dirty = true;
    dirty.identity.snapshot_id = dirty.identity.compute_snapshot_id();
    assert!(
        project(&dirty, &options)
            .unwrap_err()
            .to_string()
            .contains("--allow-dirty")
    );
    project(
        &dirty,
        &Options {
            allow_dirty: true,
            ..options.clone()
        },
    )
    .unwrap();
    let mut syntax = snapshot.clone();
    syntax.identity.python.as_mut().unwrap().native = None;
    syntax.identity.tools.mypyc = None;
    syntax.identity.snapshot_id = syntax.identity.compute_snapshot_id();
    syntax.edges.retain(|edge| edge.kind != EdgeKind::Calls);
    for site in &mut syntax.call_sites {
        site.category = CallSiteCategory::Observed;
    }
    syntax.report = common::report_for(&syntax);
    codemap_graph::validate(&syntax).unwrap();
    assert!(
        project(&syntax, &options)
            .unwrap_err()
            .to_string()
            .contains("validated semantic")
    );
    let mut invalid = snapshot;
    invalid.call_sites[0].category = CallSiteCategory::Unsupported;
    invalid.report = common::report_for(&invalid);
    assert!(project(&invalid, &options).is_err());
}

#[test]
#[ignore = "requires an owned native synthetic snapshot"]
fn owned_python_snapshot_proves_path_and_removed_calls_lose_proof() {
    let path = std::env::var_os("CODEMAP_PYTHON_CANONICAL_SNAPSHOT").unwrap();
    let mut snapshot: Snapshot = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    codemap_graph::validate(&snapshot).unwrap();
    let options = selection(&snapshot, "main.py");
    let root = tempfile::tempdir().unwrap();
    let artifact = root.path().join("python.lemmaspec");
    std::fs::write(&artifact, project(&snapshot, &options).unwrap()).unwrap();
    let (code, report) = common::run_lemmaspec(&["walk", artifact.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0, "{report}");
    snapshot.edges.retain(|edge| edge.kind != EdgeKind::Calls);
    for site in &mut snapshot.call_sites {
        if site.category == CallSiteCategory::Resolved {
            site.category = CallSiteCategory::Unsupported;
        }
    }
    snapshot.report = common::report_for(&snapshot);
    std::fs::write(&artifact, project(&snapshot, &options).unwrap()).unwrap();
    let (code, report) = common::run_lemmaspec(&["walk", artifact.to_str().unwrap(), "--json"]);
    assert_eq!(
        code, 1,
        "removing canonical calls removes the positive proof: {report}"
    );
}
