//! Layer 1.5 heuristic analysis: derived, **non-authoritative** signals over
//! the deterministic Layer 0 graph and the cross-commit diff.
//!
//! Everything here is an *interpretation* of exact facts — never a fact
//! itself. It never creates an edge, never mutates a snapshot, and its
//! outputs (hotspots, clusters, processes, refactor signals) are separate
//! types that reference snapshot node ids by string. A consumer may use them
//! for triage; it must not ingest them as structural truth.
//!
//! Determinism: clustering uses seeded label propagation with sorted
//! iteration and deterministic tie-breaks, so the same snapshot + options
//! yield the same clusters. All outputs are sorted.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use crate::diff::SnapshotDiff;
use crate::nav::Graph;
use crate::schema::{EdgeKind, Measure, Node, NodeKind};

/// Per-node deterministic metrics used by the heuristics.
#[derive(Debug, Clone, Copy)]
pub struct NodeMetrics {
    pub fan_in: usize,
    pub fan_out: usize,
    pub calls_in: usize,
    #[allow(dead_code)]
    pub calls_out: usize,
    pub size_bytes: usize,
}

impl NodeMetrics {
    /// A coupling weight: fan-in × fan-out (a hub has high both). Used to rank
    /// "heaviest bits"; not a fact.
    pub const fn coupling(&self) -> usize {
        self.fan_in * self.fan_out
    }
}

/// A "heaviest bit" hotspot — ranked by coupling × churn. Non-authoritative.
#[derive(Debug, Clone)]
pub struct Hotspot {
    pub node: String,
    pub name: String,
    pub kind: String,
    pub fan_in: usize,
    pub fan_out: usize,
    pub size_bytes: usize,
    pub churn: usize,
    pub score: usize,
}

/// A refactor-opportunity signal. Non-authoritative — a suggestion, not a fact.
#[derive(Debug, Clone)]
pub struct RefactorSignal {
    pub kind: SignalKind,
    pub node: String,
    pub detail: String,
}

/// The kind of refactor signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    /// Part of a call-graph cycle (mutual dependency).
    Cycle,
    /// High fan-in hub many callers depend on.
    Hub,
    /// A callable whose measured cyclomatic complexity is above the cutoff
    /// or whose verified call fan-out reaches the hub threshold; without
    /// measures, a large span or high fan-out over every edge kind.
    GodSymbol,
    /// Defined but never called in-repo (and not an obvious entry/file/module).
    Orphan,
    /// A call cycle newly introduced between two commits (from a diff).
    NewCycle,
    /// A node whose fan-in grew between two commits (from a diff).
    GrowingHub,
    /// A type many others inherit from, include, or extend: a base, listed
    /// apart from call hubs so a framework root is never read as a
    /// refactor target for its callers.
    Base,
    /// One side of a twin pair (`extract --measures --twins`): a callable
    /// whose token shingles overlap another's past the producer's
    /// threshold, the copy-then-edit shape clone groups cannot see.
    Twin,
}

/// A functional cluster from seeded label propagation. Non-authoritative.
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    pub label: String,
    pub members: Vec<String>,
    pub cohesion: f64,
}

/// An execution-flow process traced from an entry point. Non-authoritative.
#[derive(Debug, Clone)]
pub struct Process {
    pub entry: String,
    pub steps: Vec<String>,
}

/// Compute per-node metrics from a graph.
pub fn metrics(graph: &Graph) -> BTreeMap<String, NodeMetrics> {
    let mut out: BTreeMap<String, NodeMetrics> = BTreeMap::new();
    for n in graph.nodes() {
        let size = n
            .provenance
            .span
            .end_byte
            .saturating_sub(n.provenance.span.start_byte);
        out.insert(
            n.id.clone(),
            NodeMetrics {
                fan_in: graph.fan_in(&n.id, &[]),
                fan_out: graph.fan_out(&n.id, &[]),
                calls_in: graph.fan_in(&n.id, &[EdgeKind::Calls]),
                calls_out: graph.fan_out(&n.id, &[EdgeKind::Calls]),
                size_bytes: size,
            },
        );
    }
    out
}

