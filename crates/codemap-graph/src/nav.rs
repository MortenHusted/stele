//! Layer 1 deterministic navigation: graph queries over a `Snapshot`.
//!
//! Everything here is **graph math over the exact Layer 0 edge set** — no
//! heuristics, no ML, no edge derivation. `impact`/`trace`/`cycles` are
//! authoritative because the edges underneath are exact. This module never
//! mutates a snapshot; it only reads and projects.
//!
//! The heuristic/interpretive layer (clustering, process tracing, refactor
//! rules, embeddings) is deliberately separate and labelled non-authoritative
//! — it will live in its own module and must never write back into a snapshot.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::Serialize;

use crate::schema::{Edge, EdgeKind, Node, NodeKind, Snapshot};

/// A read-only graph view over a snapshot, with adjacency indexes for fast
/// traversal. Borrows the snapshot for its lifetime.
pub struct Graph<'a> {
    snapshot: &'a Snapshot,
    nodes_by_id: HashMap<&'a str, &'a Node>,
    nodes_by_name: HashMap<&'a str, Vec<&'a Node>>,
    nodes_by_symbol_key: HashMap<&'a str, Vec<&'a Node>>,
    out: HashMap<&'a str, Vec<&'a Edge>>,
    inn: HashMap<&'a str, Vec<&'a Edge>>,
}

/// Which direction to traverse for impact analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Who depends on this symbol (follow incoming edges).
    Upstream,
    /// What this symbol depends on (follow outgoing edges).
    Downstream,
}

/// A node reached during impact traversal, with its BFS depth.
#[derive(Debug, Clone)]
pub struct ImpactNode {
    pub node: String,
    pub depth: u32,
    pub via: EdgeKind,
}

/// A symbol-context view: the node, its container, and grouped in/out edges.
#[derive(Debug, Clone)]
pub struct ContextView {
    pub node: String,
    pub kind: String,
    pub name: String,
    pub symbol_key: String,
    pub rel_path: String,
    /// The node that lexically contains this one (via a `contains` edge), if any.
    pub container: Option<String>,
    /// Incoming edges grouped by kind: (kind, `from_id`, `rel_path`).
    pub incoming: Vec<(String, String, String)>,
    /// Outgoing edges grouped by kind: (kind, `to_id`, `rel_path`).
    pub outgoing: Vec<(String, String, String)>,
    pub fan_in: usize,
    pub fan_out: usize,
}

/// A canonical repository-relative path and an inclusive, positive line range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceLocation {
    pub rel_path: String,
    pub start_line: u32,
    pub end_line: u32,
}

impl std::str::FromStr for SourceLocation {
    type Err = anyhow::Error;

    /// Parse `path:line[-line]` without accessing the filesystem. Paths must
    /// already use canonical repository-relative spelling with `/` separators.
    fn from_str(query: &str) -> anyhow::Result<Self> {
        let (rel_path, lines) = query
            .rsplit_once(':')
            .ok_or_else(|| anyhow::anyhow!("expected path:line[-line]"))?;
        anyhow::ensure!(
            !rel_path.contains(['\\', ':'])
                && !rel_path.chars().any(char::is_control)
                && rel_path
                    .split('/')
                    .all(|part| !matches!(part, "" | "." | "..")),
            "location path must be canonical and repository-relative"
        );
        let parse_line = |line: &str| -> anyhow::Result<u32> {
            anyhow::ensure!(
                !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit()),
                "location lines must be positive integers"
            );
            let line: u32 = line.parse()?;
            anyhow::ensure!(line > 0, "location lines must be positive integers");
            Ok(line)
        };
        let (start, end) = lines.split_once('-').unwrap_or((lines, lines));
        let start_line = parse_line(start)?;
        let end_line = parse_line(end)?;
        anyhow::ensure!(
            start_line <= end_line,
            "location range must not be reversed"
        );
        Ok(Self {
            rel_path: rel_path.into(),
            start_line,
            end_line,
        })
    }
}

impl std::fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.rel_path, self.start_line)?;
        if self.end_line != self.start_line {
            write!(f, "-{}", self.end_line)?;
        }
        Ok(())
    }
}

/// Declaration spans cannot classify individual lines as executable, comments,
/// or blanks. Containment makes no claim about semantic or business correctness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutableClassification {
    Unsupported,
}

