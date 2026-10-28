//! `stele topology` writes deterministic JSON and offline HTML, refuses
//! colliding destinations, and relocates source links; its liveness input
//! is what `stele code evidence` and `stele spec check` produce.

use std::path::PathBuf;

use codemap_graph::Snapshot;
use codemap_topology::project;

/// The synthetic snapshots, check reports and selections of the topology
/// crate's own tests.
fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../codemap-topology/tests/fixtures")
}

fn snapshot(language: &str) -> Snapshot {
    serde_json::from_slice(&std::fs::read(fixtures().join(format!("{language}.json"))).unwrap())
        .unwrap()
}

fn fixture_value(language: &str, suffix: &str) -> serde_json::Value {
    serde_json::from_slice(
        &std::fs::read(fixtures().join(format!("{language}-{suffix}.json"))).unwrap(),
    )
    .unwrap()
}

fn checked(
    source: &Snapshot,
    report: serde_json::Value,
    selection: serde_json::Value,
) -> anyhow::Result<codemap_topology::Topology> {
    project(
        source,
        Some((
            &serde_json::from_value(report)?,
            &serde_json::from_value(selection)?,
        )),
    )
}

#[test]
fn cli_writes_deterministic_json_and_offline_html_and_rejects_bad_binding() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = fixtures();
    let out = dir.path().join("topology.json");
    let html = dir.path().join("TOPOLOGY.html");
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_stele"))
            .args(["topology", "--snapshot"])
            .arg(fixtures.join("rust.json"))
            .arg("--liveness")
            .arg(fixtures.join("rust-check.json"))
            .arg("--selection")
            .arg(fixtures.join("rust-selection.json"))
            .arg("--out")
            .arg(&out)
            .arg("--html")
            .arg(&html)
            .output()
            .unwrap()
    };
    assert!(run().status.success());
    let first_json = std::fs::read(&out).unwrap();
    let first_html = std::fs::read(&html).unwrap();
    assert!(run().status.success());
    assert_eq!(first_json, std::fs::read(&out).unwrap());
    assert_eq!(first_html, std::fs::read(&html).unwrap());
    let failed = std::process::Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(["topology", "--snapshot"])
        .arg(fixtures.join("rust.json"))
        .arg("--liveness")
        .arg(fixtures.join("ruby-check.json"))
        .arg("--selection")
        .arg(fixtures.join("rust-selection.json"))
        .arg("--out")
        .arg(&out)
        .arg("--html")
        .arg(&html)
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert_eq!(first_json, std::fs::read(out).unwrap());
    assert_eq!(first_html, std::fs::read(html).unwrap());
}

#[test]
fn live_projection_and_checker_workflow_matches_the_synthetic_reports() {
    for language in ["rust", "ruby", "php", "go"] {
        let dir = tempfile::tempdir().unwrap();
        let fixtures = fixtures();
        let source = snapshot(language);
        let entry = source.nodes.iter().find(|n| n.name == "entry").unwrap();
        let evidence = dir.path().join("evidence.lemmaspec");
        let projection = std::process::Command::new(env!("CARGO_BIN_EXE_stele"))
            .args([
                "code",
                "evidence",
                "--checker",
                "symbols",
                "--min-call-coverage-per-mille",
                "500",
                "--entry",
                &entry.symbol_key,
            ])
            .arg("--file")
            .arg(fixtures.join(format!("{language}.json")))
            .arg("--out")
            .arg(&evidence)
            .output()
            .unwrap();
        assert!(
            projection.status.success(),
            "{}",
            String::from_utf8_lossy(&projection.stderr)
        );
        let check = std::process::Command::new(env!("CARGO_BIN_EXE_stele"))
            .args(["spec", "check"])
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../codemap-lemmaspec/checkers/symbols.stele"),
            )
            .arg(evidence)
            .arg("--json")
            .output()
            .unwrap();
        assert_eq!(check.status.code(), Some(1));
        let report: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
        assert_eq!(report, fixture_value(language, "check"));
        assert_eq!(
            checked(&source, report, fixture_value(language, "selection"))
                .unwrap()
                .dead_ends
                .len(),
            1
        );
    }
}

#[test]
fn cli_rejects_colliding_destinations_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot_path = dir.path().join("snapshot.json");
    let snapshot_bytes = serde_json::to_vec(&snapshot("rust")).unwrap();
    std::fs::write(&snapshot_path, &snapshot_bytes).unwrap();
    let out = dir.path().join("out");
    std::fs::write(&out, b"keep").unwrap();
    let reject = |json: &std::path::Path, html: &std::path::Path| {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_stele"))
            .args(["topology", "--snapshot"])
            .arg(&snapshot_path)
            .arg("--out")
            .arg(json)
            .arg("--html")
            .arg(html)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert_eq!(std::fs::read(&out).unwrap(), b"keep");
        assert_eq!(std::fs::read(&snapshot_path).unwrap(), snapshot_bytes);
    };
    reject(&out, &out);
    reject(&snapshot_path, &out);
    #[cfg(unix)]
    {
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
        reject(&out, &alias.join("out"));
        reject(&out, &alias.join("snapshot.json"));
    }
}

#[test]
fn cli_relocates_source_links_without_changing_topology_json() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let source = root.join("source # 100%");
    let reports = root.join("reports");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&reports).unwrap();
    let snapshot_path = root.join("snapshot.json");
    std::fs::write(
        &snapshot_path,
        serde_json::to_vec(&snapshot("rust")).unwrap(),
    )
    .unwrap();
    let out = reports.join("topology.json");
    let html = reports.join("TOPOLOGY.html");
    let run = |root: Option<&std::path::Path>| {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_stele"));
        command
            .args(["topology", "--snapshot"])
            .arg(&snapshot_path)
            .arg("--out")
            .arg(&out)
            .arg("--html")
            .arg(&html);
        if let Some(root) = root {
            command.arg("--source-root").arg(root);
        }
        command.output().unwrap()
    };
    assert!(run(None).status.success());
    let json = std::fs::read(&out).unwrap();
    let result = run(Some(&source));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(json, std::fs::read(&out).unwrap());
    let page = std::fs::read_to_string(&html).unwrap();
    assert!(page.contains("/*__SOURCE_ROOT__*/ \"../source%20%23%20100%25/\""));
    #[cfg(unix)]
    {
        let nested = root.join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::os::unix::fs::symlink(&reports, nested.join("alias")).unwrap();
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_stele"))
            .args(["topology", "--snapshot"])
            .arg(&snapshot_path)
            .arg("--out")
            .arg(&out)
            .arg("--html")
            .arg(nested.join("alias/TOPOLOGY.html"))
            .arg("--source-root")
            .arg(&source)
            .output()
            .unwrap();
        assert!(result.status.success());
        let page = std::fs::read_to_string(&html).unwrap();
        assert!(page.contains("/*__SOURCE_ROOT__*/ \"../../source%20%23%20100%25/\""));
    }
    assert!(!run(Some(&snapshot_path)).status.success());
    assert_eq!(json, std::fs::read(&out).unwrap());
}
