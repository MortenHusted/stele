//! Fact store, semiring annotations, support witnesses, and indexed relations.

use std::collections::{BTreeSet, HashMap};

use crate::intern::Value;

/// Semiring annotation carried by every fact:
/// confidence in [0,1] (product t-norm on joins) and provenance
/// (set of episode/source ids, union on joins).
#[derive(Debug, Clone, PartialEq)]
pub struct Ann {
    pub conf: f64,
    pub prov: BTreeSet<String>,
}

impl Default for Ann {
    fn default() -> Self {
        Ann::unit()
    }
}

impl Ann {
    pub fn base(conf: f64, prov: impl IntoIterator<Item: Into<String>>) -> Self {
        Ann {
            conf: conf.clamp(0.0, 1.0),
            prov: prov.into_iter().map(Into::into).collect(),
        }
    }

    pub fn unit() -> Self {
        Ann {
            conf: 1.0,
            prov: BTreeSet::new(),
        }
    }

    /// Semiring product: how annotations combine across a rule body.
    pub fn join(&self, other: &Ann) -> Ann {
        Ann {
            conf: (self.conf * other.conf).clamp(0.0, 1.0),
            prov: self.prov.union(&other.prov).cloned().collect(),
        }
    }
}

pub type Key = (String, Vec<Value>);

/// One event in the streaming change feed. Downstream projections (e.g. a
/// vector index or a UI) subscribe via `changes_since`: additions cover
/// every new fact, `Updated` marks an annotation improvement on an existing
/// tuple, `Retracted` marks explicit EDB removals, and `Cleared` marks a
/// derived relation that was wholesale rebuilt (scoped/program recompute) —
/// the signal to re-sync that predicate from scratch.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Added(u64, Key),
    Updated(u64, Key),
    Retracted(u64, Key),
    Cleared(u64, String),
}

