//! Presentation-only topology over validated canonical facts.

pub mod cli;
mod liveness;
pub use liveness::{CheckReport, Selection};

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use codemap_graph::{
    CallSite, CallSiteCategory, EdgeKind, Node, NodeKind, Provenance, Snapshot, SnapshotIdentity,
};
use serde::Serialize;

/// What one projection covers besides the snapshot itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct Scope<'a> {
    /// Marks the selected entry points. Alone it supports no absence claim.
    pub selection: Option<&'a Selection>,
    /// Admits absence markers, checked against `selection`.
    pub liveness: Option<&'a CheckReport>,
    /// Repository-relative path prefixes left off the map.
    pub exclude: &'a [String],
    /// Relations derived by named policies, supplied by the caller. The
    /// topology places them; it derives none.
    pub policies: &'a [Policy],
}

/// What one named policy derived from the snapshot being drawn.
#[derive(Debug, Clone)]
pub struct Policy {
    /// The policy's provenance name, `policy:<name>/<language>`.
    pub name: String,
    /// The relation kind the viewer groups and toggles these under.
    pub kind: String,
    pub relations: Vec<Relation>,
    /// What the policy left unresolved, counted by reason.
    pub unresolved: BTreeMap<String, usize>,
}

/// One derived relation between two snapshot nodes.
#[derive(Debug, Clone)]
pub struct Relation {
    pub from: String,
    pub to: String,
    /// Which of the policy's rules found the target.
    pub rule: String,
    /// The source declaration the relation rests on, as a reader would
    /// write it.
    pub declaration: String,
    pub through: Option<String>,
    /// The node that declares it on behalf of `from`, when one does.
    pub via: Option<String>,
    pub rel_path: String,
    pub lines: (u32, u32),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Topology {
    pub system: String,
    pub root: TreeNode,
    pub edges: Vec<Connection>,
    pub entry_points: Vec<String>,
    pub dead_ends: Vec<String>,
    pub codemap: Extension,
}

#[derive(Debug, Serialize)]
pub struct Extension {
    pub version: &'static str,
    pub identity: SnapshotIdentity,
    pub selection: Option<Selection>,
    pub absence: BTreeMap<String, Absence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excluded: Option<Excluded>,
    /// What each named policy derived from the snapshot and could not.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<PolicySummary>,
    /// Relations a policy derived. They are not snapshot edges.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub policy_relations: Vec<PolicyRelation>,
    pub unowned_call_sites: Vec<CallSite>,
    pub unowned_call_site_summary: UnownedSummary,
    pub extraction_report: codemap_graph::Report,
    pub unsupported_construct_census: &'static str,
}

/// What `Scope::exclude` left off the map; the snapshot itself is unchanged.
#[derive(Debug, Serialize)]
pub struct Excluded {
    pub prefixes: Vec<String>,
    pub files: usize,
    pub nodes: usize,
    pub edges: usize,
    pub call_sites: usize,
}

#[derive(Debug, Serialize)]
pub struct PolicySummary {
    pub policy: String,
    /// Relations derived over the whole snapshot, drawn or not.
    pub relations: usize,
    pub unresolved: BTreeMap<String, usize>,
}

/// One relation between two drawn nodes, with the declaration it rests on.
#[derive(Debug, Serialize)]
pub struct PolicyRelation {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub policy: String,
    pub rule: String,
    pub declaration: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub through: Option<String>,
    /// The module that declares it for the classes including it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    pub rel_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub source_link: Option<String>,
}

/// Counts of the unresolved call sites that no unique callable owns.
#[derive(Debug, Serialize)]
pub struct UnownedSummary {
    pub total: usize,
    pub by_category: BTreeMap<String, usize>,
    pub by_file: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
pub struct Absence {
    pub qualification: String,
    pub witness: String,
    pub admission_witness: String,
}

#[derive(Debug, Serialize)]
pub struct Connection {
    pub source: String,
    pub target: String,
    pub kind: &'static str,
    pub codemap: EdgeEvidence,
}

#[derive(Debug, Serialize)]
pub struct EdgeEvidence {
    pub edge_id: String,
    pub provenance: Provenance,
    pub source_link: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TreeNode {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Self>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loc: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codemap: Option<NodeEvidence>,
}

#[derive(Debug, Serialize)]
pub struct NodeEvidence {
    pub node_id: String,
    pub symbol_key: String,
    pub kind: NodeKind,
    pub provenance: Provenance,
    pub source_link: Option<String>,
    pub unresolved_call_sites: Vec<CallSite>,
}

pub(crate) const fn callable(node: &Node) -> bool {
    matches!(
        node.kind,
        NodeKind::Function | NodeKind::Method | NodeKind::Macro
    )
}

const fn container(node: &Node) -> bool {
    matches!(
        node.kind,
        NodeKind::Module | NodeKind::Type(_) | NodeKind::Trait
    )
}

/// The node's id in the presentation tree, when the tree draws it.
fn tree_id(node: &Node) -> Option<String> {
    if callable(node) {
        Some(node.id.clone())
    } else if container(node) {
        Some(format!("@container:{}", node.id))
    } else {
        None
    }
}

/// The viewer's name for a canonical edge kind, or `None` when the map
/// shows the relation as containment or not at all.
const fn relation(kind: EdgeKind) -> Option<&'static str> {
    match kind {
        EdgeKind::Calls => Some("call"),
        EdgeKind::Inherits => Some("inherits"),
        EdgeKind::Implements => Some("implements"),
        EdgeKind::Extends => Some("extends"),
        EdgeKind::Contains | EdgeKind::Imports => None,
    }
}

/// Where each snapshot node sits on the map. A file is drawn as its group,
/// so a relation may end on one.
fn drawn(snapshot: &Snapshot) -> BTreeMap<&String, String> {
    snapshot
        .nodes
        .iter()
        .filter_map(|node| {
            let id = if node.kind == NodeKind::File {
                format!("@file:{}", node.provenance.rel_path)
            } else {
                tree_id(node)?
            };
            Some((&node.id, id))
        })
        .collect()
}

fn under(rel_path: &str, prefix: &str) -> bool {
    rel_path
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Drop everything under the excluded prefixes. An edge goes when either
/// endpoint does. A prefix naming no file is refused rather than ignored.
fn exclude(snapshot: &Snapshot, prefixes: &[String]) -> Result<(Snapshot, Excluded)> {
    let prefixes: BTreeSet<&str> = prefixes
        .iter()
        .map(|prefix| prefix.strip_suffix('/').unwrap_or(prefix))
        .collect();
    for prefix in &prefixes {
        ensure!(
            snapshot.files.iter().any(|f| under(&f.rel_path, prefix)),
            "exclusion matches no file: {prefix}"
        );
    }
    let hidden = |rel_path: &str| prefixes.iter().any(|prefix| under(rel_path, prefix));
    let mut shown = snapshot.clone();
    shown.files.retain(|f| !hidden(&f.rel_path));
    shown.nodes.retain(|n| !hidden(&n.provenance.rel_path));
    shown.call_sites.retain(|site| !hidden(&site.rel_path));
    let kept: BTreeSet<&String> = shown.nodes.iter().map(|n| &n.id).collect();
    let mut edges = std::mem::take(&mut shown.edges);
    edges.retain(|e| kept.contains(&e.from) && kept.contains(&e.to));
    drop(kept);
    shown.edges = edges;
    let excluded = Excluded {
        prefixes: prefixes.into_iter().map(str::to_string).collect(),
        files: snapshot.files.len() - shown.files.len(),
        nodes: snapshot.nodes.len() - shown.nodes.len(),
        edges: snapshot.edges.len() - shown.edges.len(),
        call_sites: snapshot.call_sites.len() - shown.call_sites.len(),
    };
    Ok((shown, excluded))
}

/// A selection alone marks entry points. Absence markers also need the
/// liveness report that selection is checked against; without one the map
/// makes no absence claim, including for nodes with zero inbound calls.
pub fn project(snapshot: &Snapshot, scope: Scope<'_>) -> Result<Topology> {
    codemap_graph::validate(snapshot)?;
    let absence = match (scope.liveness, scope.selection) {
        (Some(report), Some(selection)) => liveness::validate(snapshot, report, selection)?,
        (None, Some(selection)) => {
            liveness::validate_selection(snapshot, selection)?;
            BTreeMap::new()
        }
        (Some(_), None) => bail!("a liveness report needs its expected selection"),
        (None, None) => BTreeMap::new(),
    };
    let (shown, excluded) = if scope.exclude.is_empty() {
        (None, None)
    } else {
        let (shown, excluded) = exclude(snapshot, scope.exclude)?;
        (Some(shown), Some(excluded))
    };
    let shown = shown.as_ref().unwrap_or(snapshot);
    let (annotations, unowned) = annotations(shown);
    let root = tree(shown, annotations)?;
    let drawn = drawn(shown);
    let callables: BTreeSet<_> = shown
        .nodes
        .iter()
        .filter(|node| callable(node))
        .map(|node| &node.id)
        .collect();
    let mut edges = Vec::new();
    for edge in &shown.edges {
        let Some(kind) = relation(edge.kind) else {
            continue;
        };
        if edge.kind == EdgeKind::Calls {
            ensure!(
                callables.contains(&edge.from) && callables.contains(&edge.to),
                "call endpoints must be callable leaves"
            );
        }
        let (Some(source), Some(target)) = (drawn.get(&edge.from), drawn.get(&edge.to)) else {
            bail!("{kind} endpoints must be drawn nodes: {}", edge.id);
        };
        edges.push(Connection {
            source: source.clone(),
            target: target.clone(),
            kind,
            codemap: EdgeEvidence {
                edge_id: edge.id.clone(),
                provenance: edge.provenance.clone(),
                source_link: source_link(&edge.provenance),
            },
        });
    }
    edges.sort_by(|a, b| a.codemap.edge_id.cmp(&b.codemap.edge_id));
    let mut summary = UnownedSummary {
        total: unowned.len(),
        by_category: BTreeMap::new(),
        by_file: BTreeMap::new(),
    };
    for site in &unowned {
        *summary
            .by_category
            .entry(site.category.report_key().to_string())
            .or_default() += 1;
        *summary.by_file.entry(site.rel_path.clone()).or_default() += 1;
    }
    let (policies, policy_relations) = policy_relations(snapshot, scope.policies, &drawn)?;
    let on_map = |id: &String| drawn.contains_key(id);
    Ok(Topology {
        system: snapshot.identity.repository.id.clone(),
        root,
        edges,
        entry_points: scope.selection.map_or_else(Vec::new, |s| {
            s.entry_points
                .iter()
                .filter(|id| on_map(id))
                .cloned()
                .collect()
        }),
        dead_ends: absence.keys().filter(|id| on_map(id)).cloned().collect(),
        codemap: Extension {
            version: "codemap.topology.v2",
            identity: snapshot.identity.clone(),
            selection: scope.selection.cloned(),
            absence,
            excluded,
            policies,
            policy_relations,
            unowned_call_sites: unowned,
            unowned_call_site_summary: summary,
            extraction_report: snapshot.report.clone(),
            unsupported_construct_census: "unknown: no denominator for unrecognized constructs",
        },
    })
}

/// Place each supplied policy's relations on the map. A relation must name
/// nodes and a file of this snapshot; one whose ends are not both drawn is
/// left off, while the summary counts everything the policy reported.
fn policy_relations(
    snapshot: &Snapshot,
    policies: &[Policy],
    drawn: &BTreeMap<&String, String>,
) -> Result<(Vec<PolicySummary>, Vec<PolicyRelation>)> {
    let nodes: BTreeSet<&str> = snapshot.nodes.iter().map(|n| n.id.as_str()).collect();
    let files: BTreeSet<&str> = snapshot.files.iter().map(|f| f.rel_path.as_str()).collect();
    let mut summaries = Vec::new();
    let mut relations = Vec::new();
    for policy in policies {
        ensure!(
            policy.name.starts_with("policy:") && !policy.kind.is_empty(),
            "a policy needs a policy: name and a relation kind"
        );
        summaries.push(PolicySummary {
            policy: policy.name.clone(),
            relations: policy.relations.len(),
            unresolved: policy.unresolved.clone(),
        });
        for relation in &policy.relations {
            ensure!(
                nodes.contains(relation.from.as_str())
                    && nodes.contains(relation.to.as_str())
                    && relation
                        .via
                        .as_deref()
                        .is_none_or(|via| nodes.contains(via))
                    && files.contains(relation.rel_path.as_str()),
                "{} relation names a node or file outside the snapshot: {}",
                policy.name,
                relation.declaration
            );
            let (Some(source), Some(target)) = (drawn.get(&relation.from), drawn.get(&relation.to))
            else {
                continue;
            };
            relations.push(PolicyRelation {
                source: source.clone(),
                target: target.clone(),
                kind: policy.kind.clone(),
                policy: policy.name.clone(),
                rule: relation.rule.clone(),
                declaration: relation.declaration.clone(),
                through: relation.through.clone(),
                via: relation
                    .via
                    .as_ref()
                    .and_then(|module| drawn.get(module).cloned()),
                source_link: line_link(&relation.rel_path, relation.lines),
                rel_path: relation.rel_path.clone(),
                start_line: relation.lines.0,
                end_line: relation.lines.1,
            });
        }
    }
    Ok((summaries, relations))
}

fn annotations(snapshot: &Snapshot) -> (BTreeMap<String, Vec<CallSite>>, Vec<CallSite>) {
    let mut callables_by_file: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
    for node in snapshot.nodes.iter().filter(|node| callable(node)) {
        callables_by_file
            .entry(&node.provenance.rel_path)
            .or_default()
            .push(node);
    }
    let mut annotations: BTreeMap<String, Vec<CallSite>> = BTreeMap::new();
    let mut unowned = Vec::new();
    for site in &snapshot.call_sites {
        if site.category == CallSiteCategory::Resolved {
            continue;
        }
        let mut owners: Vec<_> = callables_by_file
            .get(site.rel_path.as_str())
            .into_iter()
            .flatten()
            .filter(|node| {
                node.provenance.span.start_byte <= site.span.start_byte
                    && node.provenance.span.end_byte >= site.span.end_byte
            })
            .collect();
        owners.sort_by_key(|node| node.provenance.span.end_byte - node.provenance.span.start_byte);
        let owner = owners.first().filter(|first| {
            owners.get(1).is_none_or(|second| {
                first.provenance.span.end_byte - first.provenance.span.start_byte
                    < second.provenance.span.end_byte - second.provenance.span.start_byte
            })
        });
        if let Some(owner) = owner {
            annotations
                .entry(owner.id.clone())
                .or_default()
                .push(site.clone());
        } else {
            unowned.push(site.clone());
        }
    }
    for sites in annotations
        .values_mut()
        .chain(std::iter::once(&mut unowned))
    {
        sites.sort_by(|a, b| {
            (
                &a.rel_path,
                a.span.start_byte,
                a.span.end_byte,
                a.category.report_key(),
                &a.raw_name,
            )
                .cmp(&(
                    &b.rel_path,
                    b.span.start_byte,
                    b.span.end_byte,
                    b.category.report_key(),
                    &b.raw_name,
                ))
        });
    }
    (annotations, unowned)
}

const fn group(id: String, name: String) -> TreeNode {
    TreeNode {
        id,
        name,
        kind: "domain",
        children: vec![],
        language: None,
        file: None,
        loc: None,
        codemap: None,
    }
}

fn tree(snapshot: &Snapshot, mut annotations: BTreeMap<String, Vec<CallSite>>) -> Result<TreeNode> {
    let mut nodes = BTreeMap::new();
    let mut parents = BTreeMap::new();
    let root_id = "@system".to_string();
    let mut root = group(root_id.clone(), snapshot.identity.repository.id.clone());
    root.kind = "system";
    nodes.insert(root_id.clone(), root);
    for file in &snapshot.files {
        let mut parent = root_id.clone();
        let parts: Vec<_> = file.rel_path.split('/').collect();
        for end in 1..parts.len() {
            let path = parts[..end].join("/");
            let id = format!("@directory:{path}");
            nodes
                .entry(id.clone())
                .or_insert_with(|| group(id.clone(), parts[end - 1].into()));
            parents.insert(id.clone(), parent);
            parent = id;
        }
        let id = format!("@file:{}", file.rel_path);
        nodes.insert(
            id.clone(),
            group(id.clone(), parts.last().unwrap_or(&"").to_string()),
        );
        parents.insert(id, parent);
    }
    let containers: BTreeSet<_> = snapshot
        .nodes
        .iter()
        .filter(|n| container(n))
        .map(|n| &n.id)
        .collect();
    let mut containment: BTreeMap<&String, BTreeSet<&String>> = BTreeMap::new();
    for edge in snapshot
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Contains && containers.contains(&e.from))
    {
        containment.entry(&edge.to).or_default().insert(&edge.from);
    }
    for node in &snapshot.nodes {
        let Some(id) = tree_id(node) else {
            continue;
        };
        let mut projected = group(id.clone(), node.name.clone());
        if callable(node) {
            projected.kind = "module";
        }
        projected.language = Some(snapshot.identity.language.tag().into());
        projected.file = Some(node.provenance.rel_path.clone());
        projected.loc = Some(node.provenance.span.end_line - node.provenance.span.start_line + 1);
        projected.codemap = Some(NodeEvidence {
            node_id: node.id.clone(),
            symbol_key: node.symbol_key.clone(),
            kind: node.kind,
            provenance: node.provenance.clone(),
            source_link: source_link(&node.provenance),
            unresolved_call_sites: annotations.remove(&node.id).unwrap_or_default(),
        });
        let parent = if let Some(containers) = containment.get(&node.id) {
            ensure!(
                containers.len() == 1,
                "multiple structural parents for {}",
                node.id
            );
            format!("@container:{}", containers.first().expect("nonempty"))
        } else {
            format!("@file:{}", node.provenance.rel_path)
        };
        parents.insert(id.clone(), parent);
        nodes.insert(id, projected);
    }
    // Validate every ancestor chain before recursive assembly: canonical validation
    // permits arbitrary contains edges, whereas a presentation tree cannot cycle.
    for id in parents.keys() {
        let mut seen = BTreeSet::new();
        let mut current = id;
        while let Some(parent) = parents.get(current) {
            ensure!(seen.insert(current), "cyclic structural containment");
            ensure!(nodes.contains_key(parent), "missing structural parent");
            current = parent;
        }
    }
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, parent) in parents {
        children.entry(parent).or_default().push(id);
    }
    Ok(assemble(&root_id, &mut nodes, &children))
}

fn assemble(
    id: &str,
    nodes: &mut BTreeMap<String, TreeNode>,
    children: &BTreeMap<String, Vec<String>>,
) -> TreeNode {
    let mut node = nodes.remove(id).expect("validated tree member");
    if let Some(ids) = children.get(id) {
        node.children = ids.iter().map(|id| assemble(id, nodes, children)).collect();
    }
    node
}

/// Repository-relative paths, fragments and HTTPS only; no browser-normalized
/// backslashes, control characters, protocol-relative URLs or other schemes.
pub fn safe_link(link: &str) -> bool {
    if link.is_empty()
        || link.trim() != link
        || link.chars().any(|c| c.is_control() || c == '\\')
        || link.starts_with('/')
    {
        return false;
    }
    if let Some(rest) = link.strip_prefix("https://") {
        return !rest.is_empty() && !rest.starts_with('/');
    }
    !link
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .contains(':')
}

fn source_link(provenance: &Provenance) -> Option<String> {
    line_link(
        &provenance.rel_path,
        (provenance.span.start_line, provenance.span.end_line),
    )
}

fn line_link(rel_path: &str, (start, end): (u32, u32)) -> Option<String> {
    if !safe_link(rel_path) {
        return None;
    }
    Some(format!("{}#L{start}-L{end}", encode_path(rel_path)))
}

fn encode_path(path: &str) -> String {
    path.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect::<String>()
}

pub fn render_html(topology: &Topology) -> Result<String> {
    render_with_prefix(topology, "")
}

/// Resolve source links against an explicit source directory from the HTML destination.
/// Canonical topology data and source citations remain unchanged.
pub fn render_html_with_source_root(
    topology: &Topology,
    source_root: &Path,
    output: &Path,
) -> Result<String> {
    ensure!(source_root.is_dir(), "source root must be a directory");
    let source = absolute_lexical(source_root)?;
    let output = absolute_lexical(output)?;
    let directory = output
        .parent()
        .context("HTML output requires a parent directory")?;
    ensure!(directory.is_dir(), "HTML parent must be a directory");
    let source_parts: Vec<_> = source.components().collect();
    let output_parts: Vec<_> = directory.components().collect();
    let shared = source_parts
        .iter()
        .zip(&output_parts)
        .take_while(|(a, b)| a == b)
        .count();
    ensure!(
        shared > 0,
        "source root and HTML must share a filesystem root"
    );
    let mut prefix = "../".repeat(output_parts.len() - shared);
    for component in &source_parts[shared..] {
        let part = component
            .as_os_str()
            .to_str()
            .context("source path must be UTF-8")?;
        prefix.push_str(&encode_path(part));
        prefix.push('/');
    }
    render_with_prefix(topology, &prefix)
}

fn absolute_lexical(path: &Path) -> Result<PathBuf> {
    let mut result = PathBuf::new();
    // URL resolution normalizes the opened path before following filesystem symlinks.
    for component in std::path::absolute(path)?.components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

fn render_with_prefix(topology: &Topology, prefix: &str) -> Result<String> {
    let mut data = serde_json::to_value(topology)?;
    data["codemap"]
        .as_object_mut()
        .context("topology extension is an object")?
        .remove("unowned_call_sites");
    let data = serde_json::to_string(&data)?
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    Ok(include_str!("../assets/topology-viewer.html")
        .replace(
            "/*__SOURCE_ROOT__*/ \"\"",
            &format!("/*__SOURCE_ROOT__*/ {}", serde_json::to_string(prefix)?),
        )
        .replace(
            "/*__TOPOLOGY_DATA__*/ null",
            &format!("/*__TOPOLOGY_DATA__*/ {data}"),
        ))
}
