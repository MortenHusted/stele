//! Cross-commit history walk: the structural change stream.
//!
//! A snapshot answers "what does this commit look like". Walking commits and
//! diffing adjacent pairs answers the other question — "what changed, and
//! when" — without storing a snapshot per commit. A snapshot of a mid-sized
//! repository holds tens of thousands of facts; the change between two
//! adjacent commits is a handful. So this emits
//! **transitions**, one line per fact that appeared or disappeared, plus the
//! per-node metric moves those transitions caused.
//!
//! Everything here is derived from `Layer 0` facts by graph math. It creates no
//! edge, upgrades no confidence, and writes nothing back into a snapshot.
//! Metric lines are `Layer 1.5` heuristics and explicitly non-authoritative.
//!
//! With `measures` on, the walk additionally keeps one measured snapshot
//! per commit under `measures/` and lists them in `measured.ndjson` with
//! the commit's author label. That is data for a consumer
//! (`stele slop series`); the walk computes no metric over it, and the
//! three streams above are byte-identical with the switch on or off.
//!
//! Facts are keyed for comparison across commits, not within one:
//! [`codemap_graph::stable_node_keys`] and [`codemap_graph::stable_edge_keys`]
//! drop the byte offsets that snapshot ids carry. Keying on the raw ids instead
//! would report every fact below an inserted line as deleted-and-recreated —
//! and since every `ImportRef` id embeds its offset, a line added at the top of
//! a file would resurrect all of that file's imports on every commit.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use codemap_graph::analysis::{self, NodeMetrics};
use codemap_graph::schema::{Language, Snapshot};
use codemap_graph::{Graph, SnapshotDiff, infer_moves, stable_edge_keys, stable_node_keys};

use crate::git;

/// Which commits to walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selector {
    /// Every commit reachable from the tip.
    All,
    /// Mainline only — merge commits collapse to their first parent, so a
    /// merged branch reads as one change rather than as its internal steps.
    FirstParent,
    /// Mainline commits that touched a source file of the snapshot language.
    /// The default: commits that cannot change structure cannot produce
    /// transitions, so extracting them is pure cost.
    FirstParentSource,
}

impl Selector {
    fn rev_list_args<'a>(self, tip: &'a str, globs: &'a [&'a str]) -> Vec<&'a str> {
        match self {
            Self::All => vec!["log", "--reverse", "--format=%H", tip],
            Self::FirstParent => vec!["log", "--first-parent", "--reverse", "--format=%H", tip],
            Self::FirstParentSource => {
                let mut args = vec![
                    "log",
                    "--first-parent",
                    "--reverse",
                    "--format=%H",
                    tip,
                    "--",
                ];
                args.extend_from_slice(globs);
                args
            }
        }
    }
}

/// Inputs for a history walk.
#[derive(Debug, Clone)]
pub struct HistoryOptions {
    pub repo: PathBuf,
    pub tip: String,
    pub selector: Selector,
    pub limit: Option<usize>,
    pub out_dir: PathBuf,
    pub language: Language,
    /// Resolve semantic `calls` edges at every commit, so the stream carries
    /// call-graph history rather than structure alone. Each commit is
    /// materialized in a throwaway worktree for rust-analyzer, which costs
    /// roughly three orders of magnitude more per commit — use it on a window,
    /// not on a whole history.
    pub semantic: bool,
    /// Extract every commit with `--measures` and keep the measured snapshot
    /// (`measures/<seq>-<sha>.json`) plus a `measured.ndjson` line naming it
    /// with the commit's author label and counting policy. Fails closed for
    /// a language without a counting policy.
    pub measures: bool,
    /// Also keep twin pairs in every measured snapshot (`--twins`); needs
    /// `measures`.
    pub twins: bool,
    /// Explicit owned source-only parser profile; required for Python syntax.
    #[cfg(unix)]
    pub python_parser: Option<crate::python_parser::ParserConfig>,
}

/// What the walk produced, for the caller to report.
#[derive(Debug, Clone, Copy, Default)]
pub struct HistorySummary {
    pub commits: usize,
    pub transitions: usize,
    pub metric_moves: usize,
    pub elapsed_ms: u128,
}

