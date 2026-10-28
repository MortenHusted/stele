//! Positive path claims over validated call facts, with exact node selections.

use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use codemap_graph::schema::{Language, NodeKind, Snapshot};

use crate::lemmaspec::{self, Basis, Expectation, Fact, Readable};

pub const POLICY: &str = "cpp-positive-path/v1";

#[derive(Debug, Clone)]
pub struct Options {
    pub entry_node: String,
    pub target_node: String,
    pub allow_dirty: bool,
}

const DECLARATIONS: &str = r#"
  relation defines { args: [symbol, symbol] }
  relation calls { args: [symbol, symbol] }
  relation entry_point { args: [symbol] }
  relation selected_target { args: [symbol] }
  relation observed_path { args: [symbol, symbol] }
  relation selected_path { args: [symbol, symbol] }

  rule direct_path {
    derive: "observed_path(Entry, Target)"
    when: ["entry_point(Entry)", "defines(Entry, _)", "defines(Target, _)", "calls(Entry, Target)"]
  }
  rule continued_path {
    derive: "observed_path(Entry, Target)"
    when: ["observed_path(Entry, Intermediate)", "calls(Intermediate, Target)", "defines(Target, _)"]
  }
  rule selected_path {
    derive: "selected_path(Entry, Target)"
    when: ["observed_path(Entry, Target)", "selected_target(Target)"]
  }
"#;

/// A failed expectation means this graph did not prove the selected path.
/// It makes no claim about runtime reachability or extraction completeness.
pub fn project(snapshot: &Snapshot, options: &Options) -> Result<String> {
    codemap_graph::validate(snapshot)?;
    ensure!(
        matches!(
            snapshot.identity.language,
            Language::Cpp | Language::JsTs | Language::Python
        ) && snapshot.identity.is_semantic(),
        "positive paths require a validated semantic C++, JS/TS or Python snapshot"
    );
    ensure!(
        !snapshot.identity.dirty || options.allow_dirty,
        "positive paths require --allow-dirty for a dirty snapshot"
    );
    let callables: BTreeMap<_, _> = snapshot
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Function | NodeKind::Method))
        .map(|node| (node.id.as_str(), node))
        .collect();
    for (flag, id) in [
        ("--entry-node", options.entry_node.as_str()),
        ("--target-node", options.target_node.as_str()),
    ] {
        ensure!(
            callables.contains_key(id),
            "{flag} must select an exact callable node ID in this snapshot: {id}"
        );
    }
    let mut facts = crate::definition_facts(&callables);
    facts.extend(crate::call_facts(snapshot, &callables));
    for (relation, id) in [
        ("entry_point", &options.entry_node),
        ("selected_target", &options.target_node),
    ] {
        facts.push(Fact {
            basis: Basis::ReviewerDeclared,
            relation,
            args: vec![id.clone()],
            provenance: None,
        });
    }
    facts.sort();
    facts.dedup();
    let (policy, language, document) = match snapshot.identity.language {
        Language::Cpp => (POLICY, "C++", "codemap_cpp_positive_path"),
        Language::JsTs => (
            "js-ts-positive-path/v1",
            "JS/TS",
            "codemap_js_ts_positive_path",
        ),
        Language::Python => (
            "python-positive-path/v1",
            "Python",
            "codemap_python_positive_path",
        ),
        _ => unreachable!("language admission checked"),
    };
    let notes = format!(
        "Policy: {policy}. Canonical snapshot: {}. Entry and target are exact reviewer selections. Calls retain source provenance. A failed expectation means no selected path was proven in this graph; runtime reachability and whole-program completeness remain unknown.",
        snapshot.identity.snapshot_id
    );
    Ok(lemmaspec::render_with_declarations(
        &[format!(
            "Does the observed call graph prove a path between the selected {language} nodes?"
        )],
        document,
        &facts,
        &[Expectation {
            id: "selected_path_is_proven",
            query: "selected_path(Entry, Target)",
            count: 1,
        }],
        Some(&Readable {
            snapshot_id: &snapshot.identity.snapshot_id,
            notes: &notes,
            labels: &[],
        }),
        DECLARATIONS,
    ))
}
