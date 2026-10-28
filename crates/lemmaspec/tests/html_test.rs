use lemmaspec::{project_artifact, render_projection_html};

const EXAMPLE: &str = include_str!("../examples/release_readiness.stele");
const PERSISTENCE_ADAPTER: &str = include_str!("../examples/persistence_adapter_readiness.stele");
const PERSISTENCE_ADAPTER_HTML: &str =
    include_str!("../examples/persistence_adapter_readiness.html");
const MUTATION_ANALYSIS: &str = include_str!("../examples/mutation_analysis.stele");

#[test]
fn renders_a_dependency_free_document_with_the_canonical_graph_embedded() {
    let projection = project_artifact(EXAMPLE).expect("project example");
    let html = render_projection_html(EXAMPLE, &projection);

    assert!(html.starts_with("<!doctype html>"), "{html}");
    assert!(html.contains("Release readiness"), "{html}");
    assert!(html.contains("All 1 expectation is satisfied"), "{html}");
    assert!(html.contains("id=\"lemmaspec-graph\""), "{html}");
    assert!(html.contains("id=\"artifact-source\""), "{html}");
    assert!(html.contains("class=\"stepper\""), "{html}");
    assert!(html.contains("\"witness\":\"blocked_by_incomplete_dependency\""));
    assert!(!html.contains("<script src="), "{html}");
    assert!(!html.contains("https://"), "{html}");

    let json = between(
        &html,
        "<script type=\"application/json\" id=\"lemmaspec-graph\">",
        "</script>",
    );
    let embedded: serde_json::Value = serde_json::from_str(json).expect("embedded graph JSON");
    assert_eq!(
        embedded,
        serde_json::to_value(&projection).expect("projection JSON")
    );
}