/// Rank the "heaviest bits": symbols by coupling, optionally multiplied by
/// churn from a diff. `top` limits the result. Non-authoritative.
pub fn hotspots(graph: &Graph, diff: Option<&SnapshotDiff>, top: usize) -> Vec<Hotspot> {
    let m = metrics(graph);
    // churn per node from the diff (added+removed edges touching the node).
    let churn: HashMap<&str, usize> = diff.map_or_else(HashMap::new, |d| {
        let mut c: HashMap<&str, usize> = HashMap::new();
        for e in d.added_edges.iter().chain(d.removed_edges.iter()) {
            *c.entry(e.from.as_str()).or_default() += 1;
            *c.entry(e.to.as_str()).or_default() += 1;
        }
        c
    });
    let mut hot: Vec<Hotspot> = m
        .iter()
        .filter_map(|(id, mt)| {
            // Skip non-symbol nodes (files, import refs) — they aren't
            // refactor targets.
            let n = graph.node(id)?;
            if matches!(n.kind, NodeKind::File | NodeKind::ImportRef) {
                return None;
            }
            let ch = churn.get(id.as_str()).copied().unwrap_or(0);
            // score = coupling * (churn + 1) so churn amplifies hotspots.
            let score = mt.coupling().saturating_mul(ch + 1);
            Some(Hotspot {
                node: id.clone(),
                name: n.name.clone(),
                kind: n.kind.to_string(),
                fan_in: mt.fan_in,
                fan_out: mt.fan_out,
                size_bytes: mt.size_bytes,
                churn: ch,
                score,
            })
        })
        .collect();
    hot.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.coupling_rank().cmp(&a.coupling_rank()))
            .then(a.node.cmp(&b.node))
    });
    hot.truncate(top);
    hot
}

impl Hotspot {
    const fn coupling_rank(&self) -> usize {
        self.fan_in * self.fan_out
    }
}