/// Source-bound structural containment in one snapshot, never a source excerpt.
#[derive(Debug, Serialize)]
pub struct LocateResult<'a> {
    pub snapshot_id: &'a str,
    pub query: String,
    pub location: SourceLocation,
    /// Innermost matching declarations, in stable ID order. Empty means none.
    pub definitions: Vec<&'a Node>,
    pub executable_classification: ExecutableClassification,
}

impl<'a> Graph<'a> {
    /// Build a graph view from a snapshot.
    pub fn from_snapshot(snapshot: &'a Snapshot) -> Self {
        let mut nodes_by_id: HashMap<&'a str, &'a Node> = HashMap::new();
        let mut nodes_by_name: HashMap<&'a str, Vec<&'a Node>> = HashMap::new();
        let mut nodes_by_symbol_key: HashMap<&'a str, Vec<&'a Node>> = HashMap::new();
        for n in &snapshot.nodes {
            nodes_by_id.insert(n.id.as_str(), n);
            nodes_by_name.entry(n.name.as_str()).or_default().push(n);
            nodes_by_symbol_key
                .entry(n.symbol_key.as_str())
                .or_default()
                .push(n);
        }
        let mut out: HashMap<&'a str, Vec<&'a Edge>> = HashMap::new();
        let mut inn: HashMap<&'a str, Vec<&'a Edge>> = HashMap::new();
        for e in &snapshot.edges {
            out.entry(e.from.as_str()).or_default().push(e);
            inn.entry(e.to.as_str()).or_default().push(e);
        }
        // Deterministic adjacency order: sort edges by (kind, start_byte).
        for v in out.values_mut() {
            v.sort_by(|a, b| {
                a.kind.tag().cmp(b.kind.tag()).then(
                    a.provenance
                        .span
                        .start_byte
                        .cmp(&b.provenance.span.start_byte),
                )
            });
        }
        for v in inn.values_mut() {
            v.sort_by(|a, b| {
                a.kind.tag().cmp(b.kind.tag()).then(
                    a.provenance
                        .span
                        .start_byte
                        .cmp(&b.provenance.span.start_byte),
                )
            });
        }
        Self {
            snapshot,
            nodes_by_id,
            nodes_by_name,
            nodes_by_symbol_key,
            out,
            inn,
        }
    }

