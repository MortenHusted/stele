//! Cross-commit structural diff: the deterministic differentiator.
//!
//! Because snapshots are byte-deterministic and commit-pinned, two snapshots
//! at different commits can be diffed to yield the *exact* set of structural
//! changes — added/removed/changed symbols and edges. This is graph math over
//! Layer 0 facts; it derives nothing new and writes nothing back. `GitNexus`'s
//! live working-tree index cannot do this; codemap can, because of Layer 0's
//! determinism.
//!
//! Churn heuristics (high fan-in ∧ high change → refactor hotspot) layer on
//! top and are explicitly non-authoritative — they live in [`churn_hotspots`].

use std::collections::{BTreeMap, BTreeSet};

use crate::schema::{Edge, Node, NodeKind, Snapshot};

/// A structural diff between two snapshots.
#[derive(Debug, Clone)]
pub struct SnapshotDiff {
    pub base_commit: String,
    pub head_commit: String,
    pub added_nodes: Vec<Node>,
    pub removed_nodes: Vec<Node>,
    /// Nodes present in both whose span, file hash, or symbol key changed.
    pub changed_nodes: Vec<NodeChange>,
    pub added_edges: Vec<Edge>,
    pub removed_edges: Vec<Edge>,
}

/// A node that exists in both snapshots but whose content/span shifted.
#[derive(Debug, Clone)]
pub struct NodeChange {
    pub id: String,
    pub name: String,
    pub rel_path: String,
    pub span_moved: bool,
    pub file_hash_changed: bool,
}

/// A per-symbol churn summary derived from a diff — non-authoritative.
#[derive(Debug, Clone)]
pub struct ChurnHotspot {
    pub node: String,
    pub name: String,
    pub added_edges: usize,
    pub removed_edges: usize,
    pub total_change: usize,
}

impl SnapshotDiff {
    /// Compute the structural diff between two snapshots.
    pub fn between(base: &Snapshot, head: &Snapshot) -> Self {
        let base_node_keys = stable_node_keys(base);
        let head_node_keys = stable_node_keys(head);
        let base_nodes: BTreeMap<&str, &Node> = base
            .nodes
            .iter()
            .map(|n| (endpoint(&base_node_keys, n.id.as_str()), n))
            .collect();
        let head_nodes: BTreeMap<&str, &Node> = head
            .nodes
            .iter()
            .map(|n| (endpoint(&head_node_keys, n.id.as_str()), n))
            .collect();

        let mut added_nodes: Vec<Node> = Vec::new();
        let mut removed_nodes: Vec<Node> = Vec::new();
        let mut changed_nodes: Vec<NodeChange> = Vec::new();

        for (id, hn) in &head_nodes {
            match base_nodes.get(id) {
                None => added_nodes.push((*hn).clone()),
                Some(bn) => {
                    let span_moved = bn.provenance.span.start_byte != hn.provenance.span.start_byte
                        || bn.provenance.span.end_byte != hn.provenance.span.end_byte;
                    let file_hash_changed = bn.provenance.file_hash != hn.provenance.file_hash;
                    if span_moved || file_hash_changed {
                        changed_nodes.push(NodeChange {
                            id: (*id).to_string(),
                            name: hn.name.clone(),
                            rel_path: hn.provenance.rel_path.clone(),
                            span_moved,
                            file_hash_changed,
                        });
                    }
                }
            }
        }
        for (id, bn) in &base_nodes {
            if !head_nodes.contains_key(*id) {
                removed_nodes.push((*bn).clone());
            }
        }

        let base_keys = stable_edge_keys(base);
        let head_keys = stable_edge_keys(head);
        let base_edges: BTreeSet<&str> = base_keys.values().map(String::as_str).collect();
        let head_edges: BTreeSet<&str> = head_keys.values().map(String::as_str).collect();

        let added_edges: Vec<Edge> = head
            .edges
            .iter()
            .filter(|e| !base_edges.contains(head_keys[e.id.as_str()].as_str()))
            .cloned()
            .collect();
        let removed_edges: Vec<Edge> = base
            .edges
            .iter()
            .filter(|e| !head_edges.contains(base_keys[e.id.as_str()].as_str()))
            .cloned()
            .collect();

        Self {
            base_commit: base.identity.commit_sha.clone(),
            head_commit: head.identity.commit_sha.clone(),
            added_nodes,
            removed_nodes,
            changed_nodes,
            added_edges,
            removed_edges,
        }
    }