/// Detect refactor-opportunity signals from a graph, optionally informed by a
/// cross-commit diff (for cycle-creep and growing hubs). Non-authoritative.
///
/// A hub is verified call fan-in at or above `hub_threshold`; fan-in over
/// `inherits`, `implements`, and `extends` is a [`SignalKind::Base`]
/// instead. When the snapshot carries measures, a callable is a god
/// symbol on `cc > god_cc` or call fan-out at or above `hub_threshold`;
/// without measures the span rule (`god_size_bytes`) and fan-out over
/// every edge kind stand.
#[allow(clippy::too_many_lines)] // one pass classifies every signal kind
pub fn refactor_signals(
    graph: &Graph,
    base: Option<&Graph>,
    diff: Option<&SnapshotDiff>,
    hub_threshold: usize,
    god_size_bytes: usize,
    god_cc: u32,
) -> Vec<RefactorSignal> {
    let mut signals: Vec<RefactorSignal> = Vec::new();
    // Cycles (reuse the deterministic SCC finder).
    for cyc in graph.cycles(&[EdgeKind::Calls]) {
        for node in &cyc {
            signals.push(RefactorSignal {
                kind: SignalKind::Cycle,
                node: node.clone(),
                detail: format!("in {}-node call cycle", cyc.len()),
            });
        }
    }

    let measures: HashMap<&str, &Measure> = graph
        .snapshot()
        .measures
        .iter()
        .map(|measure| (measure.node_id.as_str(), measure))
        .collect();
    let m = metrics(graph);
    for (id, mt) in &m {
        let Some(n) = graph.node(id) else {
            continue;
        };
        if matches!(n.kind, NodeKind::File | NodeKind::ImportRef) {
            continue;
        }
        // Hubs: many verified callers.
        if mt.calls_in >= hub_threshold {
            signals.push(RefactorSignal {
                kind: SignalKind::Hub,
                node: id.clone(),
                detail: format!("call fan-in {}", mt.calls_in),
            });
        }
        // Bases: many types inherit, include, or extend this one.
        let inherited = graph.fan_in(
            id,
            &[EdgeKind::Inherits, EdgeKind::Implements, EdgeKind::Extends],
        );
        if inherited >= hub_threshold {
            signals.push(RefactorSignal {
                kind: SignalKind::Base,
                node: id.clone(),
                detail: format!("inherited, included, or extended by {inherited}"),
            });
        }
        // God-symbols: measured complexity over the cutoff or many verified
        // callees; without measures, a large span or high fan-out.
        match measures.get(id.as_str()) {
            Some(measure) => {
                if measure.cc > god_cc || mt.calls_out >= hub_threshold {
                    signals.push(RefactorSignal {
                        kind: SignalKind::GodSymbol,
                        node: id.clone(),
                        detail: format!(
                            "cc {}  sloc {}  call fan-out {}",
                            measure.cc, measure.sloc, mt.calls_out
                        ),
                    });
                }
            }
            None => {
                if mt.size_bytes >= god_size_bytes || mt.fan_out >= hub_threshold {
                    signals.push(RefactorSignal {
                        kind: SignalKind::GodSymbol,
                        node: id.clone(),
                        detail: format!("size {}B fan-out {}", mt.size_bytes, mt.fan_out),
                    });
                }
            }
        }
        // Orphans: defined, never called in-repo, and not an entry/file/module.
        if mt.calls_in == 0
            && matches!(n.kind, NodeKind::Function | NodeKind::Method)
            && !is_likely_entry(&n.name)
        {
            signals.push(RefactorSignal {
                kind: SignalKind::Orphan,
                node: id.clone(),
                detail: "no in-repo callers (uncalled definition)".into(),
            });
        }
    }

    // Base-informed signals: use the actual base-commit graph (when provided)
    // for accurate new-cycle and growing-hub detection. Calls-based diff
    // signals are ONLY emitted when the base snapshot itself has calls edges
    // (semantic base); a syntax-only base has no calls edges, so emitting
    // "new cycle"/"growing hub" would be false positives — skip them.
    if let Some(base_g) = base {
        let base_has_calls = base_g
            .snapshot()
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls);
        let _ = diff; // churn hotspots use diff separately; signals use base graph
        if base_has_calls {
            let base_cycles: BTreeSet<BTreeSet<String>> = base_g
                .cycles(&[EdgeKind::Calls])
                .into_iter()
                .map(|c| c.into_iter().collect())
                .collect();
            for cyc in graph.cycles(&[EdgeKind::Calls]) {
                let set: BTreeSet<String> = cyc.iter().cloned().collect();
                if !base_cycles.contains(&set) && !set.is_empty() {
                    for node in &cyc {
                        signals.push(RefactorSignal {
                            kind: SignalKind::NewCycle,
                            node: node.clone(),
                            detail: format!("new {}-node cycle since base", cyc.len()),
                        });
                    }
                }
            }
            let base_m = metrics(base_g);
            for (id, mt) in &m {
                let before = base_m.get(id).map_or(0, |b| b.calls_in);
                if mt.calls_in > before && mt.calls_in >= hub_threshold {
                    signals.push(RefactorSignal {
                        kind: SignalKind::GrowingHub,
                        node: id.clone(),
                        detail: format!("call fan-in {} -> {}", before, mt.calls_in),
                    });
                }
            }
        }
    }

    // Twins: read from the snapshot when it carries them; both sides of a
    // pair get the signal so either lookup shows it.
    for pair in &graph.snapshot().twin_pairs {
        let percent = (pair.containment() * 100.0).round();
        for (side, other) in [(&pair.a, &pair.b), (&pair.b, &pair.a)] {
            signals.push(RefactorSignal {
                kind: SignalKind::Twin,
                node: side.clone(),
                detail: format!(
                    "shares {percent}% of its token shingles with {other}{}",
                    if pair.is_exact() { " (exact)" } else { "" }
                ),
            });
        }
    }

    signals.sort_by(|a, b| {
        (a.kind as usize)
            .cmp(&(b.kind as usize))
            .then(a.node.cmp(&b.node))
    });
    signals
}

