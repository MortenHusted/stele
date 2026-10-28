//! R1 Ruby adapter gates: language purity (bidirectional), structural
//! correctness of namespaced facts, syntax-only call discipline, and
//! byte determinism across independent checkouts.

use std::collections::BTreeSet;
use std::path::PathBuf;

use codemap::schema::{EdgeKind, Language, NodeKind};
use codemap::{ExtractOptions, extract, to_json};

mod common;

fn extract_ruby(repo: &std::path::Path) -> codemap::Snapshot {
    extract(&ExtractOptions {
        path: PathBuf::from(repo),
        calls_required: false,
        measures: false,
        twins: false,
        language: Language::Ruby,
    })
    .expect("ruby extraction")
}

#[test]
fn only_ruby_files_produce_facts() {
    let repo = common::make_repo_with(&common::ruby_corpus_files()).expect("create ruby repo");
    let snapshot = extract_ruby(&repo);

    for node in &snapshot.nodes {
        assert!(
            node.id.starts_with("ruby:"),
            "non-ruby node id: {}",
            node.id
        );
    }
    for file in &snapshot.files {
        assert!(
            std::path::Path::new(&file.rel_path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("rb")),
            "non-Ruby file in ruby snapshot: {}",
            file.rel_path
        );
    }
    for node in &snapshot.nodes {
        assert!(
            !node.provenance.rel_path.ends_with("helper.rs")
                && !node.provenance.rel_path.ends_with("helper.ts")
                && !node.provenance.rel_path.ends_with("helper.php")
                && !node.provenance.rel_path.ends_with("helper.go"),
            "cross-language provenance leak: {}",
            node.provenance.rel_path
        );
    }
    assert_eq!(snapshot.identity.language, Language::Ruby);
}

#[test]
fn ruby_structure_matches_the_labelled_corpus() {
    let repo = common::make_repo_with(&common::ruby_corpus_files()).expect("create ruby repo");
    let snapshot = extract_ruby(&repo);

    let ids: BTreeSet<&str> = snapshot.nodes.iter().map(|n| n.id.as_str()).collect();
    for expected in [
        "ruby:file:app/models/user.rb",
        "ruby:file:app/components/banner_component.rb",
        "ruby:type:app/components/banner_component.rb#BannerComponent",
        "ruby:method:app/components/banner_component.rb#BannerComponent::render",
        "ruby:type:app/models/record.rb#Record",
        "ruby:method:app/models/record.rb#Record::persist",
        "ruby:module:app/models/auditable.rb#Auditable",
        "ruby:method:app/models/auditable.rb#Auditable::audit_log",
        "ruby:type:app/models/user.rb#User",
        "ruby:method:app/models/user.rb#User::save",
        "ruby:method:app/models/user.rb#User::self.find",
        "ruby:module:app/controllers/users_controller.rb#Controllers",
        "ruby:type:app/controllers/users_controller.rb#Controllers::UsersController",
        "ruby:method:app/controllers/users_controller.rb#Controllers::UsersController::index",
    ] {
        assert!(ids.contains(expected), "missing node {expected}");
    }

    // Class and module kinds.
    let user = snapshot
        .nodes
        .iter()
        .find(|n| n.id == "ruby:type:app/models/user.rb#User")
        .expect("User node");
    assert_eq!(user.kind, NodeKind::Type(codemap::schema::TypeKind::Class));
    let controllers = snapshot
        .nodes
        .iter()
        .find(|n| n.id == "ruby:module:app/controllers/users_controller.rb#Controllers")
        .expect("Controllers node");
    assert_eq!(controllers.kind, NodeKind::Module);

    // Syntax-only: zero canonical call edges, observations present.
    assert!(
        snapshot.edges.iter().all(|e| e.kind != EdgeKind::Calls),
        "syntax mode must emit zero calls edges"
    );
    let observed: BTreeSet<&str> = snapshot
        .call_sites
        .iter()
        .map(|c| c.raw_name.as_str())
        .collect();
    for name in ["persist", "audit_log", "new", "find", "render"] {
        assert!(observed.contains(name), "missing call observation {name}");
    }
    // Prism local-scope tracking: bare paren-less calls that tree-sitter
    // cannot distinguish from local reads (`audit_log`, `touch` above, and
    // chained receivers below) are captured as observations.
    for name in ["touch", "tag", "div", "helpers", "icon"] {
        assert!(
            observed.contains(name),
            "missing bare-call observation {name}"
        );
    }

    // Inherits edge User -> Record (both unique in-repo).
    let record_id = "ruby:type:app/models/record.rb#Record";
    let user_id = "ruby:type:app/models/user.rb#User";
    let auditable_id = "ruby:module:app/models/auditable.rb#Auditable";
    assert!(
        snapshot
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Inherits && e.from == user_id && e.to == record_id),
        "missing inherits edge User -> Record"
    );
    // Mixin edges User -> Auditable: include becomes an `implements` edge
    // (instance-method ancestry) and extend becomes an `extends` edge
    // (singleton-method ancestry). The flavors are distinct edge kinds so
    // the semantic policy can keep the regimes separate.
    let implements_edges = snapshot
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Implements && e.from == user_id && e.to == auditable_id)
        .count();
    assert_eq!(
        implements_edges, 1,
        "include produces exactly one implements edge"
    );
    let extends_edges = snapshot
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Extends && e.from == user_id && e.to == auditable_id)
        .count();
    assert_eq!(extends_edges, 1, "extend produces exactly one extends edge");

    // Requires become imports edges with ImportRef placeholders.
    let imports = snapshot
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Imports)
        .count();
    assert_eq!(imports, 4, "json + record + auditable + ../models/user");
    let import_refs = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::ImportRef)
        .count();
    assert_eq!(import_refs, 4);

    // Parse errors are reported, not fatal. The exact count is
    // parser-granularity (Prism reports one diagnostic per unclosed
    // construct); the gate is that broken.rb surfaces errors and the
    // extraction still completes.
    assert!(snapshot.report.parse_errors >= 1, "broken.rb");
    assert_eq!(snapshot.report.files_scanned, 6);
}

#[test]
fn ruby_extraction_is_byte_deterministic_across_checkouts() {
    let repo_a = common::make_ruby_corpus_repo().expect("repo a");
    let repo_b = common::make_ruby_corpus_repo().expect("repo b");
    assert_eq!(
        to_json(&extract_ruby(&repo_a)).expect("serialize a"),
        to_json(&extract_ruby(&repo_b)).expect("serialize b"),
        "ruby extraction must be byte-identical across independent checkouts"
    );
}
