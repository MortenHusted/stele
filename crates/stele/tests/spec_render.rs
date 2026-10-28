//! `stele spec render` writes its reports where it is asked to and keeps the
//! engine's exit status.

use std::{fs, path::PathBuf, process::Command};

use lemmaspec::{
    bind_artifact, project_artifact, render_projection_html, render_projection_markdown,
};

/// `stele spec`, run from the test's current directory.
fn spec() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_stele"));
    command.arg("spec");
    command
}

const LABELLED: &str = r#"
// What can be published?
spec readable {
  notes { text: "MAINTAINER ONLY: mutate the verification rule." }
  symbol release { label: "Publish release" source: "plan.md#publish" }
  symbol review { label: "Review changes" source: "https://example.org/review" }
  relation waits { args: [symbol, symbol] roles: [item, dependency] reads: "{item} waits on {dependency}" }
  relation blocked { args: [symbol] roles: [item] reads: "{item} is blocked" }
  relation approved { args: [symbol] }
  fact dependency { relation: waits args: [release, review] provenance: ["plan.md#dependency"] }
  rule block { derive: "blocked(Item)" when: ["waits(Item, Dependency)"] }
  // Publication is conditional on completing review.
  expect confirmed { query: "blocked(release)" count: 1 }
  // Admission is inconclusive until approval evidence is supplied.
  expect admission { query: "approved(release)" count: 1 }
}
"#;

const CHECKER: &str = r#"spec checker {
  relation item { args: [symbol] }
  fact fixture { relation: item args: [fixture] }
  expect fixture { query: "item(fixture)" count: 1 }
}"#;

fn bound(source: &str) -> String {
    bind_artifact(CHECKER, &format!(r#"spec evidence {{
      symbol item {{ label: "The item" source: "{source}" }}
      fact item {{ relation: item args: [item] basis: snapshot source: "{source}" identity: "tree:synthetic" }}
      expect item {{ query: "item(item)" count: 1 }}
    }}"#)).unwrap()
}

fn directories(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let directory = std::env::temp_dir().join(format!(
        "lemmaspec-source-links-{name}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    #[cfg(not(windows))]
    let directory = directory.canonicalize().unwrap();
    let root = directory.join("source #%é");
    let output = directory.join("reports");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&output).unwrap();
    (directory, root, output)
}

#[test]
fn markdown_cli_writes_report_and_preserves_nonzero_status() {
    let dir = std::env::temp_dir().join(format!("lemmaspec-report-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.lemmaspec");
    std::fs::write(&input, LABELLED).unwrap();
    let output = spec()
        .args(["render", input.to_str().unwrap(), "--format", "md"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        std::fs::read_to_string(input.with_extension("md")).unwrap(),
        render_projection_markdown(&project_artifact(LABELLED).unwrap())
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn freshness_mismatch_cli_writes_the_report_and_returns_failure() {
    let dir = std::env::temp_dir().join(format!("lemmaspec-freshness-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.lemmaspec");
    let source = r#"spec identity {
      relation ready { args: [symbol] }
      fact ready { relation: ready args: [release] basis: observed source: "probe:release" identity: "tree:old" }
      expect ready { query: "ready(release)" count: 1 }
    }"#;
    std::fs::write(&input, source).unwrap();
    for (target, status) in [("tree:old", 0), ("tree:new", 1)] {
        let output = spec()
            .args([
                "render",
                input.to_str().unwrap(),
                "--format",
                "md",
                "--target-identity",
                target,
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(status));
        let report = std::fs::read_to_string(input.with_extension("md")).unwrap();
        assert_eq!(report.contains("Not current"), status == 1);
        assert!(report.contains("Status: clean"));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn explicit_source_root_relocates_bound_report_links() {
    let (directory, root, output) = directories("cli");
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("docs/plan.md"), "# Plan\n").unwrap();
    let input = output.join("bound.lemmaspec");
    let source = bound("docs/plan.md#plan");
    fs::write(&input, &source).unwrap();
    let projection = project_artifact(&source).unwrap();
    for format in ["html", "md"] {
        let result = spec()
            .current_dir(&directory)
            .args([
                "render",
                "reports/bound.lemmaspec",
                "--format",
                format,
                "--source-root",
                "source #%é",
                "-o",
            ])
            .arg(output.join(format!("nested/report.{format}")))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report = fs::read_to_string(output.join(format!("nested/report.{format}"))).unwrap();
        let destination = "../../source%20%23%25%C3%A9/docs/plan.md#plan";
        if format == "html" {
            assert!(report.contains(&format!("href=\"{destination}\">docs/plan.md#plan</a>")));
            assert!(report.contains("\"source\":\"docs/plan.md#plan\""));
        } else {
            assert!(report.contains(&format!("[docs/plan\\.md\\#plan](<{destination}>)")));
        }
        let default = spec()
            .args(["render", input.to_str().unwrap(), "--format", format])
            .output()
            .unwrap();
        assert!(default.status.success());
        let default_report = fs::read_to_string(input.with_extension(format)).unwrap();
        let expected = if format == "html" {
            render_projection_html(&source, &projection)
        } else {
            render_projection_markdown(&projection)
        };
        assert_eq!(default_report, expected);
    }
    assert_eq!(fs::read_to_string(input).unwrap(), source);
    fs::remove_dir_all(directory).unwrap();
}