/// Heuristic: is `name` likely an external entry point (so "uncalled" is
/// expected, not an orphan smell)?
fn is_likely_entry(name: &str) -> bool {
    matches!(
        name,
        "main" | "run" | "new" | "default" | "drop" | "from" | "into" | "fmt"
    ) || name.starts_with("pub_")
}

/// Seeded label-propagation clustering over the call/import/contains graph.
/// Deterministic: nodes iterated in sorted id order, ties broken toward the
/// smallest label. Non-authoritative.
pub fn clusters(graph: &Graph, max_rounds: u32) -> Vec<Cluster> {
    let snap = graph.snapshot();
    let nodes: Vec<&Node> = snap
        .nodes
        .iter()
        .filter(|n| !matches!(n.kind, NodeKind::File | NodeKind::ImportRef))
        .collect();
    let order: Vec<String> = {
        let mut o: Vec<&Node> = nodes.clone();
        o.sort_by(|a, b| a.id.cmp(&b.id));
        o.into_iter().map(|n| n.id.clone()).collect()
    };
    let mut label: HashMap<String, usize> = order
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i))
        .collect();
    let edge_kinds = [
        EdgeKind::Calls,
        EdgeKind::Contains,
        EdgeKind::Imports,
        EdgeKind::Implements,
        EdgeKind::Extends,
    ];
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &snap.edges {
        if !edge_kinds.contains(&edge.kind) {
            continue;
        }
        adjacency
            .entry(edge.from.as_str())
            .or_default()
            .push(edge.to.as_str());
        if edge.from != edge.to {
            adjacency
                .entry(edge.to.as_str())
                .or_default()
                .push(edge.from.as_str());
        }
    }

    for _ in 0..max_rounds {
        let mut changed = false;
        for id in &order {
            let mut neigh: Vec<usize> = Vec::new();
            for &neighbor in adjacency.get(id.as_str()).into_iter().flatten() {
                if let Some(&neighbor_label) = label.get(neighbor) {
                    neigh.push(neighbor_label);
                }
            }
            if neigh.is_empty() {
                continue;
            }
            let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
            for l in &neigh {
                *counts.entry(*l).or_default() += 1;
            }
            let best = counts
                .into_iter()
                .max_by_key(|(_, c)| *c)
                .map_or(label[id], |(l, _)| l);
            if best != label[id] {
                label.insert(id.clone(), best);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut groups: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for id in &order {
        groups.entry(label[id]).or_default().push(id.clone());
    }
    let mut out: Vec<Cluster> = groups
        .into_iter()
        .map(|(label_idx, mut members)| {
            members.sort();
            let cohesion = cluster_cohesion(graph, &members, &edge_kinds);
            Cluster {
                label: format!("cluster-{label_idx}"),
                members,
                cohesion,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then(a.label.cmp(&b.label))
    });
    out
}

/// Cohesion: fraction of a cluster's edges that stay within the cluster.
#[allow(clippy::cast_precision_loss)]
fn cluster_cohesion(graph: &Graph, members: &[String], kinds: &[EdgeKind]) -> f64 {
    let set: HashSet<&str> = members.iter().map(std::string::String::as_str).collect();
    let mut within = 0usize;
    let mut total = 0usize;
    for e in &graph.snapshot().edges {
        if !kinds.contains(&e.kind) {
            continue;
        }
        let f = set.contains(e.from.as_str());
        let t = set.contains(e.to.as_str());
        if f || t {
            total += 1;
            if f && t {
                within += 1;
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        within as f64 / total as f64
    }
}

/// Trace execution-flow "processes" from heuristically-detected entry points
/// (functions/methods with zero incoming call edges), following `calls` edges
/// downstream. Non-authoritative.
pub fn processes(graph: &Graph, max_steps: usize) -> Vec<Process> {
    let snap = graph.snapshot();
    // Entry candidates: functions/methods with no in-repo callers.
    let entries: Vec<&Node> = snap
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .filter(|n| graph.fan_in(&n.id, &[EdgeKind::Calls]) == 0)
        .collect();
    let mut out: Vec<Process> = Vec::new();
    for entry in entries {
        let mut steps: Vec<String> = vec![entry.id.clone()];
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(entry.id.clone());
        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back(entry.id.clone());
        while let Some(cur) = queue.pop_front() {
            if steps.len() >= max_steps {
                break;
            }
            let downstream = graph.impact(
                &cur,
                crate::nav::Direction::Downstream,
                &[EdgeKind::Calls],
                1,
            );
            for n in downstream {
                if visited.insert(n.node.clone()) {
                    steps.push(n.node.clone());
                    queue.push_back(n.node);
                    if steps.len() >= max_steps {
                        break;
                    }
                }
            }
        }
        steps.sort();
        out.push(Process {
            entry: entry.id.clone(),
            steps,
        });
    }
    out.sort_by(|a, b| a.entry.cmp(&b.entry));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        Edge, EdgeKind, FactSource, FileRecord, Language, NodeKind, Provenance, RepoIdentity,
        Report, Snapshot, SnapshotIdentity, Span, ToolVersions,
    };

    fn snap(nodes: Vec<Node>, edges: Vec<Edge>) -> Snapshot {
        let repo = RepoIdentity {
            id: "r".into(),
            origin: None,
            is_canonical: false,
        };
        let identity = SnapshotIdentity::build(
            Language::Rust,
            repo,
            "c".into(),
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

    fn node(id: &str, name: &str, kind: NodeKind, se: usize, ee: usize) -> Node {
        Node {
            python: None,
            id: id.into(),
            kind,
            name: name.into(),
            symbol_key: format!("rust:a.rs#{name}"),
            provenance: Provenance {
                repository: "r".into(),
                commit_sha: "c".into(),
                rel_path: "a.rs".into(),
                span: Span::from_ts(se, ee, (0, se), (0, ee)),
                file_hash: "h".into(),
                extractor_version: "0.1.0".into(),
                fact_source: FactSource::RustAnalyzer,
                confidence: 255,
            },
        }
    }

    fn edge(id: &str, from: &str, to: &str) -> Edge {
        Edge {
            id: id.into(),
            kind: EdgeKind::Calls,
            from: from.into(),
            to: to.into(),
            provenance: Provenance {
                repository: "r".into(),
                commit_sha: "c".into(),
                rel_path: "a.rs".into(),
                span: Span::from_ts(0, 5, (0, 0), (0, 5)),
                file_hash: "h".into(),
                extractor_version: "0.1.0".into(),
                fact_source: FactSource::RustAnalyzer,
                confidence: 255,
            },
        }
    }

    #[test]
    fn hotspots_rank_by_coupling_times_churn() {
        // foo: fan_in=2 fan_out=1 -> coupling 2. bar: fan_in=1 fan_out=0 -> 0.
        let s = snap(
            vec![
                node("a", "a", NodeKind::File, 0, 100),
                node("foo", "foo", NodeKind::Function, 0, 50),
                node("bar", "bar", NodeKind::Function, 60, 90),
                node("baz", "baz", NodeKind::Function, 100, 120),
            ],
            vec![
                edge("e1", "bar", "foo"),
                edge("e2", "baz", "foo"),
                edge("e3", "foo", "bar"),
            ],
        );
        let g = Graph::from_snapshot(&s);
        let hot = hotspots(&g, None, 5);
        assert!(!hot.is_empty());
        assert_eq!(hot[0].name, "foo", "foo has highest coupling");
    }

    #[test]
    fn refactor_signals_find_orphans_and_hubs() {
        // entry: no callers. hub: called by 3. orphan: uncalled, not entry.
        let s = snap(
            vec![
                node("a", "a", NodeKind::File, 0, 100),
                node("entry", "entry", NodeKind::Function, 0, 10),
                node("hub", "hub", NodeKind::Function, 20, 30),
                node("c1", "c1", NodeKind::Function, 40, 50),
                node("c2", "c2", NodeKind::Function, 60, 70),
                node("c3", "c3", NodeKind::Function, 80, 90),
                node("orphan", "orphan", NodeKind::Function, 100, 110),
            ],
            vec![
                edge("e1", "c1", "hub"),
                edge("e2", "c2", "hub"),
                edge("e3", "c3", "hub"),
                edge("e4", "entry", "hub"),
            ],
        );
        let g = Graph::from_snapshot(&s);
        let sigs = refactor_signals(&g, None, None, 3, 1000, 10);
        let kinds: Vec<_> = sigs.iter().map(|s| s.kind).collect();
        // hub is a Hub (fan-in 4) and an Orphan? No — hub is called, so not orphan.
        assert!(kinds.contains(&SignalKind::Hub), "hub detected: {kinds:?}");
        assert!(
            kinds.contains(&SignalKind::Orphan),
            "orphan detected: {kinds:?}"
        );
        // entry is not flagged orphan (is_likely_entry / actually it's not in the
        // likely-entry list, but it HAS no callers... entry has calls_in 0 and is
        // a Function not named main/run -> flagged orphan. That's a known
        // heuristic limit; acceptable.)
    }

    #[test]
    fn a_measured_callable_is_a_god_symbol_by_complexity_not_bytes() {
        let mut s = snap(
            vec![
                node("a", "a", NodeKind::File, 0, 100),
                node("knotty", "knotty", NodeKind::Function, 0, 200),
                node("plain", "plain", NodeKind::Function, 300, 500),
            ],
            vec![],
        );
        s.measures.push(Measure {
            node_id: "knotty".into(),
            cc: 12,
            sloc: 30,
            loc: 32,
            clone_group_ids: Vec::new(),
            params: None,
            nesting: None,
            chain: None,
            ivars: None,
        });
        s.measures.push(Measure {
            node_id: "plain".into(),
            cc: 2,
            sloc: 30,
            loc: 32,
            clone_group_ids: Vec::new(),
            params: None,
            nesting: None,
            chain: None,
            ivars: None,
        });
        let g = Graph::from_snapshot(&s);
        let gods: Vec<RefactorSignal> = refactor_signals(&g, None, None, 8, 100, 10)
            .into_iter()
            .filter(|sig| sig.kind == SignalKind::GodSymbol)
            .collect();
        let names: Vec<&str> = gods.iter().map(|sig| sig.node.as_str()).collect();
        assert_eq!(
            names,
            ["knotty"],
            "plain spans 200 bytes over a 100-byte rule but is measured at cc 2, so bytes do not decide"
        );
        assert!(
            gods[0].detail.starts_with("cc 12  sloc 30"),
            "{}",
            gods[0].detail
        );

        // Without measures the byte rule stands for both.
        s.measures.clear();
        let g = Graph::from_snapshot(&s);
        let gods = refactor_signals(&g, None, None, 8, 100, 10)
            .into_iter()
            .filter(|sig| sig.kind == SignalKind::GodSymbol)
            .count();
        assert_eq!(gods, 2);
    }

    #[test]
    fn a_base_type_is_not_a_call_hub() {
        let inherit = |id: &str, from: &str, to: &str| Edge {
            kind: EdgeKind::Inherits,
            ..edge(id, from, to)
        };
        let s = snap(
            vec![
                node("a", "a", NodeKind::File, 0, 100),
                node("Root", "Root", NodeKind::Trait, 0, 10),
                node("x", "x", NodeKind::Function, 20, 30),
                node("y", "y", NodeKind::Function, 40, 50),
                node("z", "z", NodeKind::Function, 60, 70),
            ],
            vec![
                inherit("i1", "x", "Root"),
                inherit("i2", "y", "Root"),
                inherit("i3", "z", "Root"),
            ],
        );
        let g = Graph::from_snapshot(&s);
        let sigs = refactor_signals(&g, None, None, 3, 1000, 10);
        let root: Vec<SignalKind> = sigs
            .iter()
            .filter(|sig| sig.node == "Root")
            .map(|sig| sig.kind)
            .collect();
        assert!(root.contains(&SignalKind::Base), "{root:?}");
        assert!(
            !root.contains(&SignalKind::Hub),
            "three inheritors are not three callers: {root:?}"
        );
    }

    #[test]
    fn a_twin_pair_in_the_snapshot_signals_both_sides() {
        let mut s = snap(
            vec![
                node("a", "a", NodeKind::File, 0, 100),
                node("left", "left", NodeKind::Function, 0, 40),
                node("right", "right", NodeKind::Function, 50, 90),
            ],
            vec![],
        );
        s.twin_pairs.push(crate::schema::TwinPair {
            a: "left".into(),
            b: "right".into(),
            shared: 90,
            shingles_a: 100,
            shingles_b: 120,
        });
        let g = Graph::from_snapshot(&s);
        let twins: Vec<&RefactorSignal> = refactor_signals(&g, None, None, 8, 1000, 10)
            .into_iter()
            .filter(|sig| sig.kind == SignalKind::Twin)
            .collect::<Vec<_>>()
            .leak()
            .iter()
            .collect();
        let nodes: Vec<&str> = twins.iter().map(|sig| sig.node.as_str()).collect();
        assert_eq!(nodes, ["left", "right"]);
        assert!(
            twins[0]
                .detail
                .starts_with("shares 90% of its token shingles with right"),
            "{}",
            twins[0].detail
        );
    }

    #[test]
    fn clusters_are_deterministic() {
        let s = snap(
            vec![
                node("a", "a", NodeKind::File, 0, 100),
                node("x", "x", NodeKind::Function, 0, 10),
                node("y", "y", NodeKind::Function, 20, 30),
                node("z", "z", NodeKind::Function, 40, 50),
            ],
            vec![
                edge("e1", "x", "y"),
                edge("e2", "y", "z"),
                edge("e3", "z", "x"),
            ],
        );
        let g = Graph::from_snapshot(&s);
        let c1 = clusters(&g, 10);
        let c2 = clusters(&g, 10);
        assert_eq!(c1, c2, "clustering must be deterministic");
        // The x-y-z cycle should collapse into one cluster.
        assert!(
            c1.iter().any(|c| c.members.len() == 3),
            "x,y,z cluster together"
        );
    }

    #[test]
    fn processes_trace_from_entries() {
        // entry (no callers) -> mid -> leaf.
        let s = snap(
            vec![
                node("a", "a", NodeKind::File, 0, 100),
                node("entry", "entry", NodeKind::Function, 0, 10),
                node("mid", "mid", NodeKind::Function, 20, 30),
                node("leaf", "leaf", NodeKind::Function, 40, 50),
            ],
            vec![edge("e1", "entry", "mid"), edge("e2", "mid", "leaf")],
        );
        let g = Graph::from_snapshot(&s);
        let procs = processes(&g, 50);
        let entry_proc = procs
            .iter()
            .find(|p| p.entry == "entry")
            .expect("entry process");
        assert!(entry_proc.steps.iter().any(|s| s == "mid"));
        assert!(entry_proc.steps.iter().any(|s| s == "leaf"));
    }
}
