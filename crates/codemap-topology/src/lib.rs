//! Presentation-only topology over validated canonical facts.

pub mod cli;
mod liveness;
pub use liveness::{CheckReport, Selection};

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use codemap_graph::{
    CallSite, CallSiteCategory, EdgeKind, Node, NodeKind, Provenance, Snapshot, SnapshotIdentity,
};
use serde::Serialize;

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
    pub unowned_call_sites: Vec<CallSite>,
    pub extraction_report: codemap_graph::Report,
    pub unsupported_construct_census: &'static str,
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

/// Accept a report only with an independently supplied expected selection.
/// No report means no absence claims, including for nodes with zero inbound calls.
pub fn project(
    snapshot: &Snapshot,
    liveness: Option<(&CheckReport, &Selection)>,
) -> Result<Topology> {
    codemap_graph::validate(snapshot)?;
    let (selection, absence) = if let Some((report, selection)) = liveness {
        (
            Some(selection.clone()),
            liveness::validate(snapshot, report, selection)?,
        )
    } else {
        (None, BTreeMap::new())
    };
    let (annotations, unowned) = annotations(snapshot);
    let root = tree(snapshot, annotations)?;
    let callables: BTreeSet<_> = snapshot
        .nodes
        .iter()
        .filter(|node| callable(node))
        .map(|node| &node.id)
        .collect();
    let mut edges = Vec::new();
    for edge in snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
    {
        ensure!(
            callables.contains(&edge.from) && callables.contains(&edge.to),
            "call endpoints must be callable leaves"
        );
        edges.push(Connection {
            source: edge.from.clone(),
            target: edge.to.clone(),
            kind: "call",
            codemap: EdgeEvidence {
                edge_id: edge.id.clone(),
                provenance: edge.provenance.clone(),
                source_link: source_link(&edge.provenance),
            },
        });
    }
    edges.sort_by(|a, b| a.codemap.edge_id.cmp(&b.codemap.edge_id));
    Ok(Topology {
        system: snapshot.identity.repository.id.clone(),
        root,
        edges,
        entry_points: selection
            .as_ref()
            .map_or_else(Vec::new, |s| s.entry_points.clone()),
        dead_ends: absence.keys().cloned().collect(),
        codemap: Extension {
            version: "codemap.topology.v1",
            identity: snapshot.identity.clone(),
            selection,
            absence,
            unowned_call_sites: unowned,
            extraction_report: snapshot.report.clone(),
            unsupported_construct_census: "unknown: no denominator for unrecognized constructs",
        },
    })
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
    let containers: BTreeMap<_, _> = snapshot
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Module | NodeKind::Type(_) | NodeKind::Trait
            )
        })
        .map(|n| (&n.id, n))
        .collect();
    let mut containment: BTreeMap<&String, BTreeSet<&String>> = BTreeMap::new();
    for edge in snapshot
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Contains && containers.contains_key(&e.from))
    {
        containment.entry(&edge.to).or_default().insert(&edge.from);
    }
    for node in snapshot
        .nodes
        .iter()
        .filter(|n| callable(n) || containers.contains_key(&n.id))
    {
        let id = if callable(node) {
            node.id.clone()
        } else {
            format!("@container:{}", node.id)
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
    if !safe_link(&provenance.rel_path) {
        return None;
    }
    let path = encode_path(&provenance.rel_path);
    Some(format!(
        "{path}#L{}-L{}",
        provenance.span.start_line, provenance.span.end_line
    ))
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
    let data = serde_json::to_string(topology)?
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