    /// Locate the innermost declarations whose recorded line coverage contains
    /// the entire inclusive query range. Use a validated snapshot.
    ///
    /// File containers and import placeholders are excluded. An end-exclusive
    /// span ending at column 0 does not cover that final line (snapshot columns
    /// are zero-based, as emitted by `Span::from_ts`). Strict byte-span
    /// nesting removes outer declarations; unrelated or equal-span candidates
    /// are retained because a line-only query cannot choose between columns.
    /// No source is read and executable classification is always unsupported.
    pub fn locate(&self, query: &str) -> anyhow::Result<LocateResult<'a>> {
        let location: SourceLocation = query.parse()?;
        let candidates: Vec<&Node> = self
            .snapshot
            .nodes
            .iter()
            .filter(|node| {
                let span = node.provenance.span;
                let last_line = span.end_line.saturating_sub(u32::from(span.end_col == 0));
                !matches!(node.kind, NodeKind::File | NodeKind::ImportRef)
                    && node.provenance.rel_path == location.rel_path
                    && span.start_byte < span.end_byte
                    && span.start_line <= location.start_line
                    && location.end_line <= last_line
            })
            .collect();
        let mut definitions: Vec<&Node> = candidates
            .iter()
            .copied()
            .filter(|outer| {
                let outer_span = outer.provenance.span;
                !candidates.iter().any(|inner| {
                    let inner_span = inner.provenance.span;
                    outer_span.start_byte <= inner_span.start_byte
                        && inner_span.end_byte <= outer_span.end_byte
                        && (outer_span.start_byte != inner_span.start_byte
                            || outer_span.end_byte != inner_span.end_byte)
                })
            })
            .collect();
        definitions.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(LocateResult {
            snapshot_id: &self.snapshot.identity.snapshot_id,
            query: location.to_string(),
            location,
            definitions,
            executable_classification: ExecutableClassification::Unsupported,
        })
    }

    /// Resolve a query string to a single node, or report ambiguity.
    ///
    /// Accepts, in this order: an exact node id; an exact `symbol_key`; a
    /// declared name; and a qualified suffix of the part of `symbol_key`
    /// after `#`, written with `::` or `#` between the segments, so
    /// `UsersController#index` and `Admin::UsersController::index`
    /// both find `ruby:…#Api::V1::Admin::UsersController::index`.
    /// Several matches at any step are returned as the ambiguity list.
    pub fn resolve(&self, query: &str) -> ResolveResult<'a> {
        if let Some(n) = self.nodes_by_id.get(query) {
            return ResolveResult::Unique(n);
        }
        match self.nodes_by_symbol_key.get(query) {
            Some(v) if v.len() == 1 => return ResolveResult::Unique(v[0]),
            Some(v) if !v.is_empty() => return ResolveResult::Ambiguous(v.clone()),
            _ => {}
        }
        match self.nodes_by_name.get(query) {
            Some(v) if v.len() == 1 => return ResolveResult::Unique(v[0]),
            Some(v) if !v.is_empty() => return ResolveResult::Ambiguous(v.clone()),
            _ => {}
        }
        if !query.contains("::") && !query.contains('#') {
            return ResolveResult::Unknown;
        }
        let wanted = query.replace('#', "::");
        let suffix = format!("::{wanted}");
        let mut hits: Vec<&'a Node> = self
            .snapshot
            .nodes
            .iter()
            .filter(|n| {
                n.symbol_key.split_once('#').is_some_and(|(_, qualified)| {
                    qualified == wanted || qualified.ends_with(suffix.as_str())
                })
            })
            .collect();
        hits.sort_by(|a, b| a.id.cmp(&b.id));
        match hits.len() {
            0 => ResolveResult::Unknown,
            1 => ResolveResult::Unique(hits[0]),
            _ => ResolveResult::Ambiguous(hits),
        }
    }

    /// Find nodes whose name or `symbol_key` contains `needle` (case-sensitive).
    pub fn search(&self, needle: &str) -> Vec<&'a Node> {
        let mut hits: Vec<&'a Node> = self
            .snapshot
            .nodes
            .iter()
            .filter(|n| n.name.contains(needle) || n.symbol_key.contains(needle))
            .collect();
        hits.sort_by(|a, b| a.id.cmp(&b.id));
        hits
    }

    /// 360-degree context for a node.
    pub fn context(&self, id: &str) -> Option<ContextView> {
        let n = self.nodes_by_id.get(id)?;
        let container = self
            .inn
            .get(id)
            .into_iter()
            .flatten()
            .find(|e| e.kind == EdgeKind::Contains)
            .map(|e| e.from.clone());
        let incoming = self
            .inn
            .get(id)
            .into_iter()
            .flatten()
            .map(|e| {
                (
                    e.kind.tag().to_string(),
                    e.from.clone(),
                    e.provenance.rel_path.clone(),
                )
            })
            .collect();
        let outgoing = self
            .out
            .get(id)
            .into_iter()
            .flatten()
            .map(|e| {
                (
                    e.kind.tag().to_string(),
                    e.to.clone(),
                    e.provenance.rel_path.clone(),
                )
            })
            .collect();
        let fan_in = self.inn.get(id).map_or(0, std::vec::Vec::len);
        let fan_out = self.out.get(id).map_or(0, std::vec::Vec::len);
        Some(ContextView {
            node: n.id.clone(),
            kind: n.kind.to_string(),
            name: n.name.clone(),
            symbol_key: n.symbol_key.clone(),
            rel_path: n.provenance.rel_path.clone(),
            container,
            incoming,
            outgoing,
            fan_in,
            fan_out,
        })
    }

    /// Transitive impact: BFS over edges in `direction`, optionally filtered
    /// to a set of edge kinds (empty = all kinds). Returns reached nodes
    /// grouped by BFS depth, excluding the start node.
    pub fn impact(
        &self,
        start: &str,
        direction: Direction,
        edge_kinds: &[EdgeKind],
        max_depth: u32,
    ) -> Vec<ImpactNode> {
        let mut visited: HashSet<&str> = HashSet::new();
        visited.insert(start);
        let mut queue: VecDeque<(&str, u32)> = VecDeque::new();
        queue.push_back((start, 0));
        let mut result: Vec<ImpactNode> = Vec::new();
        while let Some((cur, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let edges: Vec<&Edge> = match direction {
                Direction::Upstream => self.inn.get(cur).into_iter().flatten().copied().collect(),
                Direction::Downstream => self.out.get(cur).into_iter().flatten().copied().collect(),
            };
            for e in edges {
                if !edge_kinds.is_empty() && !edge_kinds.contains(&e.kind) {
                    continue;
                }
                let next = match direction {
                    Direction::Upstream => e.from.as_str(),
                    Direction::Downstream => e.to.as_str(),
                };
                if visited.insert(next) {
                    result.push(ImpactNode {
                        node: next.to_string(),
                        depth: depth + 1,
                        via: e.kind,
                    });
                    queue.push_back((next, depth + 1));
                }
            }
        }
        // Deterministic order: by (depth, node id).
        result.sort_by(|a, b| a.depth.cmp(&b.depth).then(a.node.cmp(&b.node)));
        result
    }

    /// Shortest path from `from` to `to` along edges of the given kinds
    /// (empty = all kinds). Returns the node-id path inclusive, or `None`.
    pub fn trace(&self, from: &str, to: &str, edge_kinds: &[EdgeKind]) -> Option<Vec<String>> {
        if from == to {
            return Some(vec![from.to_string()]);
        }
        let mut prev: HashMap<&str, &str> = HashMap::new();
        let mut visited: HashSet<&str> = HashSet::new();
        visited.insert(from);
        let mut queue: VecDeque<&str> = VecDeque::new();
        queue.push_back(from);
        while let Some(cur) = queue.pop_front() {
            for e in self.out.get(cur).into_iter().flatten() {
                if !edge_kinds.is_empty() && !edge_kinds.contains(&e.kind) {
                    continue;
                }
                let next = e.to.as_str();
                if visited.insert(next) {
                    prev.insert(next, cur);
                    if next == to {
                        // Reconstruct.
                        let mut path = vec![to.to_string()];
                        let mut step = to;
                        while let Some(&p) = prev.get(step) {
                            path.push(p.to_string());
                            step = p;
                        }
                        path.reverse();
                        return Some(path);
                    }
                    queue.push_back(next);
                }
            }
        }
        None
    }

    /// Strongly-connected components of size > 1 (and self-loops) over the
    /// edge subgraph filtered to `edge_kinds` (empty = all kinds). Each
    /// returned cycle is a sorted vec of node ids — a refactor signal
    /// (mutual dependency). Uses iterative Tarjan.
    pub fn cycles(&self, edge_kinds: &[EdgeKind]) -> Vec<Vec<String>> {
        let adjacency: HashMap<&str, Vec<&str>> = self
            .out
            .iter()
            .map(|(&id, edges)| {
                let neighbors = edges
                    .iter()
                    .copied()
                    .filter(|e| edge_kinds.is_empty() || edge_kinds.contains(&e.kind))
                    .map(|e| e.to.as_str())
                    .collect();
                (id, neighbors)
            })
            .collect();
        let adj = |id: &str| adjacency.get(id).map_or(&[][..], Vec::as_slice);
        let ids: Vec<&str> = self.nodes_by_id.keys().copied().collect();
        // Iterative Tarjan SCC.
        let mut index = 0i32;
        let mut indices: HashMap<&str, i32> = HashMap::new();
        let mut lowlink: HashMap<&str, i32> = HashMap::new();
        let mut on_stack: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = Vec::new();
        let mut sccs: Vec<Vec<String>> = Vec::new();

        for &root in &ids {
            if indices.contains_key(root) {
                continue;
            }
            // (node, child iterator state: index into adjacency)
            let mut work: Vec<(&str, usize)> = vec![(root, 0)];
            while let Some((v, i)) = work.last_mut() {
                let v = *v;
                if !indices.contains_key(v) {
                    indices.insert(v, index);
                    lowlink.insert(v, index);
                    index += 1;
                    stack.push(v);
                    on_stack.insert(v);
                }
                let neighbors = adj(v);
                if *i < neighbors.len() {
                    let w = neighbors[*i];
                    *i += 1;
                    if !indices.contains_key(w) {
                        work.push((w, 0));
                    } else if on_stack.contains(w) {
                        let lw = lowlink[&w];
                        let lv = lowlink[&v];
                        lowlink.insert(v, lv.min(lw));
                    }
                } else {
                    // Done with v.
                    if lowlink[&v] == indices[&v] {
                        let mut comp: Vec<&str> = Vec::new();
                        loop {
                            let w = stack.pop().expect("stack non-empty");
                            on_stack.remove(w);
                            comp.push(w);
                            if w == v {
                                break;
                            }
                        }
                        // Report as a cycle if size > 1, or a self-loop exists.
                        let has_self_loop =
                            comp.len() == 1 && adj(comp[0]).iter().any(|&n| n == comp[0]);
                        if comp.len() > 1 || has_self_loop {
                            let mut c: Vec<String> =
                                comp.iter().map(std::string::ToString::to_string).collect();
                            c.sort();
                            sccs.push(c);
                        }
                    }
                    work.pop();
                    if let Some(&(parent, _)) = work.last() {
                        let lv = lowlink[&v];
                        let lp = lowlink[&parent];
                        lowlink.insert(parent, lp.min(lv));
                    }
                }
            }
        }
        sccs.sort();
        sccs
    }

    /// Containment children of `parent_id` (via `contains` edges).
    pub fn children(&self, parent_id: &str) -> Vec<&'a Node> {
        let mut kids: Vec<&'a Node> = self
            .out
            .get(parent_id)
            .into_iter()
            .flatten()
            .filter(|e| e.kind == EdgeKind::Contains)
            .filter_map(|e| self.nodes_by_id.get(e.to.as_str()).copied())
            .collect();
        kids.sort_by(|a, b| a.id.cmp(&b.id));
        kids
    }

    /// Fan-in: number of incoming edges (optionally filtered by kind).
    pub fn fan_in(&self, id: &str, kinds: &[EdgeKind]) -> usize {
        self.inn
            .get(id)
            .into_iter()
            .flatten()
            .filter(|e| kinds.is_empty() || kinds.contains(&e.kind))
            .count()
    }

    /// Fan-out: number of outgoing edges (optionally filtered by kind).
    pub fn fan_out(&self, id: &str, kinds: &[EdgeKind]) -> usize {
        self.out
            .get(id)
            .into_iter()
            .flatten()
            .filter(|e| kinds.is_empty() || kinds.contains(&e.kind))
            .count()
    }

    /// Look up a node by its exact id.
    pub(crate) fn node(&self, id: &str) -> Option<&'a Node> {
        self.nodes_by_id.get(id).copied()
    }

    /// Iterate all nodes in deterministic (id-sorted) order.
    pub fn nodes(&self) -> impl Iterator<Item = &'a Node> {
        let mut ns: Vec<&'a Node> = self.snapshot.nodes.iter().collect();
        ns.sort_by(|a, b| a.id.cmp(&b.id));
        ns.into_iter()
    }

    /// Access the underlying snapshot.
    pub const fn snapshot(&self) -> &'a Snapshot {
        self.snapshot
    }
}

