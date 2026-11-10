//! The relations `stele topology` draws beside a snapshot's edges: each
//! comes from a named policy of the evidence adapter, never from the
//! topology crate, which reads the canonical graph only.

use std::collections::BTreeMap;

use codemap::schema::Snapshot;
use codemap_lemmaspec::{association, policy};
use codemap_topology::{Policy, Relation};

/// What the shipped policies derive from `snapshot`; none when a policy
/// has nothing to say about it.
pub fn policies(snapshot: &Snapshot) -> Vec<Policy> {
    let derived = association::associations(snapshot);
    if derived.relations.is_empty() && derived.unresolved.is_empty() {
        return Vec::new();
    }
    let mut unresolved = BTreeMap::new();
    for entry in &derived.unresolved {
        *unresolved
            .entry(entry.reason.name().to_string())
            .or_default() += 1;
    }
    vec![Policy {
        name: policy::provenance(association::ASSOCIATION, snapshot.identity.language),
        kind: association::ASSOCIATION.into(),
        relations: derived
            .relations
            .into_iter()
            .map(|relation| Relation {
                from: relation.from,
                to: relation.to,
                rule: relation.rule.name().into(),
                declaration: format!(
                    "{} :{}",
                    relation.declaration.macro_name,
                    relation.declaration.name.as_deref().unwrap_or("?")
                ),
                through: relation.through,
                via: relation.via,
                rel_path: relation.declaration.rel_path,
                lines: relation.declaration.lines,
            })
            .collect(),
        unresolved,
    }]
}