/// Walk the selected commits oldest-first, extracting each and emitting the
/// change stream to `out_dir` as three NDJSON files:
///
/// - `commits.ndjson` — one line per commit walked, with totals and cost;
/// - `transitions.ndjson` — one line per fact that appeared or disappeared;
/// - `metrics.ndjson` — one line per node whose fan-in or fan-out moved.
///
/// The first commit walked has no predecessor, so every fact in it is an
/// addition. That is the genesis keyframe: replaying transitions forward from
/// it reconstructs any later commit exactly.
pub fn walk(options: &HistoryOptions, progress: &mut dyn Write) -> Result<HistorySummary> {
    validate_options(options)?;
    let started = Instant::now();
    let commits = select_commits(options)?;
    anyhow::ensure!(!commits.is_empty(), "no commits selected");

    let mut first_python_snapshot = match options.language {
        Language::Python => Some(extract_commit_timed(options, &commits[0])?),
        _ => None,
    };

    std::fs::create_dir_all(&options.out_dir)
        .with_context(|| format!("create {}", options.out_dir.display()))?;
    let mut commit_out = writer(&options.out_dir, "commits.ndjson")?;
    let mut transition_out = writer(&options.out_dir, "transitions.ndjson")?;
    let mut metric_out = writer(&options.out_dir, "metrics.ndjson")?;
    let mut measured_out = measured_writer(options)?;

    let mut summary = HistorySummary::default();
    let mut previous: Option<(Snapshot, BTreeMap<String, NodeMetrics>)> = None;

    for (index, commit) in commits.iter().enumerate() {
        let (snapshot, extract_ms) = if let Some(first) = first_python_snapshot.take() {
            first
        } else {
            extract_commit_timed(options, commit)?
        };
        let seconds = git::commit_time_seconds(&options.repo, commit).unwrap_or(0);
        if let Some(out) = measured_out.as_mut() {
            emit_measured(
                out,
                &options.out_dir,
                index,
                commit,
                seconds,
                &options.repo,
                &snapshot,
            )?;
        }

        let graph = Graph::from_snapshot(&snapshot);
        let current_metrics = analysis::metrics(&graph);

        let (transitions, metric_moves) = match previous.as_ref() {
            Some((base, base_metrics)) => {
                let diff = SnapshotDiff::between(base, &snapshot);
                let transitions =
                    emit_transitions(&mut transition_out, commit, seconds, base, &snapshot, &diff)?;
                let moves = emit_metric_moves(
                    &mut metric_out,
                    commit,
                    seconds,
                    base_metrics,
                    &current_metrics,
                )?;
                (transitions, moves)
            }
            // Genesis: no predecessor, so the whole snapshot is an addition.
            None => (
                emit_genesis(&mut transition_out, commit, seconds, &snapshot)?,
                0,
            ),
        };

        summary.commits += 1;
        summary.transitions += transitions;
        summary.metric_moves += metric_moves;

        writeln!(
            commit_out,
            "{}",
            serde_json::json!({
                "seq": index,
                "commit": commit,
                "ts": seconds,
                "nodes": snapshot.nodes.len(),
                "edges": snapshot.edges.len(),
                "transitions": transitions,
                "metric_moves": metric_moves,
                "extract_ms": extract_ms,
            })
        )?;

        if index % 25 == 0 || index + 1 == commits.len() {
            writeln!(
                progress,
                "  [{}/{}] {} — {} transitions, {} metric moves ({} ms)",
                index + 1,
                commits.len(),
                &commit[..12.min(commit.len())],
                transitions,
                metric_moves,
                extract_ms
            )
            .ok();
        }

        previous = Some((snapshot, current_metrics));
    }

    commit_out.flush()?;
    transition_out.flush()?;
    metric_out.flush()?;
    if let Some(out) = measured_out.as_mut() {
        out.flush()?;
    }
    summary.elapsed_ms = started.elapsed().as_millis();
    Ok(summary)
}

fn validate_options(options: &HistoryOptions) -> Result<()> {
    if options.language == Language::Python {
        anyhow::ensure!(
            !options.semantic,
            "Python semantic history has no accepted policy"
        );
        anyhow::ensure!(
            !options.measures && !options.twins,
            "Python measures, clones and twins have no accepted policy"
        );
        #[cfg(unix)]
        anyhow::ensure!(
            options.python_parser.is_some(),
            "Python history requires an explicit captured parser configuration"
        );
        #[cfg(not(unix))]
        anyhow::bail!("Python captured parser requires Unix");
    }
    #[cfg(unix)]
    anyhow::ensure!(
        options.python_parser.is_none() || options.language == Language::Python,
        "Python parser configuration requires language python"
    );
    Ok(())
}

