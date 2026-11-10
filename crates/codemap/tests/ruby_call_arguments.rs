//! Receiver-less Ruby calls made outside any method body keep their
//! arguments as written when at least one is a literal.
use std::collections::BTreeMap;

use codemap::schema::Language;
use codemap::{ExtractOptions, extract};
use serde_json::{Value, json};

mod common;

const SOURCE: &str = r#"
class Account
  belongs_to :owner, class_name: "Person", optional: true
  has_many :invoices, -> { order(:id) }, dependent: :destroy
  delegated_type :entryable, types: %w[Message Comment]
  has_one :profile, through: :owner, source: Billing::Profile
  validates :name, presence: true, allow_nil: nil
  mount Engine => "/engine"
  merge_options(**defaults, strict: false)
  helper_call
  compute(value)
  Other.configure :remote
  self.table_name = "accounts"

  with_options dependent: :destroy do
    has_many :notes
  end

  def run
    belongs_to :inside
  end
end
"#;

fn arguments_by_line() -> BTreeMap<(u32, String), Value> {
    let repo = common::make_repo_with(&[("app/models/account.rb".into(), SOURCE.into())]).unwrap();
    let snapshot = extract(&ExtractOptions {
        path: repo,
        language: Language::Ruby,
        calls_required: false,
        measures: false,
        twins: false,
    })
    .unwrap();
    codemap::validate::validate(&snapshot).unwrap();
    snapshot
        .call_sites
        .iter()
        .filter(|site| !site.arguments.is_empty())
        .map(|site| {
            (
                (site.span.start_line, site.raw_name.clone()),
                serde_json::to_value(&site.arguments).unwrap(),
            )
        })
        .collect()
}

fn symbol(name: &str) -> Value {
    json!({ "kind": "symbol", "value": name })
}

fn keyword(name: &str, value: Value) -> Value {
    Value::Object(
        [
            ("keyword".to_string(), name.into()),
            ("value".to_string(), value),
        ]
        .into_iter()
        .collect(),
    )
}

fn positional(value: Value) -> Value {
    Value::Object(std::iter::once(("value".to_string(), value)).collect())
}

#[test]
fn class_body_calls_keep_their_literal_arguments() {
    let other = json!({ "kind": "other" });
    let expected: BTreeMap<(u32, String), Value> = [
        (
            (3, "belongs_to"),
            json!([
                positional(symbol("owner")),
                keyword("class_name", json!({ "kind": "string", "value": "Person" })),
                keyword("optional", json!({ "kind": "boolean", "value": true })),
            ]),
        ),
        (
            (4, "has_many"),
            json!([
                positional(symbol("invoices")),
                positional(other.clone()),
                keyword("dependent", symbol("destroy")),
            ]),
        ),
        // A block or lambda outside a method is still outside any method.
        ((4, "order"), json!([positional(symbol("id"))])),
        (
            (5, "delegated_type"),
            json!([
                positional(symbol("entryable")),
                keyword(
                    "types",
                    json!({ "kind": "array", "value": [
                        { "kind": "string", "value": "Message" },
                        { "kind": "string", "value": "Comment" },
                    ] })
                ),
            ]),
        ),
        (
            (6, "has_one"),
            json!([
                positional(symbol("profile")),
                keyword("through", symbol("owner")),
                keyword(
                    "source",
                    json!({ "kind": "constant", "value": "Billing::Profile" })
                ),
            ]),
        ),
        (
            (7, "validates"),
            json!([
                positional(symbol("name")),
                keyword("presence", json!({ "kind": "boolean", "value": true })),
                keyword("allow_nil", json!({ "kind": "nil" })),
            ]),
        ),
        // A splat names no keyword. `mount Engine => "/engine"` on line 8
        // has no literal argument at all, so it keeps nothing.
        (
            (9, "merge_options"),
            json!([
                positional(other),
                keyword("strict", json!({ "kind": "boolean", "value": false })),
            ]),
        ),
        (
            (15, "with_options"),
            json!([keyword("dependent", symbol("destroy"))]),
        ),
        ((16, "has_many"), json!([positional(symbol("notes"))])),
    ]
    .into_iter()
    .map(|((line, name), value)| ((line, name.to_string()), value))
    .collect();
    assert_eq!(arguments_by_line(), expected);
}