impl Change {
    pub fn epoch(&self) -> u64 {
        match self {
            Change::Added(e, _)
            | Change::Updated(e, _)
            | Change::Retracted(e, _)
            | Change::Cleared(e, _) => *e,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InsertOutcome {
    New,
    Improved,
    Unchanged,
}

#[derive(Debug)]
pub(crate) struct Emission {
    pub(crate) key: Key,
    pub(crate) is_new: bool,
}

/// How one derived fact was produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Support {
    /// Asserted directly (EDB).
    Base,
    /// Derived by rule `rule` from body facts `body`.
    Rule { rule: String, body: Vec<Key> },
}

#[derive(Debug, Clone)]
pub struct StoredFact {
    pub ann: Ann,
    pub supports: Vec<Support>,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub key: Vec<Value>,
    pub fact: StoredFact,
}

/// A predicate's tuple set: rows in insertion order, with a key map for
/// dedup and lazy per-position secondary indexes whose lookups return row
/// ids (cheap) instead of cloned keys.
#[derive(Debug, Default, Clone)]
pub struct Relation {
    pub rows: Vec<Row>,
    by_key: HashMap<Vec<Value>, usize>,
    /// position -> value -> row ids
    idx: HashMap<usize, HashMap<Value, Vec<usize>>>,
    /// Facts added since the last completed `run()` (epoch delta).
    pub pending: BTreeSet<Vec<Value>>,
}

pub(crate) fn merge_supports(existing: &mut Vec<Support>, incoming: Vec<Support>) {
    for support in incoming {
        match support {
            Support::Base => {
                if !existing
                    .iter()
                    .any(|support| matches!(support, Support::Base))
                {
                    existing.push(Support::Base);
                }
            }
            Support::Rule { rule, body } => {
                if let Some(Support::Rule {
                    body: existing_body,
                    ..
                }) = existing.iter_mut().find(
                    |support| matches!(support, Support::Rule { rule: existing, .. } if existing == &rule),
                ) {
                    if body < *existing_body {
                        *existing_body = body;
                    }
                } else {
                    existing.push(Support::Rule { rule, body });
                }
            }
        }
    }
}

impl Relation {
    /// Insert or merge, distinguishing tuple creation from an annotation
    /// improvement that must flow through dependent rules.
    pub(crate) fn insert(&mut self, args: Vec<Value>, mut f: StoredFact) -> InsertOutcome {
        let supports = std::mem::take(&mut f.supports);
        merge_supports(&mut f.supports, supports);
        if let Some(&id) = self.by_key.get(&args) {
            let existing = &mut self.rows[id].fact;
            let previous_conf = existing.ann.conf.to_bits();
            let previous_provenance_len = existing.ann.prov.len();
            // lattice-merge annotations: max confidence, union provenance
            existing.ann.conf = existing.ann.conf.max(f.ann.conf);
            existing.ann.prov.extend(f.ann.prov);
            merge_supports(&mut existing.supports, f.supports);
            let improved = existing.ann.conf.to_bits() != previous_conf
                || existing.ann.prov.len() != previous_provenance_len;
            if improved {
                self.pending.insert(args);
                InsertOutcome::Improved
            } else {
                InsertOutcome::Unchanged
            }
        } else {
            let id = self.rows.len();
            self.index_row(id, &args);
            self.by_key.insert(args.clone(), id);
            self.rows.push(Row { key: args, fact: f });
            let key = self.rows[id].key.clone();
            self.pending.insert(key);
            InsertOutcome::New
        }
    }

    pub(crate) fn remove(&mut self, args: &[Value]) -> bool {
        let Some(&id) = self.by_key.get(args) else {
            return false;
        };
        let last = self.rows.len() - 1;
        self.unindex_row(id);
        self.by_key.remove(args);
        if id != last {
            // swap_remove moves the LAST row into the hole; its key map
            // and index entries must follow it. (swap_remove itself
            // returns the removed row, not the relocated one.)
            let last_key = self.rows[last].key.clone();
            self.unindex_row(last);
            self.rows.swap_remove(id); // last row is now at `id`
            self.by_key.insert(last_key, id);
            let relocated_key = self.rows[id].key.clone();
            self.index_row(id, &relocated_key);
        } else {
            self.rows.pop();
        }
        true
    }

    fn index_row(&mut self, id: usize, key: &[Value]) {
        for (pos, v) in key.iter().enumerate() {
            self.idx
                .entry(pos)
                .or_default()
                .entry(*v)
                .or_default()
                .push(id);
        }
    }

    fn unindex_row(&mut self, id: usize) {
        let key = self.rows[id].key.clone();
        for (pos, v) in key.iter().enumerate() {
            if let Some(bucket) = self.idx.get_mut(&pos).and_then(|m| m.get_mut(v)) {
                bucket.retain(|&i| i != id);
            }
        }
    }

    /// Clear all contents (scoped recompute).
    pub(crate) fn clear(&mut self) {
        self.rows.clear();
        self.by_key.clear();
        self.idx.clear();
        self.pending.clear();
    }

    /// Candidate row ids for a pattern given pre-resolved bound positions.
    /// Uses the bound position with the smallest bucket; falls back to a
    /// full scan when nothing is bound. Indexes are maintained eagerly on
    /// insert/remove, so this needs only `&self`.
    pub fn lookup(&self, bound: &[Option<Value>]) -> Vec<usize> {
        let mut best: Option<(usize, usize)> = None; // (size, pos)
        for (pos, b) in bound.iter().enumerate() {
            if let Some(v) = b {
                let size = self
                    .idx
                    .get(&pos)
                    .and_then(|m| m.get(v))
                    .map(|s| s.len())
                    .unwrap_or(0);
                if size == 0 {
                    return Vec::new(); // bound to a value with no facts
                }
                if best.map(|(s, _)| size < s).unwrap_or(true) {
                    best = Some((size, pos));
                }
            }
        }
        match best {
            Some((_, pos)) => {
                let v = bound[pos].unwrap();
                self.idx[&pos][&v]
                    .iter()
                    .copied()
                    .filter(|&id| {
                        bound
                            .iter()
                            .zip(&self.rows[id].key)
                            .all(|(b, v)| b.as_ref().map(|bv| bv == v).unwrap_or(true))
                    })
                    .collect()
            }
            None => (0..self.rows.len()).collect(),
        }
    }

    pub fn get(&self, args: &[Value]) -> Option<&StoredFact> {
        self.by_key.get(args).map(|&id| &self.rows[id].fact)
    }

    pub fn contains(&self, args: &[Value]) -> bool {
        self.by_key.contains_key(args)
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}