    /// Summary counts.
    pub fn summary(&self) -> DiffSummary {
        DiffSummary {
            added_nodes: self.added_nodes.len(),
            removed_nodes: self.removed_nodes.len(),
            changed_nodes: self.changed_nodes.len(),
            added_edges: self.added_edges.len(),
            removed_edges: self.removed_edges.len(),
        }
    }

    /// Non-authoritative churn hotspots: nodes with the most added/removed
    /// edges touching them in this diff. A refactor-triage signal, not a fact.
    pub fn hotspots(&self, top: usize) -> Vec<ChurnHotspot> {
        let mut counts: BTreeMap<String, (String, usize, usize)> = BTreeMap::new();
        for e in &self.added_edges {
            for id in [&e.from, &e.to] {
                counts
                    .entry(id.clone())
                    .and_modify(|(_, a, _)| *a += 1)
                    .or_insert_with(|| (id.clone(), 1, 0));
            }
        }
        for e in &self.removed_edges {
            for id in [&e.from, &e.to] {
                counts
                    .entry(id.clone())
                    .and_modify(|(_, _, r)| *r += 1)
                    .or_insert_with(|| (id.clone(), 0, 1));
            }
        }
        let mut hot: Vec<ChurnHotspot> = counts
            .into_iter()
            .map(|(node, (_, added, removed))| ChurnHotspot {
                name: node.clone(),
                node,
                added_edges: added,
                removed_edges: removed,
                total_change: added + removed,
            })
            .collect();
        hot.sort_by(|a, b| {
            b.total_change
                .cmp(&a.total_change)
                .then(a.node.cmp(&b.node))
        });
        hot.truncate(top);
        hot
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DiffSummary {
    pub added_nodes: usize,
    pub removed_nodes: usize,
    pub changed_nodes: usize,
    pub added_edges: usize,
    pub removed_edges: usize,
}

/// A node that probably moved rather than died and was reborn — **derived and
/// non-authoritative**. See [`infer_moves`].
#[derive(Debug, Clone)]
pub struct NodeMove {
    pub from: Node,
    pub to: Node,
}

/// Pair removed nodes with added ones that are most likely the same fact
/// relocated, so a refactor does not read as a mass extinction.
///
/// Node identity is path-scoped: `rust:function:src/a.rs#foo` and
/// `rust:function:src/b.rs#foo` are different ids, so moving a symbol between
/// files is a death and a birth in the exact diff. That is *correct* as Layer 0
/// — the facts really are distinct — and useless as history, because the one
/// question worth asking ("when did this appear?") answers with the date of the
/// last refactor rather than the date it was written.
///
/// So the pairing lives here rather than in [`SnapshotDiff::between`], which
/// stays exact. This is a hypothesis layered on top:
///
/// - **File nodes** pair on an identical file hash — a renamed file whose
///   contents did not change is about as strong as this evidence gets.
/// - **Every other node** pairs on kind and name.
///
/// A candidate is only paired when it is **unique on both sides** and the path
/// actually changed. Two functions called `new` moving in one commit stay a
/// death and a birth: guessing between them would be exactly the name-matching
/// the truth boundary forbids for calls, and it is no more defensible here.
pub fn infer_moves(diff: &SnapshotDiff) -> Vec<NodeMove> {
    let mut moves = Vec::new();
    let mut paired: BTreeSet<&str> = BTreeSet::new();

    // File hash first: it is the stronger signal, and claiming it before the
    // name pass stops a renamed file from also matching on its name.
    pair_unique(diff, &mut paired, &mut moves, |node| {
        (node.kind == NodeKind::File).then_some(node.provenance.file_hash.as_str())
    });
    pair_unique(diff, &mut paired, &mut moves, |node| {
        (node.kind != NodeKind::File).then_some(node.name.as_str())
    });

    moves.sort_by(|a, b| a.from.id.cmp(&b.from.id).then(a.to.id.cmp(&b.to.id)));
    moves
}

/// Pair removed and added nodes sharing a key, when that key identifies exactly
/// one node on each side and the two sit at different paths.
fn pair_unique<'a, F>(
    diff: &'a SnapshotDiff,
    paired: &mut BTreeSet<&'a str>,
    moves: &mut Vec<NodeMove>,
    key: F,
) where
    F: Fn(&'a Node) -> Option<&'a str>,
{
    // Keyed by kind as well as the caller's key, so a function and a type of
    // the same name are never mistaken for one another.
    let index = |nodes: &'a [Node]| -> BTreeMap<(&'a str, &'static str), Vec<&'a Node>> {
        let mut out: BTreeMap<(&'a str, &'static str), Vec<&'a Node>> = BTreeMap::new();
        for node in nodes {
            if paired.contains(node.id.as_str()) {
                continue;
            }
            if let Some(key) = key(node) {
                out.entry((key, node.kind.tag())).or_default().push(node);
            }
        }
        out
    };

    let removed = index(&diff.removed_nodes);
    let added = index(&diff.added_nodes);
    for (key, from) in removed {
        let (Some(to), 1) = (added.get(&key), from.len()) else {
            continue;
        };
        let ([from], [to]) = (from.as_slice(), to.as_slice()) else {
            continue;
        };
        if from.provenance.rel_path == to.provenance.rel_path {
            continue;
        }
        paired.insert(from.id.as_str());
        paired.insert(to.id.as_str());
        moves.push(NodeMove {
            from: (*from).clone(),
            to: (*to).clone(),
        });
    }
}

/// One edge as `(relation, rel_path, start_byte, edge id)`, where `relation` is
/// `kind:from->to` over stable endpoints. Sorting groups a relation and puts its
/// occurrences in source order.
type EdgeSite<'a> = (String, &'a str, usize, &'a str);

/// Split a node id into its offset-free base and its byte offset, if it carries
/// one. `ImportRef` ids always end in `@<start_byte>`, and a definition whose
/// qualified name repeats within a file gets the same suffix from
/// `schema::node_id_disambiguated`.
fn split_disambiguator(id: &str) -> Option<(&str, usize)> {
    let (base, offset) = id.rsplit_once('@')?;
    offset.parse().ok().map(|offset| (base, offset))
}

/// Map every node in `snapshot` to its stable cross-commit key.
///
/// Most node ids are already stable — path plus qualified name. Two kinds are
/// not: every `ImportRef` placeholder embeds the import's byte offset, and a
/// definition whose qualified name repeats in a file is disambiguated by
/// offset. Those churn for the same reason edge ids do, so an unrelated edit
/// above them reads as the old fact dying and a new one being born — and it
/// drags the `imports` edges pointing at them along with it.
///
/// The offset is replaced by an occurrence index among the ids sharing a base,
/// numbered in source order, so repeats stay distinct facts while an edit above
/// them changes nothing.
pub fn stable_node_keys(snapshot: &Snapshot) -> BTreeMap<&str, String> {
    // Only ids carrying an offset need rewriting; leaving the rest verbatim
    // keeps the common key identical to the node id, which is what callers
    // reading these streams expect to see.
    let mut families: BTreeMap<&str, Vec<(usize, &str)>> = BTreeMap::new();
    for node in &snapshot.nodes {
        if let Some((base, offset)) = split_disambiguator(&node.id) {
            families
                .entry(base)
                .or_default()
                .push((offset, node.id.as_str()));
        }
    }

    let mut keys: BTreeMap<&str, String> = BTreeMap::new();
    for node in &snapshot.nodes {
        keys.insert(node.id.as_str(), node.id.clone());
    }
    for (base, mut members) in families {
        members.sort_unstable();
        for (occurrence, (_, id)) in members.into_iter().enumerate() {
            keys.insert(id, format!("{base}#{occurrence}"));
        }
    }
    keys
}

/// The stable key for a node id, falling back to the id itself. Edge endpoints
/// name nodes, so an endpoint whose id churns would churn the edge key too.
fn endpoint<'a>(keys: &'a BTreeMap<&str, String>, id: &'a str) -> &'a str {
    keys.get(id).map_or(id, String::as_str)
}

/// Map every edge in `snapshot` to its stable cross-commit key, by edge id.
///
/// [`Edge::id`] embeds the fact's byte offset (see `schema::edge_id`). That is
/// the right identity *within* one snapshot — it separates repeated facts and
/// addresses them to an exact source position — and the wrong identity *across*
/// commits: inserting one line near the top of a file shifts every offset below
/// it, so a diff keyed on `Edge::id` reports phantom add/remove pairs for
/// relationships that never changed, and any churn signal derived from those
/// sets ranks *where an early line was edited* rather than where structure
/// moved.
///
/// The stable key keeps `(kind, from, to)` and drops the offset, adding an
/// occurrence index so genuine repeats of the same triple in one file stay
/// distinct facts — three separate `foo -> bar` call sites remain three edges.
/// Occurrences are numbered in source order, so the key survives any edit that
/// does not change how many times the triple occurs.
///
/// This derives an identity for comparison only; it never alters the snapshot,
/// so no serialized byte and no pinned digest depends on it.
pub fn stable_edge_keys(snapshot: &Snapshot) -> BTreeMap<&str, String> {
    // Sorting groups each triple together and puts its occurrences in source
    // order, so the occurrence index is a function of the source rather than of
    // the order edges happen to appear in the snapshot.
    let nodes = stable_node_keys(snapshot);
    let mut sites: Vec<EdgeSite<'_>> = snapshot
        .edges
        .iter()
        .map(|e| {
            let relation = format!(
                "{}:{}->{}",
                e.kind.tag(),
                endpoint(&nodes, e.from.as_str()),
                endpoint(&nodes, e.to.as_str()),
            );
            (
                relation,
                e.provenance.rel_path.as_str(),
                e.provenance.span.start_byte,
                e.id.as_str(),
            )
        })
        .collect();
    sites.sort_unstable();

    let mut keys: BTreeMap<&str, String> = BTreeMap::new();
    let mut previous: Option<String> = None;
    let mut occurrence = 0usize;
    for (relation, _, _, id) in sites {
        if previous.as_deref() == Some(relation.as_str()) {
            occurrence += 1;
        } else {
            occurrence = 0;
        }
        keys.insert(id, format!("{relation}#{occurrence}"));
        previous = Some(relation);
    }
    keys
}

/// Format an edge for diff display: `kind from -> to`.
pub fn edge_display(e: &Edge) -> String {
    format!("{} {} -> {}", e.kind.tag(), e.from, e.to)
}

/// Format a node for diff display: `kind name (symbol_key) [rel_path]`.
pub fn node_display(n: &Node) -> String {
    format!(
        "{} {} ({}) [{}]",
        n.kind, n.name, n.symbol_key, n.provenance.rel_path
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        Edge, EdgeKind, FactSource, FileRecord, Language, NodeKind, Provenance, RepoIdentity,
        Report, SnapshotIdentity, Span, ToolVersions,
    };

    fn snap(commit: &str, nodes: Vec<Node>, edges: Vec<Edge>) -> Snapshot {
        let repo = RepoIdentity {
            id: "r".into(),
            origin: None,
            is_canonical: false,
        };
        let identity = SnapshotIdentity::build(
            Language::Rust,
            repo,
            commit.into(),
            "t".into(),
            ToolVersions::syntax(),
            false,
        );
        Snapshot {
            identity,
            files: vec![FileRecord {
                rel_path: "a.rs".into(),
                sha256: "h".into(),
                byte_len: 1024,
            }],
            nodes,
            edges,
            call_sites: vec![],
            measures: Vec::new(),
            file_measures: Vec::new(),
            clone_groups: Vec::new(),
            twin_pairs: Vec::new(),
            report: Report::default(),
        }
    }

    fn node(id: &str, name: &str, se: usize, hash: &str) -> Node {
        Node {
            python: None,
            id: id.into(),
            kind: NodeKind::Function,
            name: name.into(),
            symbol_key: format!("rust:a.rs#{name}"),
            provenance: Provenance {
                repository: "r".into(),
                commit_sha: "c".into(),
                rel_path: "a.rs".into(),
                span: Span::from_ts(se, se + 10, (0, se), (0, se + 10)),
                file_hash: hash.into(),
                extractor_version: "0.1.0".into(),
                fact_source: FactSource::TreeSitter,
                confidence: 255,
            },
        }
    }

    fn edge(id: &str, from: &str, to: &str) -> Edge {
        edge_at(id, from, to, 0)
    }

    /// An edge whose id embeds its byte offset, exactly as `schema::edge_id`
    /// builds it — the shape the stable key has to see through.
    fn edge_at(id: &str, from: &str, to: &str, start_byte: usize) -> Edge {
        Edge {
            id: id.into(),
            kind: EdgeKind::Calls,
            from: from.into(),
            to: to.into(),
            provenance: Provenance {
                repository: "r".into(),
                commit_sha: "c".into(),
                rel_path: "a.rs".into(),
                span: Span::from_ts(start_byte, start_byte + 5, (0, 0), (0, 5)),
                file_hash: "h".into(),
                extractor_version: "0.1.0".into(),
                fact_source: FactSource::RustAnalyzer,
                confidence: 255,
            },
        }
    }

    #[test]
    fn diff_adds_and_removes() {
        let base = snap(
            "AAA",
            vec![
                node("rust:a.rs#foo", "foo", 0, "h1"),
                node("rust:a.rs#bar", "bar", 50, "h1"),
            ],
            vec![edge("e1", "rust:a.rs#foo", "rust:a.rs#bar")],
        );
        let head = snap(
            "BBB",
            vec![
                node("rust:a.rs#foo", "foo", 0, "h1"),
                node("rust:a.rs#baz", "baz", 80, "h2"),
            ],
            vec![edge("e2", "rust:a.rs#foo", "rust:a.rs#baz")],
        );
        let d = SnapshotDiff::between(&base, &head);
        assert_eq!(d.added_nodes.len(), 1);
        assert_eq!(d.added_nodes[0].name, "baz");
        assert_eq!(d.removed_nodes.len(), 1);
        assert_eq!(d.removed_nodes[0].name, "bar");
        assert_eq!(d.added_edges.len(), 1);
        assert_eq!(d.removed_edges.len(), 1);
        assert_eq!(d.base_commit, "AAA");
        assert_eq!(d.head_commit, "BBB");
    }

    #[test]
    fn diff_detects_span_and_hash_changes() {
        let base = snap("AAA", vec![node("rust:a.rs#foo", "foo", 0, "h1")], vec![]);
        let head = snap("BBB", vec![node("rust:a.rs#foo", "foo", 20, "h2")], vec![]);
        let d = SnapshotDiff::between(&base, &head);
        assert_eq!(d.changed_nodes.len(), 1);
        assert!(d.changed_nodes[0].span_moved);
        assert!(d.changed_nodes[0].file_hash_changed);
        assert_eq!(d.added_nodes.len(), 0);
        assert_eq!(d.removed_nodes.len(), 0);
    }

    /// The regression this key exists for: an edit above an edge shifts its
    /// byte offset, which changes `Edge::id`. Keyed on the id, the same
    /// unchanged relationship reads as one removal plus one addition.
    #[test]
    fn shifting_bytes_does_not_churn_an_unchanged_edge() {
        let nodes = vec![
            node("rust:a.rs#foo", "foo", 0, "h1"),
            node("rust:a.rs#bar", "bar", 50, "h1"),
        ];
        let base = snap(
            "AAA",
            nodes.clone(),
            vec![edge_at(
                "calls:rust:a.rs#foo->rust:a.rs#bar@100",
                "rust:a.rs#foo",
                "rust:a.rs#bar",
                100,
            )],
        );
        // One line inserted above: same relationship, new offset, new edge id.
        let head = snap(
            "BBB",
            nodes,
            vec![edge_at(
                "calls:rust:a.rs#foo->rust:a.rs#bar@142",
                "rust:a.rs#foo",
                "rust:a.rs#bar",
                142,
            )],
        );
        let d = SnapshotDiff::between(&base, &head);
        assert!(d.added_edges.is_empty(), "{:?}", d.added_edges);
        assert!(d.removed_edges.is_empty(), "{:?}", d.removed_edges);
        // ...and the churn signal derived from those sets stays silent too.
        assert!(d.hotspots(5).is_empty());
    }

    /// Repeats of one triple stay separate facts: three `foo -> bar` call
    /// sites are three edges, and deleting one is one removal.
    #[test]
    fn repeated_triple_keeps_one_key_per_occurrence() {
        let nodes = vec![
            node("rust:a.rs#foo", "foo", 0, "h1"),
            node("rust:a.rs#bar", "bar", 50, "h1"),
        ];
        let sites = |offsets: &[usize]| -> Vec<Edge> {
            offsets
                .iter()
                .map(|o| {
                    edge_at(
                        &format!("calls:rust:a.rs#foo->rust:a.rs#bar@{o}"),
                        "rust:a.rs#foo",
                        "rust:a.rs#bar",
                        *o,
                    )
                })
                .collect()
        };
        let base = snap("AAA", nodes.clone(), sites(&[100, 200, 300]));
        let head = snap("BBB", nodes, sites(&[100, 200]));

        let keys = stable_edge_keys(&base);
        assert_eq!(keys.len(), 3, "each occurrence gets its own key");
        assert_eq!(
            keys.values().collect::<BTreeSet<_>>().len(),
            3,
            "occurrence keys are distinct"
        );

        let d = SnapshotDiff::between(&base, &head);
        assert!(d.added_edges.is_empty(), "{:?}", d.added_edges);
        assert_eq!(d.removed_edges.len(), 1);
    }

    /// `ImportRef` ids always embed the import's byte offset, so without a
    /// stable node key an edit above the imports resurrects every one of them —
    /// and drags the `imports` edges pointing at them along too.
    #[test]
    fn shifting_bytes_does_not_churn_an_import_or_its_edge() {
        let import = |offset: usize| Node {
            python: None,
            id: format!("rust:importref:a.rs#std::io::Write@{offset}"),
            kind: NodeKind::ImportRef,
            name: "std::io::Write".into(),
            symbol_key: "rust:importref:a.rs#std::io::Write".into(),
            provenance: Provenance {
                repository: "r".into(),
                commit_sha: "c".into(),
                rel_path: "a.rs".into(),
                span: Span::from_ts(offset, offset + 20, (0, 0), (0, 20)),
                file_hash: "h".into(),
                extractor_version: "0.1.0".into(),
                fact_source: FactSource::TreeSitter,
                confidence: 255,
            },
        };
        let imports_edge = |offset: usize| {
            let mut e = edge_at(
                &format!(
                    "imports:rust:file:a.rs->rust:importref:a.rs#std::io::Write@{offset}@{offset}"
                ),
                "rust:file:a.rs",
                &format!("rust:importref:a.rs#std::io::Write@{offset}"),
                offset,
            );
            e.kind = EdgeKind::Imports;
            e
        };

        let base = snap("AAA", vec![import(100)], vec![imports_edge(100)]);
        let head = snap("BBB", vec![import(142)], vec![imports_edge(142)]);
        let d = SnapshotDiff::between(&base, &head);

        assert!(d.added_nodes.is_empty(), "{:?}", d.added_nodes);
        assert!(d.removed_nodes.is_empty(), "{:?}", d.removed_nodes);
        assert!(d.added_edges.is_empty(), "{:?}", d.added_edges);
        assert!(d.removed_edges.is_empty(), "{:?}", d.removed_edges);
    }

    /// Repeated names in one file are still distinct nodes: the first keeps the
    /// plain id and later ones carry `@offset`, and both map into one family.
    #[test]
    fn repeated_names_in_a_file_keep_one_key_each() {
        let mut second = node("rust:a.rs#foo@50", "foo", 50, "h1");
        second.symbol_key = "rust:a.rs#foo".into();
        let base = snap(
            "AAA",
            vec![node("rust:a.rs#foo", "foo", 0, "h1"), second],
            vec![],
        );
        let keys = stable_node_keys(&base);
        assert_eq!(
            keys["rust:a.rs#foo@50"], "rust:a.rs#foo#0",
            "the disambiguated id becomes an occurrence of its base"
        );

        // Dropping the repeat is one removal, not a reshuffle of both.
        let head = snap("BBB", vec![node("rust:a.rs#foo", "foo", 0, "h1")], vec![]);
        let d = SnapshotDiff::between(&base, &head);
        assert!(d.added_nodes.is_empty(), "{:?}", d.added_nodes);
        assert_eq!(d.removed_nodes.len(), 1);
    }

    fn node_in(rel_path: &str, name: &str, kind: NodeKind, hash: &str) -> Node {
        Node {
            python: None,
            id: format!("rust:{}:{rel_path}#{name}", kind.tag()),
            kind,
            name: name.into(),
            symbol_key: format!("rust:{rel_path}#{name}"),
            provenance: Provenance {
                repository: "r".into(),
                commit_sha: "c".into(),
                rel_path: rel_path.into(),
                span: Span::from_ts(0, 10, (0, 0), (0, 10)),
                file_hash: hash.into(),
                extractor_version: "0.1.0".into(),
                fact_source: FactSource::TreeSitter,
                confidence: 255,
            },
        }
    }

    /// A symbol relocated to another file is one move, not a death and a birth.
    #[test]
    fn a_symbol_moved_between_files_pairs_as_one_move() {
        let base = snap(
            "AAA",
            vec![node_in("src/a.rs", "parse", NodeKind::Function, "h1")],
            vec![],
        );
        let head = snap(
            "BBB",
            vec![node_in("src/b.rs", "parse", NodeKind::Function, "h2")],
            vec![],
        );
        let moves = infer_moves(&SnapshotDiff::between(&base, &head));
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].from.provenance.rel_path, "src/a.rs");
        assert_eq!(moves[0].to.provenance.rel_path, "src/b.rs");
    }