/// The commits to walk, oldest first.
fn select_commits(options: &HistoryOptions) -> Result<Vec<String>> {
    let globs = crate::git::source_globs(options.language);
    let args = options.selector.rev_list_args(&options.tip, globs);
    let out = git::run(&options.repo, &args)?;
    let mut commits: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(std::string::ToString::to_string)
        .collect();
    // A limit keeps the most recent history, which is what a walk is usually
    // pointed at — so trim from the front, not the back.
    if let Some(limit) = options.limit {
        if commits.len() > limit {
            commits.drain(..commits.len() - limit);
        }
    }
    Ok(commits)
}

/// One commit's snapshot, in the mode the walk asked for.
fn extract_commit_timed(options: &HistoryOptions, commit: &str) -> Result<(Snapshot, u128)> {
    let started = Instant::now();
    Ok((
        extract_commit(options, commit)?,
        started.elapsed().as_millis(),
    ))
}

/// One commit's snapshot, in the mode the walk asked for.
fn extract_commit(options: &HistoryOptions, commit: &str) -> Result<Snapshot> {
    #[cfg(unix)]
    if let Some(parser) = &options.python_parser {
        return crate::python_syntax::extract_python_at_commit(&options.repo, commit, parser)
            .with_context(|| format!("extract Python at {commit}"));
    }
    let extracted = if options.semantic {
        crate::extract_at_commit_semantic_with(
            &options.repo,
            commit,
            options.language,
            options.measures,
            options.twins,
        )
    } else {
        crate::extract_at_commit_with(
            &options.repo,
            commit,
            options.language,
            options.measures,
            options.twins,
        )
    };
    extracted.with_context(|| format!("extract at {commit}"))
}

/// The `measured.ndjson` writer and its snapshot directory, when the walk
/// keeps measures; nothing is created otherwise.
fn measured_writer(options: &HistoryOptions) -> Result<Option<BufWriter<File>>> {
    if !options.measures {
        return Ok(None);
    }
    let dir = options.out_dir.join(MEASURES_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(Some(writer(&options.out_dir, "measured.ndjson")?))
}

/// Where a measured walk keeps its per-commit snapshots, under `out_dir`.
pub const MEASURES_DIR: &str = "measures";

/// Keep the measured snapshot of one commit and list it. The line carries
/// what a consumer needs to fold a series without opening git again: the
/// walk position, the commit, its time and author label, the counting
/// policy the snapshot was measured under, and the snapshot's path
/// relative to `out_dir`.
fn emit_measured(
    out: &mut dyn Write,
    out_dir: &Path,
    seq: usize,
    commit: &str,
    ts: i64,
    repo: &Path,
    snapshot: &Snapshot,
) -> Result<()> {
    let rel = format!(
        "{MEASURES_DIR}/{seq:05}-{}.json",
        &commit[..12.min(commit.len())]
    );
    let path = out_dir.join(&rel);
    std::fs::write(&path, crate::to_json(snapshot)?)
        .with_context(|| format!("write {}", path.display()))?;
    let (author, author_email) = git::commit_author(repo, commit)?;
    writeln!(
        out,
        "{}",
        serde_json::json!({
            "seq": seq,
            "commit": commit,
            "ts": ts,
            "author": author,
            "author_email": author_email,
            "measures_policy": snapshot.identity.measures_policy,
            "twins_policy": snapshot.identity.twins_policy,
            "snapshot_id": snapshot.identity.snapshot_id,
            "snapshot": rel,
        })
    )?;
    Ok(())
}

fn writer(dir: &Path, name: &str) -> Result<BufWriter<File>> {
    let path = dir.join(name);
    let file = File::create(&path).with_context(|| format!("create {}", path.display()))?;
    Ok(BufWriter::new(file))
}

/// Every fact in the first commit, as an addition.
fn emit_genesis(out: &mut dyn Write, commit: &str, ts: i64, snapshot: &Snapshot) -> Result<usize> {
    let mut count = 0;
    let node_keys = stable_node_keys(snapshot);
    for node in &snapshot.nodes {
        writeln!(
            out,
            "{}",
            node_line(commit, ts, "+", node, &node_keys[node.id.as_str()])
        )?;
        count += 1;
    }
    let keys = stable_edge_keys(snapshot);
    for edge in &snapshot.edges {
        writeln!(
            out,
            "{}",
            edge_line(commit, ts, "+", edge, &keys[edge.id.as_str()])
        )?;
        count += 1;
    }
    Ok(count)
}

/// Facts that appeared or disappeared between two adjacent commits.
///
/// Node *moves* (a span shifting because something above it changed) are
/// deliberately not transitions: the fact is the same fact. Only births and
/// deaths are emitted.
fn emit_transitions(
    out: &mut dyn Write,
    commit: &str,
    ts: i64,
    base: &Snapshot,
    head: &Snapshot,
    diff: &SnapshotDiff,
) -> Result<usize> {
    let mut count = 0;
    let head_nodes = stable_node_keys(head);
    let base_nodes = stable_node_keys(base);

    // Pair relocations first, so a refactor reads as symbols moving rather than
    // as a mass extinction followed by a mass birth. Whatever stays unpaired is
    // a genuine birth or death.
    let moves = infer_moves(diff);
    let relocated: BTreeSet<&str> = moves
        .iter()
        .flat_map(|m| [m.from.id.as_str(), m.to.id.as_str()])
        .collect();
    for moved in &moves {
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "commit": commit,
                "ts": ts,
                "fact": "node",
                "op": "->",
                "key": head_nodes[moved.to.id.as_str()],
                "was": base_nodes[moved.from.id.as_str()],
                "kind": moved.to.kind.to_string(),
                "name": moved.to.name,
                "path": moved.to.provenance.rel_path,
                "from_path": moved.from.provenance.rel_path,
            })
        )?;
        count += 1;
    }

    for node in &diff.added_nodes {
        if relocated.contains(node.id.as_str()) {
            continue;
        }
        writeln!(
            out,
            "{}",
            node_line(commit, ts, "+", node, &head_nodes[node.id.as_str()])
        )?;
        count += 1;
    }
    for node in &diff.removed_nodes {
        if relocated.contains(node.id.as_str()) {
            continue;
        }
        writeln!(
            out,
            "{}",
            node_line(commit, ts, "-", node, &base_nodes[node.id.as_str()])
        )?;
        count += 1;
    }
    let head_keys = stable_edge_keys(head);
    for edge in &diff.added_edges {
        writeln!(
            out,
            "{}",
            edge_line(commit, ts, "+", edge, &head_keys[edge.id.as_str()])
        )?;
        count += 1;
    }
    let base_keys = stable_edge_keys(base);
    for edge in &diff.removed_edges {
        writeln!(
            out,
            "{}",
            edge_line(commit, ts, "-", edge, &base_keys[edge.id.as_str()])
        )?;
        count += 1;
    }
    Ok(count)
}

