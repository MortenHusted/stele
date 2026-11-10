//! The stele binary carries two copies of Prism under the same symbols and
//! runs one. This holds which, on every platform the suite runs on: the
//! build its snapshots name.

use std::path::Path;
use std::process::Command;

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[test]
fn the_binary_parses_ruby_with_the_prism_its_snapshots_name() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--quiet"]);
    git(root.path(), &["config", "user.email", "test@example.com"]);
    git(root.path(), &["config", "user.name", "Test User"]);
    // Valid since Ruby 3.4; the plain Prism 2.1.0 release rejects it.
    std::fs::write(root.path().join("probe.rb"), "foo(bar baz do end)\n").unwrap();
    std::fs::write(root.path().join("layout.html.erb"), "<%= yield %>\n").unwrap();
    git(root.path(), &["add", "-A"]);
    git(root.path(), &["commit", "--quiet", "-m", "probe"]);
    let out = root.path().join("snapshot.json");
    let run = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(["code", "extract", "--language", "ruby", "--path"])
        .arg(root.path())
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    let tools = &snapshot["identity"]["tools"];
    assert_eq!(tools["ruby_parser"], "prism 2.1.0+4691667");
    assert_eq!(tools["erb_parser"], "herb 0.11.2");
    assert_eq!(snapshot["report"]["parse_errors"], 0);
    assert!(snapshot["report"].get("observations").is_none());
}