    /// Two symbols of one name moving in the same commit cannot be told apart,
    /// so neither is paired — the same rule the truth boundary applies to
    /// ambiguous call targets.
    #[test]
    fn an_ambiguous_name_is_not_paired() {
        let base = snap(
            "AAA",
            vec![
                node_in("src/a.rs", "new", NodeKind::Function, "h1"),
                node_in("src/b.rs", "new", NodeKind::Function, "h1"),
            ],
            vec![],
        );
        let head = snap(
            "BBB",
            vec![
                node_in("src/c.rs", "new", NodeKind::Function, "h2"),
                node_in("src/d.rs", "new", NodeKind::Function, "h2"),
            ],
            vec![],
        );
        assert!(infer_moves(&SnapshotDiff::between(&base, &head)).is_empty());
    }

    /// A renamed file whose contents did not change pairs on its hash.
    #[test]
    fn a_renamed_file_pairs_on_its_hash() {
        let base = snap(
            "AAA",
            vec![node_in("src/old.rs", "src/old.rs", NodeKind::File, "same")],
            vec![],
        );
        let head = snap(
            "BBB",
            vec![node_in("src/new.rs", "src/new.rs", NodeKind::File, "same")],
            vec![],
        );
        let moves = infer_moves(&SnapshotDiff::between(&base, &head));
        assert_eq!(
            moves.len(),
            1,
            "identical contents at a new path is a rename"
        );
        assert_eq!(moves[0].to.provenance.rel_path, "src/new.rs");
    }

