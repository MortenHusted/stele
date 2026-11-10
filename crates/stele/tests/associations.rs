//! `policy:association/ruby` over a real extraction: every way a class-body
//! association macro names its class, and every reason it names none.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use codemap::schema::{Language, Snapshot};
use codemap::{ExtractOptions, extract};
use codemap_lemmaspec::association::{Associations, associations};

const FILES: [(&str, &str); 11] = [
    (
        "app/models/person.rb",
        r#"
class Person < ApplicationRecord
  include Taggable
  has_many :invoices
  has_many :line_items, through: :invoices
  has_many :categories
  has_many :statuses
  has_many :api_keys
  has_many :notes, as: :subject
  has_one :profile
  has_one :duplicate
  belongs_to :manager
  belongs_to :ghost
  belongs_to :employer, class_name: employer_class
  has_many relation_name, class_name: "Tag"
  has_many :taggings
  has_many :labels, through: :taggings, source: :tag
end
class Manager < Person
  has_many :team_labels, through: :taggings, source: :tag
end
class Tagging
  belongs_to :tag
end
class Category; end
class Status; end
class APIKey; end
class Tag; end
class LineItem; end
"#,
    ),
    (
        "app/models/billing/invoice.rb",
        r#"
module Billing
  class Invoice < ApplicationRecord
    include Taggable
    belongs_to :payer, class_name: "Person"
    belongs_to :account, class_name: ::Account
    has_many :line_items
    has_one :note, as: :subject
  end
  class LineItem; end
end
class Invoice
  has_many :people
end
class Account; end
"#,
    ),
    (
        "app/models/note.rb",
        r"
class Note
  belongs_to :subject, polymorphic: true
end
class Attachment
  belongs_to :owner, polymorphic: true
end
",
    ),
    (
        "app/models/entry.rb",
        r"
class Entry
  delegated_type :entryable, types: %w[Message Missing]
  delegated_type :other, types: entry_types
end
class Message; end
",
    ),
    (
        "app/models/concerns/taggable.rb",
        r"
module Taggable
  included do
    has_many :tags
  end
end
module Orphaned
  has_many :tags
end
",
    ),
    ("app/models/profile.rb", "class Profile; end\n"),
    ("lib/core_ext.rb", "class Profile\n  def extra; end\nend\n"),
    ("app/models/duplicate.rb", "class Duplicate; end\n"),
    ("lib/duplicate.rb", "class Duplicate; end\n"),
    (
        "app/models/application_record.rb",
        "class ApplicationRecord; end\n",
    ),
    ("config/setup.rb", "has_many :things\n"),
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

type Relation = (String, String, &'static str, String, String, String);

/// (owner, name, rule, target, through, via), with classes by qualified name.
fn relations(snapshot: &Snapshot, derived: &Associations) -> BTreeSet<Relation> {
    let class = |id: &str| {
        let node = snapshot.nodes.iter().find(|node| node.id == id).unwrap();
        node.symbol_key.split_once('#').unwrap().1.to_string()
    };
    derived
        .relations
        .iter()
        .map(|relation| {
            (
                class(&relation.from),
                format!(
                    "{} :{}",
                    relation.declaration.macro_name,
                    relation.declaration.name.as_deref().unwrap()
                ),
                relation.rule.name(),
                class(&relation.to),
                relation.through.clone().unwrap_or_default(),
                relation.via.as_deref().map(class).unwrap_or_default(),
            )
        })
        .collect()
}

/// (owner, declaration, rule, class reached, through, declaring module).
#[rustfmt::skip]
const RELATED: [(&str, &str, &str, &str, &str, &str); 22] = [
    // The association's own name, as written or as a singular of it.
    ("Person", "has_many :invoices", "inferred", "Invoice", "", ""),
    ("Person", "has_many :categories", "inferred", "Category", "", ""),
    ("Person", "has_many :statuses", "inferred", "Status", "", ""),
    ("Person", "has_many :api_keys", "inferred", "APIKey", "", ""),
    ("Person", "has_one :profile", "inferred", "Profile", "", ""),
    ("Person", "belongs_to :manager", "inferred", "Manager", "", ""),
    ("Invoice", "has_many :people", "inferred", "Person", "", ""),
    ("Person", "has_many :notes", "inferred", "Note", "", ""),
    ("Billing::Invoice", "has_one :note", "inferred", "Note", "", ""),
    // A class in the owner's namespace shadows one at the top level.
    ("Billing::Invoice", "has_many :line_items", "inferred", "Billing::LineItem", "", ""),
    // `through:` follows the association it goes through, here and from a
    // subclass; when that leads nowhere the name decides.
    ("Person", "has_many :taggings", "inferred", "Tagging", "", ""),
    ("Tagging", "belongs_to :tag", "inferred", "Tag", "", ""),
    ("Person", "has_many :labels", "through", "Tag", "taggings", ""),
    ("Manager", "has_many :team_labels", "through", "Tag", "taggings", ""),
    ("Person", "has_many :line_items", "inferred", "LineItem", "invoices", ""),
    // `class_name:` as a string or a constant.
    ("Billing::Invoice", "belongs_to :payer", "class_name", "Person", "", ""),
    ("Billing::Invoice", "belongs_to :account", "class_name", "Account", "", ""),
    // A polymorphic belongs_to reaches the classes declaring `as:` back.
    ("Note", "belongs_to :subject", "polymorphic_as", "Person", "", ""),
    ("Note", "belongs_to :subject", "polymorphic_as", "Billing::Invoice", "", ""),
    ("Entry", "delegated_type :entryable", "delegated_type", "Message", "", ""),
    // A module's declaration belongs to each class that includes it.
    ("Person", "has_many :tags", "inferred", "Tag", "", "Taggable"),
    ("Billing::Invoice", "has_many :tags", "inferred", "Tag", "", "Taggable"),
];

/// (reason, file, macro, association name or `?`).
#[rustfmt::skip]
const UNRESOLVED: [(&str, &str, &str, &str); 9] = [
    ("ambiguous", "app/models/person.rb", "has_one", "duplicate"),
    ("no_class", "app/models/person.rb", "belongs_to", "ghost"),
    ("dynamic", "app/models/person.rb", "belongs_to", "employer"),
    ("dynamic", "app/models/person.rb", "has_many", "?"),
    ("no_polymorphic_target", "app/models/note.rb", "belongs_to", "owner"),
    ("no_class", "app/models/entry.rb", "delegated_type", "entryable"),
    ("dynamic", "app/models/entry.rb", "delegated_type", "other"),
    ("no_includer", "app/models/concerns/taggable.rb", "has_many", "tags"),
    ("no_owner", "config/setup.rb", "has_many", "things"),
];

#[test]
fn associations_relate_classes_only_when_exactly_one_class_is_named() {
    let snapshot = snapshot();
    let derived = associations(&snapshot);
    let expected: BTreeSet<Relation> = RELATED
        .into_iter()
        .map(|(owner, declaration, rule, target, through, via)| {
            (
                owner.to_string(),
                declaration.to_string(),
                rule,
                target.to_string(),
                through.to_string(),
                via.to_string(),
            )
        })
        .collect();
    assert_eq!(relations(&snapshot, &derived), expected);
    // Two files define Profile; the one an autoloader maps the name to is it.
    assert!(derived.relations.iter().any(|relation| {
        relation.declaration.name.as_deref() == Some("profile")
            && relation.to == "ruby:type:app/models/profile.rb#Profile"
    }));

    let unresolved: BTreeSet<_> = derived
        .unresolved
        .iter()
        .map(|entry| {
            (
                entry.reason.name(),
                entry.declaration.rel_path.as_str(),
                entry.declaration.macro_name.as_str(),
                entry.declaration.name.as_deref().unwrap_or("?"),
            )
        })
        .collect();
    assert_eq!(unresolved, BTreeSet::from(UNRESOLVED));
}

#[test]
fn a_snapshot_in_another_language_has_no_associations() {
    let mut snapshot = snapshot();
    snapshot.identity.language = Language::Rust;
    assert_eq!(associations(&snapshot), Associations::default());
}

#[test]
fn the_map_carries_associations_as_policy_relations_beside_its_edges() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("snapshot.json");
    let out = dir.path().join("topology.json");
    std::fs::write(&source, serde_json::to_vec(&snapshot()).unwrap()).unwrap();
    let project = |extra: &[&str]| -> serde_json::Value {
        let run = Command::new(env!("CARGO_BIN_EXE_stele"))
            .args(["topology", "--snapshot"])
            .arg(&source)
            .arg("--out")
            .arg(&out)
            .args(extra)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap()
    };
    let topology = project(&[]);
    let relations = topology["codemap"]["policy_relations"].as_array().unwrap();
    assert_eq!(relations.len(), RELATED.len());
    let payer = relations
        .iter()
        .find(|relation| relation["declaration"] == "belongs_to :payer")
        .unwrap();
    assert_eq!(
        payer["source"],
        "@container:ruby:type:app/models/billing/invoice.rb#Billing::Invoice"
    );
    assert_eq!(
        payer["target"],
        "@container:ruby:type:app/models/person.rb#Person"
    );
    assert_eq!(payer["kind"], "association");
    assert_eq!(payer["policy"], "policy:association/ruby");
    assert_eq!(payer["rule"], "class_name");
    assert_eq!(payer["source_link"], "app/models/billing/invoice.rb#L5-L5");
    assert!(
        topology["edges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|edge| edge["kind"] != "association")
    );
    let summary = &topology["codemap"]["policies"];
    assert_eq!(summary[0]["policy"], "policy:association/ruby");
    assert_eq!(summary[0]["relations"], RELATED.len());
    assert_eq!(summary[0]["unresolved"]["no_class"], 2);

    // An exclusion hides relations; what the policy found is unchanged.
    let scoped = project(&["--exclude", "app/models/billing"]);
    assert!(
        scoped["codemap"]["policy_relations"]
            .as_array()
            .unwrap()
            .len()
            < RELATED.len()
    );
    assert_eq!(scoped["codemap"]["policies"], *summary);
}
