use lemmaspec::{
    project_artifact, render_projection_html, render_projection_html_with_target,
    render_projection_markdown, render_projection_markdown_with_target,
};

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

#[test]
fn answer_first_formats_share_failure_order_labels_citations_and_witnesses() {
    let projection = project_artifact(LABELLED).unwrap();
    let html = render_projection_html(LABELLED, &projection);
    let brief = html
        .split("id=\"brief\"")
        .nth(1)
        .unwrap()
        .split("</section>")
        .next()
        .unwrap();
    let md = render_projection_markdown(&projection);
    for text in [brief, md.as_str()] {
        assert!(
            text.find("inconclusive").unwrap() < text.find("Publish release is blocked").unwrap()
        );
        assert!(text.contains("Publish release waits on Review changes"));
        assert!(text.contains("reviewer-declared"));
        assert!(text.contains("plan.md#publish"));
        assert!(!text.contains("MAINTAINER ONLY"));
        assert!(!text.contains("unlabelled"));
        assert!(text.contains("Why this answer holds"));
    }
    assert!(brief.contains("href=\"plan.md#publish\""));
    assert!(md.contains("](<plan.md#publish>)"));
    assert!(brief.contains("<ol>"));
    assert!(md.contains("1. **reviewer"));
    assert!(html.contains("let fullGraph = false"));
    assert!(html.contains("nodes = nodes.filter(node => closure.has(node.id))"));
    assert!(html.contains("body.walkthrough #brief { display:none }"));
    assert!(html.contains("data-answer="));
    assert_eq!(html, render_projection_html(LABELLED, &projection));
    assert_eq!(md, render_projection_markdown(&projection));
}

#[test]
fn all_synthetic_reader_cases_have_real_witnesses_even_for_constant_queries() {
    for source in [
        include_str!("fixtures/readability/blocked_dependency.lemmaspec"),
        include_str!("fixtures/readability/accepted_plan.lemmaspec"),
        include_str!("fixtures/readability/failed_admission.lemmaspec"),
    ] {
        let projection = project_artifact(source).unwrap();
        let md = render_projection_markdown(&projection);
        assert!(md.contains("1. **reviewer"), "{md}");
        assert!(md.contains("unlabelled"));
        assert!(md.contains("**conclusion:"));
    }
}

#[test]
fn unsafe_links_and_markup_are_inert_in_both_formats() {
    for destination in [
        "javascript:alert(1)",
        "JaVaScRiPt:alert(1)",
        "data:text/html,boom",
        "//example.org",
        "http://example.org",
    ] {
        let source = LABELLED
            .replace("plan.md#publish", destination)
            .replace("Publish release", "Publish <script> & [release]");
        let projection = project_artifact(&source).unwrap();
        let html = render_projection_html(&source, &projection);
        let md = render_projection_markdown(&projection);
        assert!(!html.contains(&format!("href=\"{destination}\"")));
        assert!(!md.contains(&format!("](<{destination}>)")));
        assert!(md.contains(&format!("` {destination} `")));
        assert!(!html.contains("Publish <script>"));
        assert!(md.contains("Publish &lt;script&gt; &amp; \\[release\\]"));
    }
}

#[test]
fn empty_expectation_set_makes_no_claim_and_has_no_default_witness() {
    let source =
        "spec empty { relation item { args: [symbol] } fact one { relation: item args: [one] } }";
    let projection = project_artifact(source).unwrap();
    for report in [
        render_projection_html(source, &projection),
        render_projection_markdown(&projection),
    ] {
        assert!(report.contains("No expectations are declared"));
    }
}

#[test]
fn long_witnesses_keep_all_steps_and_recursive_proofs_terminate() {
    let mut source = String::from(
        "spec chain { relation input { args: [integer] } fact start { relation: input args: [0] } ",
    );
    for i in 0..12 {
        source.push_str(&format!("relation r{i} {{ args: [integer] }} rule derive_{i} {{ derive: \"r{i}(X)\" when: [\"{}(X)\"] }} ", if i == 0 { "input".to_string() } else { format!("r{}", i-1) }));
    }
    source.push_str("rule recursive { derive: \"r11(X)\" when: [\"r11(X)\"] } expect end { query: \"r11(0)\" count: 1 } }");
    let projection = project_artifact(&source).unwrap();
    for report in [
        render_projection_html(&source, &projection),
        render_projection_markdown(&projection),
    ] {
        assert!(report.contains("5 more steps"));
        assert!(report.contains("derive_11") || report.contains("derive\\_11"));
        assert!(report.len() < 200_000);
    }
}