    /// A symbol that stays put is not a move, however much its body changed.
    #[test]
    fn a_symbol_that_did_not_move_is_not_paired() {
        let base = snap(
            "AAA",
            vec![node_in("src/a.rs", "parse", NodeKind::Function, "h1")],
            vec![],
        );
        let head = snap(
            "BBB",
            vec![node_in("src/a.rs", "parse", NodeKind::Function, "h2")],
            vec![],
        );
        assert!(infer_moves(&SnapshotDiff::between(&base, &head)).is_empty());
    }

    #[test]
    fn hotspots_rank_by_edge_churn() {
        let base = snap(
            "AAA",
            vec![
                node("rust:a.rs#foo", "foo", 0, "h1"),
                node("rust:a.rs#bar", "bar", 50, "h1"),
            ],
            vec![],
        );
        let head = snap(
            "BBB",
            vec![
                node("rust:a.rs#foo", "foo", 0, "h1"),
                node("rust:a.rs#bar", "bar", 50, "h1"),
            ],
            vec![
                edge("e1", "rust:a.rs#foo", "rust:a.rs#bar"),
                edge("e2", "rust:a.rs#foo", "rust:a.rs#bar"),
            ],
        );
        let d = SnapshotDiff::between(&base, &head);
        let hot = d.hotspots(5);
        // foo touches both added edges (as from), bar touches both (as to).
        let foo = hot.iter().find(|h| h.name == "rust:a.rs#foo").unwrap();
        assert_eq!(foo.added_edges, 2);
        assert_eq!(foo.total_change, 2);
    }
}