/// Nodes whose fan-in or fan-out moved. This is the "what did that change
/// affect" half: a transition says a fact appeared, a metric move says what
/// that did to the shape around it. Non-authoritative.
///
/// Size changes are not reported — a node's byte length moves on any edit
/// inside its body, which would drown the coupling signal.
fn emit_metric_moves(
    out: &mut dyn Write,
    commit: &str,
    ts: i64,
    base: &BTreeMap<String, NodeMetrics>,
    head: &BTreeMap<String, NodeMetrics>,
) -> Result<usize> {
    let mut count = 0;
    for (id, now) in head {
        let Some(before) = base.get(id) else {
            // A new node's metrics are its birth, already carried by the node
            // transition; reporting them again would double-count.
            continue;
        };
        if before.fan_in == now.fan_in && before.fan_out == now.fan_out {
            continue;
        }
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "commit": commit,
                "ts": ts,
                "node": id,
                "fan_in": now.fan_in,
                "fan_out": now.fan_out,
                "prev_fan_in": before.fan_in,
                "prev_fan_out": before.fan_out,
            })
        )?;
        count += 1;
    }
    Ok(count)
}

fn node_line(commit: &str, ts: i64, op: &str, node: &codemap_graph::Node, key: &str) -> String {
    serde_json::json!({
        "commit": commit,
        "ts": ts,
        "fact": "node",
        "op": op,
        "key": key,
        "kind": node.kind.to_string(),
        "name": node.name,
        "path": node.provenance.rel_path,
    })
    .to_string()
}

fn edge_line(commit: &str, ts: i64, op: &str, edge: &codemap_graph::Edge, key: &str) -> String {
    serde_json::json!({
        "commit": commit,
        "ts": ts,
        "fact": "edge",
        "op": op,
        "key": key,
        "kind": edge.kind.tag(),
        "from": edge.from,
        "to": edge.to,
        "path": edge.provenance.rel_path,
    })
    .to_string()
}