#[test]
fn standing_and_freshness_use_typed_bases_only() {
    let source = r#"spec observed {
      relation item { args: [symbol] }
      fact snapshot { relation: item args: [a] basis: snapshot source: "src/a.rs#L1" identity: "tree:source" }
      fact policy { relation: item args: [b] basis: policy source: "policy:entry/rust" }
      fact observed { relation: item args: [c] basis: observed source: "probe:gate" identity: "tree:observed" }
      fact declared { relation: item args: [d] provenance: ["probe:unverified"] }
      expect all { query: "item(Item)" count: 4 }
    }"#;
    let projection = project_artifact(source).unwrap();
    for report in [
        render_projection_html_with_target(source, &projection, Some("tree:new")),
        render_projection_markdown_with_target(&projection, Some("tree:new")),
    ] {
        for text in [
            "Not current for target tree:new",
            "snapshot fact",
            "policy fact",
            "observed status",
            "reviewer-declared",
            "as of tree:observed",
        ] {
            assert!(report.contains(text), "missing {text}");
        }
    }
    let md = render_projection_markdown_with_target(&projection, Some("tree:observed"));
    assert!(!md.contains("Not current"));
    assert!(!render_projection_markdown(&projection).contains("Not current"));
}

#[test]
fn each_answer_graph_contains_its_witness_closure_without_unrelated_facts() {
    let source = LABELLED.replace(
        "  rule block",
        "  fact unrelated { relation: approved args: [review] }\n  rule block",
    );
    let projection = project_artifact(&source).unwrap();
    let html = render_projection_html(&source, &projection);
    let closures: std::collections::BTreeMap<String, Vec<String>> = serde_json::from_str(
        html.split("const answerClosures = ")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap(),
    )
    .unwrap();
    let confirmed = closures
        .iter()
        .find(|(id, _)| id.ends_with(":confirmed"))
        .unwrap()
        .1;
    let admission = closures
        .iter()
        .find(|(id, _)| id.ends_with(":admission"))
        .unwrap()
        .1;
    for node in &projection.nodes {
        match &node.data {
            lemmaspec::GraphNodeData::Fact { relation, .. }
                if relation == "waits" || relation == "blocked" =>
            {
                assert!(confirmed.contains(&node.id));
                assert!(!admission.contains(&node.id));
            }
            lemmaspec::GraphNodeData::Fact { relation, .. } if relation == "approved" => {
                assert!(!confirmed.contains(&node.id))
            }
            _ => {}
        }
    }
}

#[test]
fn labelled_dependency_demo_reads_as_an_answer_with_documented_reasoning() {
    let source = include_str!("fixtures/readability/labelled_dependency.lemmaspec");
    let projection = project_artifact(source).unwrap();
    for report in [
        render_projection_html(source, &projection),
        render_projection_markdown(&projection),
    ] {
        assert!(report.contains("Publish release is blocked by Review changes"));
        assert!(report.contains("Work must wait until its required dependency is complete"));
        assert!(!report.contains("release (unlabelled)"));
    }
}

#[test]
fn mixed_assertions_keep_each_declared_standing_and_citation() {
    let source = r#"spec mixed {
      relation item { args: [symbol] }
      fact machine { relation: item args: [a] basis: snapshot source: "src/a.rs#L1" identity: "tree:a" }
      fact human { relation: item args: [a] }
      expect item { query: "item(a)" count: 1 }
    }"#;
    let projection = project_artifact(source).unwrap();
    for report in [
        render_projection_html(source, &projection),
        render_projection_markdown(&projection),
    ] {
        assert!(report.contains("reviewer-declared / snapshot fact"));
        assert!(report.contains("src/a.rs#L1"));
        assert!(report.contains("human"));
    }
}
