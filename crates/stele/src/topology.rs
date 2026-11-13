//! The relations `stele topology` draws beside a snapshot's edges: each
//! comes from a named policy of the evidence adapter, never from the
//! topology crate, which reads the canonical graph only.

use std::collections::BTreeMap;

use codemap::schema::Snapshot;
use codemap_lemmaspec::{association, laravel, policy, template};
use codemap_topology::{Policy, Relation};

/// What the shipped policies derive from `snapshot`; a policy with
/// nothing to say about it is left out.
pub fn policies(snapshot: &Snapshot) -> Vec<Policy> {
    let language = snapshot.identity.language;
    let templates = [
        (
            template::TEMPLATE_RENDER,
            "renders",
            template::renders(snapshot),
        ),
        (
            template::TEMPLATE_OWNER,
            "template",
            template::owners(snapshot),
        ),
        (
            laravel::ASSOCIATION,
            "association",
            laravel::associations(snapshot),
        ),
        (
            laravel::TEMPLATE_RENDER,
            "renders",
            laravel::templates(snapshot),
        ),
    ]
    .into_iter()
    .filter(|(_, _, derived)| !derived.relations.is_empty() || !derived.unresolved.is_empty())
    .map(|(name, kind, derived)| Policy {
        name: policy::provenance(name, language),
        kind: kind.into(),
        relations: derived
            .relations
            .into_iter()
            .map(|relation| Relation {
                from: relation.from,
                to: relation.to,
                rule: relation.rule.into(),
                declaration: relation.declaration,
                through: None,
                via: None,
                rel_path: relation.rel_path,
                lines: relation.lines,
            })
            .collect(),
        unresolved: derived
            .unresolved
            .into_iter()
            .map(|(reason, count)| (reason.to_string(), count))
            .collect(),
    });
    associations(snapshot)
        .into_iter()
        .chain(templates)
        .collect()
}

fn associations(snapshot: &Snapshot) -> Option<Policy> {
    let derived = association::associations(snapshot);
    if derived.relations.is_empty() && derived.unresolved.is_empty() {
        return None;
    }
    let mut unresolved = BTreeMap::new();
    for entry in &derived.unresolved {
        *unresolved
            .entry(entry.reason.name().to_string())
            .or_default() += 1;
    }
    Some(Policy {
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
    })
}