#[test]
fn escapes_source_and_embedded_json_across_html_script_boundaries() {
    let source = r#"
spec escaping {
  relation input { args: [symbol] }
  fact hostile { relation: input args: ["</script><script>alert(1)</script>"] }
  expect input_exists { query: "input(Value)" count: 1 }
}
"#;
    let projection = project_artifact(source).expect("project hostile source");
    let html = render_projection_html(source, &projection);

    assert!(!html.contains("</script><script>alert(1)</script>"));
    assert!(html.contains("&lt;/script&gt;&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(html.contains("\\u003c/script\\u003e\\u003cscript\\u003ealert(1)"));
}

#[test]
fn repeated_html_projection_is_byte_stable() {
    let projection = project_artifact(EXAMPLE).expect("project example");

    assert_eq!(
        render_projection_html(EXAMPLE, &projection),
        render_projection_html(EXAMPLE, &projection)
    );
}

#[test]
fn renders_mutation_policies_as_stress_tests() {
    let projection = project_artifact(MUTATION_ANALYSIS).expect("project mutation analysis");
    let html = render_projection_html(MUTATION_ANALYSIS, &projection);

    assert!(html.contains("class=\"card policy\" id="), "{html}");
    assert!(html.contains("semantic_rules_are_load_bearing"), "{html}");
    assert!(
        html.contains("Dropping any rule must break at least one claim."),
        "{html}"
    );
}

#[test]
fn guides_a_reader_from_observations_to_conclusions() {
    let source = r#"
// Can the release ship today?

spec guide {
  relation depends_on {
    args: [symbol, symbol]
    roles: [item, dependency]
    reads: "{item} depends on {dependency}"
  }
  relation incomplete { args: [symbol] roles: [item] reads: "{item} is incomplete" }
  relation blocked { args: [symbol] roles: [item] reads: "{item} is blocked" }

  // Nobody has written down where this comes from.
  fact release_needs_tests { relation: depends_on args: [release, tests] }
  fact tests_are_incomplete {
    relation: incomplete
    args: [tests]
    provenance: ["plan:test-gate"]
  }

  rule blocked_by_incomplete_dependency {
    derive: "blocked(Item)"
    when: {
      has_dependency: "depends_on(Item, Dependency)"
      dependency_is_incomplete: "incomplete(Dependency)"
    }
  }

  expect release_is_blocked { query: "blocked(release)" count: 1 }
  expect nothing_else_is_blocked { query: "blocked(tests)" count: 1 }
}
"#;
    let projection = project_artifact(source).expect("project guide");
    let html = render_projection_html(source, &projection);

    assert!(
        html.contains("<div class=\"question\"><p>Can the release ship today?</p></div>"),
        "{html}"
    );

    // Freeform provenance cannot upgrade a reviewer-declared fact into an observation.
    assert!(html.contains("data-step=\"observations\""), "{html}");
    assert!(html.contains("<span class=\"chip c-attention\">reviewer-declared</span><span class=\"sentence\">tests (unlabelled) is incomplete</span>"), "{html}");
    assert!(html.contains("evidence: plan:test-gate"), "{html}");
    assert!(html.contains("data-step=\"assumptions\""), "{html}");
    assert!(html.contains("<span class=\"chip c-attention\">reviewer-declared</span><span class=\"sentence\">release (unlabelled) depends on tests (unlabelled)</span>"), "{html}");
    assert!(
        html.contains("If this is wrong, 1 conclusion and 1 claim fall with it."),
        "{html}"
    );
    assert!(
        html.contains("<p>Nobody has written down where this comes from.</p>"),
        "{html}"
    );

    // Rules read as sentences with their variables in place.
    assert!(
        html.contains("Concludes <span class=\"sentence\">Item is blocked</span> when"),
        "{html}"
    );
    assert!(
        html.contains("<span class=\"cond\">has_dependency</span>Item depends on Dependency"),
        "{html}"
    );

    // Conclusions unfold into the rule and the facts behind them.
    assert!(html.contains("data-step=\"conclusions\""), "{html}");
    assert!(html.contains("via rule <a class=\"ref\" href=\"#n-lemmaspec-guide-rule-blocked_by_incomplete_dependency\">blocked_by_incomplete_dependency</a>"), "{html}");
    assert!(
        html.contains("<span class=\"chip c-attention\">reviewer-declared</span> <a class=\"ref\""),
        "{html}"
    );

    // Open claims come first and say what was found.
    let open = html.find("nothing_else_is_blocked").expect("open claim");
    let confirmed = html.find("release_is_blocked").expect("confirmed claim");
    assert!(
        open < confirmed,
        "open claims are listed before confirmed ones"
    );
    assert!(html.contains("There must be exactly 1 result where <span class=\"sentence\">release (unlabelled) is blocked</span>."), "{html}");
    assert!(html.contains("Found 1. Confirmed."), "{html}");
    assert!(html.contains("Found 0. This claim is open"), "{html}");

    let open = r#"
spec open {
  relation loose_end { args: [symbol] roles: [item] reads: "{item} is a loose end" }
  fact one { relation: loose_end args: [popup] }
  expect no_loose_ends { query: "loose_end(X)" count: 0 }
}
"#;
    let open_html = render_projection_html(open, &project_artifact(open).expect("project open"));
    assert!(
        open_html.contains("<span class=\"why-label\">found instead</span>"),
        "{open_html}"
    );
    assert!(
        open_html.contains("popup (unlabelled) is a loose end</a>"),
        "{open_html}"
    );

    // Vocabulary groups symbols by the role they play.
    assert!(html.contains("<summary><i>item</i>"), "{html}");
    assert!(html.contains("<summary><i>dependency</i>"), "{html}");
}

#[test]
fn committed_persistence_adapter_view_matches_its_incomplete_projection() {
    let projection =
        project_artifact(PERSISTENCE_ADAPTER).expect("project persistence adapter example");

    assert_eq!(projection.status, "incomplete");

    let html = render_projection_html(PERSISTENCE_ADAPTER, &projection);
    // Archived reports retain their original presentation. Their canonical graph
    // remains the contract when the current renderer intentionally changes.
    let archived: serde_json::Value = serde_json::from_str(between(
        PERSISTENCE_ADAPTER_HTML,
        "<script type=\"application/json\" id=\"lemmaspec-graph\">",
        "</script>",
    ))
    .unwrap();
    assert_eq!(archived, serde_json::to_value(&projection).unwrap());

    let json = between(
        &html,
        "<script type=\"application/json\" id=\"lemmaspec-graph\">",
        "</script>",
    );
    let embedded: serde_json::Value = serde_json::from_str(json).expect("embedded graph JSON");
    assert_eq!(
        embedded,
        serde_json::to_value(&projection).expect("projection JSON")
    );
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let (_, tail) = text.split_once(start).expect("start marker");
    let (value, _) = tail.split_once(end).expect("end marker");
    value
}