/// Result of resolving a query string to a node.
#[derive(Debug)]
pub enum ResolveResult<'a> {
    Unique(&'a Node),
    /// Every node the query matched, in id order.
    Ambiguous(Vec<&'a Node>),
    Unknown,
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        Edge, EdgeKind, FactSource, FileRecord, Language, NodeKind, Provenance, RepoIdentity,
        Report, SnapshotIdentity, Span, ToolVersions,
    };

    #[allow(clippy::too_many_lines)]
    fn snap() -> Snapshot {
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
        let prov = |path: &str, se: usize, ee: usize| Provenance {
            repository: "r".into(),
            commit_sha: "c".into(),
            rel_path: path.into(),
            span: Span::from_ts(se, ee, (0, 0), (0, ee)),
            file_hash: "h".into(),
            extractor_version: identity.extractor_version.clone(),
            fact_source: FactSource::TreeSitter,
            confidence: 255,
        };
        let n = |id: &str, kind: NodeKind, name: &str, key: &str, path: &str, se: usize| Node {
            python: None,
            id: id.into(),
            kind,
            name: name.into(),
            symbol_key: key.into(),
            provenance: prov(path, se, se + 10),
        };
        let e = |id: &str, kind: EdgeKind, from: &str, to: &str, se: usize, path: &str| Edge {
            id: id.into(),
            kind,
            from: from.into(),
            to: to.into(),
            provenance: prov(path, se, se + 5),
        };
        let nodes = vec![
            n(
                "rust:file:a.rs",
                NodeKind::File,
                "a.rs",
                "rust:file:a.rs",
                "a.rs",
                0,
            ),
            n(
                "rust:function:a.rs#foo",
                NodeKind::Function,
                "foo",
                "rust:a.rs#foo",
                "a.rs",
                10,
            ),
            n(
                "rust:function:a.rs#bar",
                NodeKind::Function,
                "bar",
                "rust:a.rs#bar",
                "a.rs",
                50,
            ),
            n(
                "rust:function:b.rs#baz",
                NodeKind::Function,
                "baz",
                "rust:b.rs#baz",
                "b.rs",
                10,
            ),
        ];
        let edges = vec![
            e(
                "c1",
                EdgeKind::Contains,
                "rust:file:a.rs",
                "rust:function:a.rs#foo",
                9,
                "a.rs",
            ),
            e(
                "c2",
                EdgeKind::Contains,
                "rust:file:a.rs",
                "rust:function:a.rs#bar",
                49,
                "a.rs",
            ),
            e(
                "call1",
                EdgeKind::Calls,
                "rust:function:a.rs#foo",
                "rust:function:a.rs#bar",
                30,
                "a.rs",
            ),
            e(
                "call2",
                EdgeKind::Calls,
                "rust:function:a.rs#bar",
                "rust:function:b.rs#baz",
                60,
                "a.rs",
            ),
        ];
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

    fn locate_snapshot() -> Snapshot {
        let mut snapshot = snap();
        let template = snapshot.nodes[1].clone();
        snapshot.nodes = [
            ("file", NodeKind::File, 1, 20, 0, 0, 200),
            ("outer", NodeKind::Module, 2, 15, 2, 10, 150),
            ("first", NodeKind::Function, 3, 7, 0, 20, 60),
            ("second", NodeKind::Function, 8, 11, 2, 70, 110),
            ("placeholder", NodeKind::ImportRef, 4, 5, 0, 30, 40),
        ]
        .into_iter()
        .map(
            |(id, kind, start_line, end_line, end_col, start_byte, end_byte)| {
                let mut node = template.clone();
                node.id = id.into();
                node.kind = kind;
                node.provenance.span = Span {
                    start_byte,
                    end_byte,
                    start_line,
                    start_col: 0,
                    end_line,
                    end_col,
                };
                node
            },
        )
        .collect();
        snapshot
    }

    #[test]
    fn locate_returns_innermost_definition_and_explicit_limits() {
        let snapshot = locate_snapshot();
        let graph = Graph::from_snapshot(&snapshot);
        let found = graph.locate("a.rs:004-006").unwrap();
        assert_eq!(found.snapshot_id, snapshot.identity.snapshot_id);
        assert_eq!(found.query, "a.rs:4-6");
        assert_eq!(found.location.rel_path, "a.rs");
        assert_eq!((found.location.start_line, found.location.end_line), (4, 6));
        assert_eq!(
            found
                .definitions
                .iter()
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>(),
            ["first"]
        );
        assert_eq!(
            serde_json::to_value(&found).unwrap()["executable_classification"],
            "unsupported"
        );
    }

    #[test]
    fn locate_range_requires_whole_range_containment() {
        let snapshot = locate_snapshot();
        let graph = Graph::from_snapshot(&snapshot);
        assert_eq!(graph.locate("a.rs:6-8").unwrap().definitions[0].id, "outer");
        assert_eq!(
            graph.locate("a.rs:7").unwrap().definitions[0].id,
            "outer",
            "end column 0 excludes the next line"
        );
        assert_eq!(graph.locate("a.rs:11").unwrap().definitions[0].id, "second");
        for query in ["a.rs:1", "a.rs:16", "a.rs:999", "absent.rs:4", "a.rs:4-16"] {
            assert!(
                graph.locate(query).unwrap().definitions.is_empty(),
                "{query}"
            );
        }
        let mut no_module = snapshot.clone();
        no_module.nodes.retain(|node| node.kind != NodeKind::Module);
        assert_eq!(
            Graph::from_snapshot(&no_module)
                .locate("a.rs:6-8")
                .unwrap()
                .definitions
                .len(),
            0
        );
    }

    #[test]
    fn locate_keeps_same_line_candidates_and_stable_order() {
        let mut snapshot = locate_snapshot();
        snapshot.nodes[2].provenance.span.end_line = 6;
        snapshot.nodes[2].provenance.span.end_col = 5;
        let mut other = snapshot.nodes[2].clone();
        other.id = "another".into();
        other.provenance.span = Span {
            start_byte: 61,
            end_byte: 69,
            start_line: 6,
            start_col: 10,
            end_line: 6,
            end_col: 18,
        };
        snapshot.nodes.push(other);
        let first = serde_json::to_value(Graph::from_snapshot(&snapshot).locate("a.rs:6").unwrap())
            .unwrap();
        snapshot.nodes.reverse();
        let second =
            serde_json::to_value(Graph::from_snapshot(&snapshot).locate("a.rs:6-6").unwrap())
                .unwrap();
        assert_eq!(first, second);
        let ids: Vec<_> = first["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["another", "first"]);
    }

    #[test]
    fn locate_rejects_noncanonical_paths_and_invalid_ranges() {
        let snapshot = locate_snapshot();
        let graph = Graph::from_snapshot(&snapshot);
        for query in [
            "",
            "a.rs",
            ":1",
            "a.rs:0",
            "a.rs:0-1",
            "a.rs:1-0",
            "a.rs:2-1",
            "a.rs:1-",
            "a.rs:-1",
            "a.rs:1-2-3",
            "a.rs:+1",
            "a.rs: 1",
            "a.rs:1:2",
            "a.rs:4294967296",
            "a.rs:1-4294967296",
            "/a.rs:1",
            "../a.rs:1",
            "a/../a.rs:1",
            "./a.rs:1",
            "a//b.rs:1",
            "a/./b.rs:1",
            "a.rs/:1",
            "C:/a.rs:1",
            "C:a.rs:1",
            "a\\b.rs:1",
            "a\0.rs:1",
        ] {
            assert!(graph.locate(query).is_err(), "accepted {query:?}");
        }
    }

    #[test]
    fn resolve_by_id_name_and_symbol_key() {
        let s = snap();
        let g = Graph::from_snapshot(&s);
        assert!(matches!(
            g.resolve("rust:function:a.rs#foo"),
            ResolveResult::Unique(_)
        ));
        assert!(matches!(g.resolve("foo"), ResolveResult::Unique(_)));
        assert!(matches!(
            g.resolve("rust:a.rs#foo"),
            ResolveResult::Unique(_)
        ));
        assert!(matches!(g.resolve("nope"), ResolveResult::Unknown));
    }

    #[test]
    fn resolve_by_qualified_suffix_with_hash_or_colons() {
        let mut s = snap();
        let mut nested = s.nodes[1].clone();
        nested.id = "rust:function:a.rs#Api::Agents::Census::summary".into();
        nested.name = "summary".into();
        nested.symbol_key = "rust:a.rs#Api::Agents::Census::summary".into();
        let mut other = nested.clone();
        other.id = "rust:function:b.rs#Api::Managers::Census::summary".into();
        other.symbol_key = "rust:b.rs#Api::Managers::Census::summary".into();
        other.provenance.rel_path = "b.rs".into();
        s.nodes.push(nested);
        s.nodes.push(other);
        let g = Graph::from_snapshot(&s);

        let ResolveResult::Unique(hit) = g.resolve("Agents::Census#summary") else {
            panic!("one node ends in Agents::Census::summary");
        };
        assert_eq!(hit.provenance.rel_path, "a.rs");
        assert!(matches!(
            g.resolve("Managers::Census::summary"),
            ResolveResult::Unique(_)
        ));
        let ResolveResult::Ambiguous(hits) = g.resolve("Census#summary") else {
            panic!("two nodes end in Census::summary");
        };
        let paths: Vec<&str> = hits
            .iter()
            .map(|n| n.provenance.rel_path.as_str())
            .collect();
        assert_eq!(paths, ["a.rs", "b.rs"], "in id order");
        assert!(
            matches!(g.resolve("ensus#summary"), ResolveResult::Unknown),
            "a suffix matches whole segments only"
        );
        assert!(matches!(g.resolve("Nope#summary"), ResolveResult::Unknown));
    }

    #[test]
    fn duplicate_symbol_keys_are_ambiguous() {
        let mut s = snap();
        let mut duplicate = s.nodes[1].clone();
        duplicate.id = "rust:function:b.rs#foo".into();
        duplicate.name = "other_name".into();
        duplicate.provenance.rel_path = "b.rs".into();
        s.nodes.push(duplicate);

        let g = Graph::from_snapshot(&s);
        let ResolveResult::Ambiguous(matches) = g.resolve("rust:a.rs#foo") else {
            panic!("duplicate exact symbol keys must be ambiguous");
        };
        let ids: Vec<&str> = matches.iter().map(|node| node.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["rust:function:a.rs#foo", "rust:function:b.rs#foo"]
        );
    }

    #[test]
    fn context_groups_in_and_out_edges() {
        let s = snap();
        let g = Graph::from_snapshot(&s);
        let ctx = g.context("rust:function:a.rs#foo").expect("foo context");
        assert_eq!(ctx.fan_in, 1); // contained by file
        assert_eq!(ctx.fan_out, 1); // calls bar
        assert_eq!(ctx.container.as_deref(), Some("rust:file:a.rs"));
        assert!(
            ctx.outgoing
                .iter()
                .any(|(k, to, _)| k == "calls" && to == "rust:function:a.rs#bar")
        );
    }

    #[test]
    fn impact_downstream_traverses_calls() {
        let s = snap();
        let g = Graph::from_snapshot(&s);
        let imp = g.impact(
            "rust:function:a.rs#foo",
            Direction::Downstream,
            &[EdgeKind::Calls],
            10,
        );
        // foo -> bar (depth 1), bar -> baz (depth 2)
        assert_eq!(imp.len(), 2);
        assert_eq!(imp[0].node, "rust:function:a.rs#bar");
        assert_eq!(imp[0].depth, 1);
        assert_eq!(imp[1].node, "rust:function:b.rs#baz");
        assert_eq!(imp[1].depth, 2);
    }

    #[test]
    fn impact_upstream_finds_callers() {
        let s = snap();
        let g = Graph::from_snapshot(&s);
        let imp = g.impact(
            "rust:function:b.rs#baz",
            Direction::Upstream,
            &[EdgeKind::Calls],
            10,
        );
        assert_eq!(imp.len(), 2);
        assert_eq!(imp[0].node, "rust:function:a.rs#bar"); // depth 1
        assert_eq!(imp[1].node, "rust:function:a.rs#foo"); // depth 2
    }

    #[test]
    fn trace_finds_shortest_call_path() {
        let s = snap();
        let g = Graph::from_snapshot(&s);
        let path = g
            .trace(
                "rust:function:a.rs#foo",
                "rust:function:b.rs#baz",
                &[EdgeKind::Calls],
            )
            .expect("path exists");
        assert_eq!(
            path,
            vec![
                "rust:function:a.rs#foo".to_string(),
                "rust:function:a.rs#bar".to_string(),
                "rust:function:b.rs#baz".to_string(),
            ]
        );
    }

    #[test]
    fn search_by_substring() {
        let s = snap();
        let g = Graph::from_snapshot(&s);
        let hits = g.search("ba");
        let names: Vec<_> = hits.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"bar"));
        assert!(names.contains(&"baz"));
        assert!(!names.contains(&"foo"));
    }

    #[test]
    fn children_of_file() {
        let s = snap();
        let g = Graph::from_snapshot(&s);
        let kids = g.children("rust:file:a.rs");
        let names: Vec<_> = kids.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["bar", "foo"]); // sorted by id
    }

    #[test]
    fn cycles_detects_mutual_dependency() {
        // Build a 2-cycle: foo -> bar -> foo.
        let mut s = snap();
        s.edges.push(Edge {
            id: "call3".into(),
            kind: EdgeKind::Calls,
            from: "rust:function:b.rs#baz".into(),
            to: "rust:function:a.rs#foo".into(),
            provenance: Provenance {
                repository: "r".into(),
                commit_sha: "c".into(),
                rel_path: "b.rs".into(),
                span: Span::from_ts(20, 25, (0, 20), (0, 25)),
                file_hash: "h".into(),
                extractor_version: s.identity.extractor_version.clone(),
                fact_source: FactSource::TreeSitter,
                confidence: 255,
            },
        });
        let g = Graph::from_snapshot(&s);
        let cyc = g.cycles(&[EdgeKind::Calls]);
        assert_eq!(cyc.len(), 1, "one SCC cycle: {cyc:?}");
        assert_eq!(cyc[0].len(), 3, "foo, bar, baz all in the cycle");
    }
}
