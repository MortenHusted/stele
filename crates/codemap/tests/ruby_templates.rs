//! ERB templates are part of the Ruby family: each is a file node and the
//! call sites of its tags, at the template's own offsets, and nothing else.
use std::collections::BTreeMap;

use codemap::schema::{CallSiteCategory, Language, NodeKind, Snapshot};
use codemap::{ExtractOptions, extract};
use serde_json::json;

mod common;

const SHOW: &str = r#"<h1><%= @post.title %></h1>
<%# render "ignored" %>
<% if admin? %>
  <%= link_to "Edit", edit_post_path(@post), class: "button" -%>
<% end %>
<%= render "posts/card", post: @post %>
<%= render Card.new(title: "caf\u{e9}") %>
"#;

const FILES: [(&str, &str); 6] = [
    ("app/views/posts/show.html.erb", SHOW),
    // Tags that are not one Ruby program between them.
    (
        "app/views/posts/broken.html.erb",
        "<% if admin? %>\n<p>open</p>\n",
    ),
    // A definition in a template is counted, never emitted.
    (
        "app/views/posts/_card.html.erb",
        "<% def shout(text) = text.upcase %>\n<%= shout(post.title) %>\n",
    ),
    ("config/settings.yml.erb", "name: <%= app_name %>\n"),
    // A template is a method body: `yield` is valid in it.
    (
        "app/views/layouts/application.html.erb",
        "<head><%= yield :head %></head>\n<body><%= yield %></body>\n",
    ),
    (
        "app/models/post.rb",
        "class Post\n  def title\n    name.strip\n  end\nend\n",
    ),
];

fn snapshot(calls_required: bool, measures: bool) -> Snapshot {
    let files: Vec<_> = FILES
        .iter()
        .map(|(path, source)| ((*path).to_string(), (*source).to_string()))
        .collect();
    let snapshot = extract(&ExtractOptions {
        path: common::make_repo_with(&files).unwrap(),
        language: Language::Ruby,
        calls_required,
        measures,
        twins: false,
    })
    .unwrap();
    codemap::validate::validate(&snapshot).unwrap();
    snapshot
}

fn template(path: &str) -> bool {
    codemap::schema::is_erb_template(path)
}

#[test]
fn a_template_is_a_file_node_and_the_call_sites_of_its_tags() {
    let snapshot = snapshot(false, true);
    for node in &snapshot.nodes {
        if template(&node.provenance.rel_path) {
            assert_eq!(node.kind, NodeKind::File, "{}", node.id);
        }
    }
    assert!(snapshot.edges.iter().all(|edge| {
        !template(&edge.provenance.rel_path) || edge.from.starts_with("ruby:file:")
    }));
    assert_eq!(
        snapshot
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::File)
            .count(),
        FILES.len()
    );

    // Every site sits where the template has it.
    let path = "app/views/posts/show.html.erb";
    let sites: Vec<_> = snapshot
        .call_sites
        .iter()
        .filter(|site| site.rel_path == path)
        .collect();
    let mut names = Vec::new();
    for site in &sites {
        let written = &SHOW[site.span.start_byte..site.span.end_byte];
        assert!(written.contains(site.raw_name.as_str()), "{written}");
        assert_eq!(site.category, CallSiteCategory::Observed);
        names.push((site.span.start_line, site.raw_name.as_str()));
    }
    names.sort_unstable();
    assert_eq!(
        names,
        [
            (1, "title"),
            (3, "admin?"),
            (4, "edit_post_path"),
            (4, "link_to"),
            (6, "render"),
            (7, "new"),
            (7, "render"),
        ]
    );
    let render = sites
        .iter()
        .find(|site| site.raw_name == "render" && site.span.start_line == 6)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&render.arguments).unwrap(),
        json!([
            { "value": { "kind": "string", "value": "posts/card" } },
            { "keyword": "post", "value": { "kind": "other" } },
        ])
    );

    // Outside the Ruby sources' parse errors and outside the counting policy.
    assert_eq!(snapshot.report.parse_errors, 0);
    assert_eq!(
        snapshot.report.observations,
        BTreeMap::from([
            ("template_definitions".to_string(), 1),
            ("template_parse_errors".to_string(), 1),
        ])
    );
    assert_eq!(snapshot.file_measures.len(), 1);
    assert_eq!(
        snapshot.file_measures[0].file_id,
        "ruby:file:app/models/post.rb"
    );
}

#[test]
#[ignore = "requires the pinned Ruby-LSP operator bundle"]
fn no_resolver_reads_a_template() {
    let snapshot = snapshot(true, false);
    let mut templates = 0;
    for site in &snapshot.call_sites {
        assert_eq!(
            site.category == CallSiteCategory::Unsupported,
            template(&site.rel_path),
            "{} in {}",
            site.raw_name,
            site.rel_path
        );
        templates += usize::from(template(&site.rel_path));
    }
    assert_eq!(
        snapshot.report.call_sites_by_category["Unsupported"],
        templates
    );
    assert!(templates > 0);
    assert!(
        snapshot
            .edges
            .iter()
            .all(|edge| !template(&edge.provenance.rel_path) || edge.from.starts_with("ruby:file:"))
    );
}
