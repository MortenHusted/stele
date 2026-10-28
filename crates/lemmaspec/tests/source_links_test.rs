use lemmaspec::{
    bind_artifact, project_artifact, render_projection_html, render_projection_html_with_context,
    render_projection_markdown, render_projection_markdown_with_context, SourceContext,
};
use std::{fs, path::PathBuf};

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
fn context_changes_only_safe_relative_destinations() {
    let (directory, root, output) = directories("library");
    let context = SourceContext::new(&root, &output).unwrap();
    for (citation, expected) in [
        (
            "docs/plan.md#plan",
            Some("../source%20%23%25%C3%A9/docs/plan.md#plan"),
        ),
        (
            "docs/with%20space.md#plan",
            Some("../source%20%23%25%C3%A9/docs/with%20space.md#plan"),
        ),
        ("#plan", Some("#plan")),
        (
            "https://example.org/plan#section",
            Some("https://example.org/plan#section"),
        ),
        ("../secret.md", None),
        ("%2e%2e/secret.md", None),
        ("javascript:alert(1)", None),
        ("data:text/html,boom", None),
        ("//example.org", None),
        ("/absolute.md", None),
    ] {
        let source = bound(citation);
        let projection = project_artifact(&source).unwrap();
        let metadata = serde_json::to_string(&projection).unwrap();
        let html = render_projection_html_with_context(&source, &projection, None, Some(&context));
        let md = render_projection_markdown_with_context(&projection, None, Some(&context));
        if let Some(destination) = expected {
            assert!(
                html.contains(&format!("href=\"{destination}\"")),
                "{citation}"
            );
            assert!(md.contains(&format!("](<{destination}>)")), "{citation}");
            let default_html = render_projection_html(&source, &projection);
            assert_eq!(
                html.matches(&format!("href=\"{destination}\"")).count(),
                default_html
                    .matches(&format!("href=\"{citation}\""))
                    .count()
            );
        } else {
            assert!(!html.contains(&format!("href=\"{citation}\"")));
            assert!(html.contains(&format!("class=\"unsafe-source\">{citation}</span>")));
            assert!(md.contains(&format!("` {citation} `")));
        }
        assert_eq!(serde_json::to_string(&projection).unwrap(), metadata);
        assert_eq!(
            render_projection_html_with_context(&source, &projection, None, None),
            render_projection_html(&source, &projection)
        );
        assert_eq!(
            render_projection_markdown_with_context(&projection, None, None),
            render_projection_markdown(&projection)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn source_context_requires_explicit_existing_directories() {
    let (directory, root, output) = directories("invalid");
    assert!(SourceContext::new(&root.join("missing"), &output).is_err());
    fs::write(root.join("file"), "not a directory").unwrap();
    assert!(SourceContext::new(&root.join("file"), &output).is_err());
    assert!(SourceContext::new(&root, &output.join("missing")).is_err());
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn source_context_resolves_explicit_directory_symlinks() {
    let (directory, root, output) = directories("symlink");
    let alias = directory.join("links/deep/alias");
    fs::create_dir_all(alias.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&output, &alias).unwrap();
    let context = SourceContext::new(&root, &alias).unwrap();
    let source = bound("plan.md");
    let projection = project_artifact(&source).unwrap();
    let md = render_projection_markdown_with_context(&projection, None, Some(&context));
    assert!(md.contains("](<../../../source%20%23%25%C3%A9/plan.md>)"));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn legacy_provenance_is_text_while_explicit_sources_are_links() {
    let source = bound("src/main.rs#L9").replace(
        "identity: \"tree:synthetic\"",
        "identity: \"tree:synthetic\" provenance: [\"src/main.rs:9\"]",
    );
    let projection = project_artifact(&source).unwrap();
    let html = render_projection_html(&source, &projection);
    let md = render_projection_markdown(&projection);
    assert!(html.contains("src/main.rs:9"));
    assert!(!html.contains("href=\"src/main.rs:9\""));
    assert!(html.contains("href=\"src/main.rs#L9\""));
    assert!(md.contains("` src/main.rs:9 `"));
    assert!(!md.contains("](<src/main.rs:9>)"));
    assert!(md.contains("](<src/main.rs#L9>)"));
}
