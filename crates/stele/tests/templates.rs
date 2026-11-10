//! `policy:template-render/ruby` and `policy:template-owner/ruby` over a
//! real extraction: what templates render, who owns them, and how the map
//! carries both.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use codemap::schema::{Language, Snapshot};
use codemap::{ExtractOptions, extract};
use codemap_lemmaspec::template::{Derived, owners, renders};

const INDEX: &str = r#"<%= render "card", post: post %>
<%= render partial: "shared/footer" %>
<%= render CardComponent.new(title: "x") %>
<%= render template: "posts/show" %>
<%= render @posts %>
<%= render "missing" %>
<%= render Missing::Thing.new %>
"#;

const FILES: [(&str, &str); 17] = [
    (
        "app/controllers/posts_controller.rb",
        "class PostsController\n  def index; end\n  def show; end\n  def self.helper; end\n  def secret; end\nend\n",
    ),
    (
        "app/controllers/admin/posts_controller.rb",
        "module Admin\n  class PostsController\n    def index; end\n  end\nend\n",
    ),
    ("app/views/posts/index.html.erb", INDEX),
    // No partial in this format: the only one there is answers.
    (
        "app/views/posts/index.turbo_stream.erb",
        "<%= render \"card\" %>\n",
    ),
    ("app/views/posts/_card.html.erb", "<%= post.title %>\n"),
    ("app/views/posts/show.html.erb", "<p>show</p>\n"),
    ("app/views/shared/_footer.html.erb", "<p>footer</p>\n"),
    ("app/views/shared/_dup.html.erb", "<p>one</p>\n"),
    (
        "engines/shop/app/views/shared/_dup.html.erb",
        "<p>two</p>\n",
    ),
    ("app/views/admin/posts/index.html.erb", "<p>admin</p>\n"),
    (
        "app/components/card_component.rb",
        "class CardComponent\n  def initialize(title:); end\nend\n",
    ),
    // Outside any views root, so neither `shared/_dup` is nearer.
    (
        "app/components/card_component.html.erb",
        "<%= render \"shared/dup\" %>\n",
    ),
    (
        "app/components/badge_component.rb",
        "class BadgeComponent; end\n",
    ),
    (
        "app/components/badge_component/badge_component.html.erb",
        "<span>badge</span>\n",
    ),
    (
        "app/mailers/user_mailer.rb",
        "class UserMailer\n  def welcome; end\nend\n",
    ),
    ("app/views/user_mailer/welcome.html.erb", "<p>hi</p>\n"),
    ("app/views/user_mailer/welcome.text.erb", "hi\n"),
];

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn snapshot() -> Snapshot {
    let root = tempfile::tempdir().unwrap().keep();
    git(&root, &["init", "--quiet"]);
    git(&root, &["config", "user.email", "test@example.com"]);
    git(&root, &["config", "user.name", "Test User"]);
    for (path, source) in FILES {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, source).unwrap();
    }
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "--quiet", "-m", "fixture"]);
    extract(&ExtractOptions {
        path: root,
        language: Language::Ruby,
        calls_required: false,
        measures: false,
        twins: false,
    })
    .unwrap()
}

/// (from, rule, to), with nodes by symbol key tail or file path.
fn related(snapshot: &Snapshot, derived: &Derived) -> BTreeSet<(String, &'static str, String)> {
    let name = |id: &str| {
        let node = snapshot.nodes.iter().find(|node| node.id == id).unwrap();
        node.symbol_key.split_once('#').map_or_else(
            || node.provenance.rel_path.clone(),
            |(_, name)| name.to_string(),
        )
    };
    derived
        .relations
        .iter()
        .map(|relation| (name(&relation.from), relation.rule, name(&relation.to)))
        .collect()
}

fn expected(rows: &[(&str, &'static str, &str)]) -> BTreeSet<(String, &'static str, String)> {
    rows.iter()
        .map(|(from, rule, to)| ((*from).to_string(), *rule, (*to).to_string()))
        .collect()
}

#[test]
fn a_render_relates_a_template_to_the_one_partial_template_or_class_it_names() {
    let snapshot = snapshot();
    let derived = renders(&snapshot);
    let index = "app/views/posts/index.html.erb";
    assert_eq!(
        related(&snapshot, &derived),
        expected(&[
            (index, "partial", "app/views/posts/_card.html.erb"),
            (index, "partial", "app/views/shared/_footer.html.erb"),
            (index, "component", "CardComponent"),
            (index, "template", "app/views/posts/show.html.erb"),
            (
                "app/views/posts/index.turbo_stream.erb",
                "partial",
                "app/views/posts/_card.html.erb"
            ),
        ])
    );
    assert_eq!(
        derived.unresolved,
        BTreeMap::from([
            ("ambiguous", 1),
            ("dynamic", 1),
            ("no_class", 1),
            ("no_partial", 1),
        ])
    );
    let card = derived
        .relations
        .iter()
        .find(|relation| relation.declaration == "render \"card\"")
        .unwrap();
    assert_eq!((card.rel_path.as_str(), card.lines), (index, (1, 1)));
}

#[test]
fn a_template_belongs_to_the_action_or_the_class_it_is_named_for() {
    let snapshot = snapshot();
    let derived = owners(&snapshot);
    assert_eq!(
        related(&snapshot, &derived),
        expected(&[
            (
                "PostsController::index",
                "action",
                "app/views/posts/index.html.erb"
            ),
            (
                "PostsController::index",
                "action",
                "app/views/posts/index.turbo_stream.erb"
            ),
            (
                "PostsController::show",
                "action",
                "app/views/posts/show.html.erb"
            ),
            (
                "Admin::PostsController::index",
                "action",
                "app/views/admin/posts/index.html.erb"
            ),
            (
                "UserMailer::welcome",
                "action",
                "app/views/user_mailer/welcome.html.erb"
            ),
            (
                "UserMailer::welcome",
                "action",
                "app/views/user_mailer/welcome.text.erb"
            ),
            (
                "CardComponent",
                "sidecar",
                "app/components/card_component.html.erb"
            ),
            (
                "BadgeComponent",
                "sidecar",
                "app/components/badge_component/badge_component.html.erb"
            ),
        ])
    );
    assert_eq!(derived.unresolved, BTreeMap::new());
}

#[test]
fn the_map_carries_both_template_policies_under_their_own_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("snapshot.json");
    let out = dir.path().join("topology.json");
    std::fs::write(&source, serde_json::to_vec(&snapshot()).unwrap()).unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(["topology", "--snapshot"])
        .arg(&source)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let topology: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    let policies: Vec<_> = topology["codemap"]["policies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|policy| {
            (
                policy["policy"].as_str().unwrap(),
                policy["relations"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        policies,
        [
            ("policy:template-render/ruby", 5),
            ("policy:template-owner/ruby", 8),
        ]
    );
    let relations = topology["codemap"]["policy_relations"].as_array().unwrap();
    let find = |declaration: &str| {
        relations
            .iter()
            .find(|relation| relation["declaration"] == declaration)
            .unwrap()
    };
    let card = find("render \"card\"");
    assert_eq!(card["kind"], "renders");
    assert_eq!(card["source"], "@file:app/views/posts/index.html.erb");
    assert_eq!(card["target"], "@file:app/views/posts/_card.html.erb");
    let show = find("def show");
    assert_eq!(show["kind"], "template");
    assert_eq!(
        show["source"],
        "ruby:method:app/controllers/posts_controller.rb#PostsController::show"
    );
    assert_eq!(show["target"], "@file:app/views/posts/show.html.erb");
}
