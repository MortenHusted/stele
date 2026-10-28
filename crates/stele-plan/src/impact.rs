//! Which statements a code change touches: the catalog's `realized_by`
//! anchors and its examples, matched against the lines a diff changes in
//! code snapshots taken before and after it.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

use codemap_graph::{Node, NodeKind, Snapshot};
use serde_json::{Value, json};

use crate::{Statement, approvals::ItemKind, examples::Example};

/// One file of a diff: its paths and the line ranges it changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileDiff {
    /// The file's path at the base; `None` when the change adds it.
    pub old_path: Option<String>,
    /// The file's path after the change; `None` when the change deletes it.
    pub new_path: Option<String>,
    /// Changed lines at the base, as inclusive one-based ranges.
    pub old: Vec<(u32, u32)>,
    /// Changed lines after the change, as inclusive one-based ranges.
    pub new: Vec<(u32, u32)>,
}

impl FileDiff {
    fn is(&self, path: &str) -> bool {
        self.old_path.as_deref() == Some(path) || self.new_path.as_deref() == Some(path)
    }
}

/// The files and changed line ranges of `git diff --unified=0
/// --no-renames` output. A range of no lines, as an insertion has on the
/// base side, is left out; a binary file is listed with no ranges.
pub fn parse_hunks(diff: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    for line in diff.lines() {
        if let Some(header) = line.strip_prefix("diff --git ") {
            let path = header_path(header);
            files.push(FileDiff {
                old_path: path.clone(),
                new_path: path,
                ..FileDiff::default()
            });
        } else if let Some(file) = files.last_mut() {
            if let Some(path) = line.strip_prefix("--- ") {
                file.old_path = side_path(path, "a/");
            } else if let Some(path) = line.strip_prefix("+++ ") {
                file.new_path = side_path(path, "b/");
            } else if let Some(hunk) = line.strip_prefix("@@ ") {
                let mut ranges = hunk.split_whitespace();
                if let Some(range) = ranges.next().and_then(|old| old.strip_prefix('-')) {
                    file.old.extend(line_range(range));
                }
                if let Some(range) = ranges.next().and_then(|new| new.strip_prefix('+')) {
                    file.new.extend(line_range(range));
                }
            }
        }
    }
    files
}

/// The path in `a/P b/P`, the header of a file whose name did not change.
fn header_path(header: &str) -> Option<String> {
    let path = header.strip_prefix("a/")?;
    let half = path.len().checked_sub(3)? / 2;
    let (old, new) = (path.get(..half)?, path.get(half..)?);
    (new.strip_prefix(" b/") == Some(old)).then(|| old.to_string())
}

/// The path on a `---` or `+++` line, `None` for `/dev/null`.
fn side_path(path: &str, prefix: &str) -> Option<String> {
    let path = unquote(path.trim_end_matches('\t'));
    path.strip_prefix(prefix).map(str::to_string)
}

/// A path git C-quoted because it holds a quote, a backslash or a control
/// character, unquoted; any other path as it is.
fn unquote(path: &str) -> String {
    let Some(quoted) = path
        .strip_prefix('"')
        .and_then(|path| path.strip_suffix('"'))
    else {
        return path.to_string();
    };
    let mut bytes = Vec::new();
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buffer = [0; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => bytes.push(b'\n'),
            Some('t') => bytes.push(b'\t'),
            Some(digit @ '0'..='7') => {
                let octal: String = [Some(digit), chars.next(), chars.next()]
                    .into_iter()
                    .flatten()
                    .collect();
                bytes.push(u8::from_str_radix(&octal, 8).unwrap_or(b'?'));
            }
            Some(other) => {
                let mut buffer = [0; 4];
                bytes.extend_from_slice(other.encode_utf8(&mut buffer).as_bytes());
            }
            None => {}
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `start[,count]` as an inclusive range; `None` for a count of 0.
fn line_range(range: &str) -> Option<(u32, u32)> {
    let (start, count) = match range.split_once(',') {
        Some((start, count)) => (start.parse().ok()?, count.parse().ok()?),
        None => (range.parse().ok()?, 1),
    };
    (count > 0).then(|| (start, start + count - 1))
}

/// How a mapped anchor changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorChange {
    Changed,
    Added,
    Removed,
}

impl AnchorChange {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::Added => "added",
            Self::Removed => "removed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchedAnchor {
    pub anchor: String,
    pub change: AnchorChange,
    /// Where the anchor is, as `file:first-last`; a removed symbol where it
    /// was at the base, a path anchor as its path.
    pub location: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchedStatement {
    pub id: String,
    /// `definition` or `exception`.
    pub kind: &'static str,
    pub anchors: Vec<TouchedAnchor>,
    /// Every example of the statement, with whether its test changed.
    pub examples: Vec<(String, bool)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchedExample {
    pub id: String,
    pub statement: String,
    pub file: String,
}

/// What a change touches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Impact {
    /// Each statement with a touched anchor, in ID order.
    pub statements: Vec<TouchedStatement>,
    /// The symbol keys of the innermost declarations a change overlaps that
    /// no anchor covers and no example tags as its test, sorted.
    pub unmapped: Vec<String>,
    /// The examples whose test file changed, in scan order.
    pub examples: Vec<TouchedExample>,
}

/// One side of a change: its snapshots, one per language, and the changed
/// ranges of each file.
struct Side<'a> {
    nodes: BTreeMap<&'a str, Vec<&'a Node>>,
    snapshots: &'a [Snapshot],
    ranges: BTreeMap<&'a str, &'a [(u32, u32)]>,
}

impl<'a> Side<'a> {
    fn new(snapshots: &'a [Snapshot], diff: &'a [FileDiff], new: bool) -> Self {
        let mut nodes: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
        for node in snapshots.iter().flat_map(|snapshot| &snapshot.nodes) {
            nodes
                .entry(node.symbol_key.as_str())
                .or_default()
                .push(node);
        }
        let ranges = diff
            .iter()
            .filter_map(|file| {
                let (path, ranges) = if new {
                    (file.new_path.as_deref()?, &file.new)
                } else {
                    (file.old_path.as_deref()?, &file.old)
                };
                Some((path, ranges.as_slice()))
            })
            .collect();
        Self {
            nodes,
            snapshots,
            ranges,
        }
    }

    fn nodes(&self, key: &str) -> &[&'a Node] {
        self.nodes.get(key).map_or(&[], Vec::as_slice)
    }

    fn touched(&self, key: &str) -> bool {
        self.nodes(key).iter().any(|node| self.overlaps(node))
    }

    fn overlaps(&self, node: &Node) -> bool {
        self.ranges
            .get(node.provenance.rel_path.as_str())
            .is_some_and(|ranges| ranges.iter().any(|range| overlaps(node, *range)))
    }

    /// The innermost declarations each changed range overlaps.
    fn changed_declarations(&self) -> Vec<&'a Node> {
        let mut changed = Vec::new();
        for (path, ranges) in &self.ranges {
            let declarations: Vec<&Node> = self
                .snapshots
                .iter()
                .flat_map(|snapshot| &snapshot.nodes)
                .filter(|node| {
                    node.provenance.rel_path == *path
                        && !matches!(node.kind, NodeKind::File | NodeKind::ImportRef)
                })
                .collect();
            for range in *ranges {
                let hit: Vec<&Node> = declarations
                    .iter()
                    .copied()
                    .filter(|node| overlaps(node, *range))
                    .collect();
                changed.extend(
                    hit.iter()
                        .copied()
                        .filter(|outer| !hit.iter().any(|inner| strictly_within(inner, outer))),
                );
            }
        }
        changed
    }

    /// Whether a node with key `anchor` spans `node`.
    fn covers(&self, anchor: &str, node: &Node) -> bool {
        self.nodes(anchor).iter().any(|outer| within(node, outer))
    }
}

/// The inclusive lines a node spans. An end at column 0 does not cover
/// its last line.
fn lines(node: &Node) -> (u32, u32) {
    let span = node.provenance.span;
    let last = span.end_line.saturating_sub(u32::from(span.end_col == 0));
    (span.start_line, last.max(span.start_line))
}

fn overlaps(node: &Node, (first, last): (u32, u32)) -> bool {
    let (start, end) = lines(node);
    start <= last && first <= end
}

fn within(inner: &Node, outer: &Node) -> bool {
    let (inner_span, outer_span) = (inner.provenance.span, outer.provenance.span);
    inner.provenance.rel_path == outer.provenance.rel_path
        && outer_span.start_byte <= inner_span.start_byte
        && inner_span.end_byte <= outer_span.end_byte
}

fn strictly_within(inner: &Node, outer: &Node) -> bool {
    let (inner_span, outer_span) = (inner.provenance.span, outer.provenance.span);
    within(inner, outer)
        && (inner_span.start_byte, inner_span.end_byte)
            != (outer_span.start_byte, outer_span.end_byte)
}

/// Whether `node` is the test function `example` tags.
fn is_test_of(node: &Node, example: &Example) -> bool {
    node.provenance.rel_path == example.file
        && node
            .symbol_key
            .split_once('#')
            .is_some_and(|(_, path)| path == example.test)
}

fn location(node: &Node) -> String {
    let (first, last) = lines(node);
    let path = &node.provenance.rel_path;
    if first == last {
        format!("{path}:{first}")
    } else {
        format!("{path}:{first}-{last}")
    }
}

/// Which statements, anchors and examples the change `diff` touches, from
/// the code at the base (`base`) to the code after it (`head`), each one
/// snapshot per language.
///
/// A symbol anchor is touched when one of its declarations overlaps a
/// changed line on either side, so an anchor on a declaration that spans
/// others counts a change to any of them; it is removed when only the base
/// has it, and added when only the head does. A path anchor is touched
/// when the diff names its file, and an example when the diff names its
/// test file.
pub fn impact(
    catalog: &BTreeMap<String, Statement>,
    examples: &[Example],
    base: &[Snapshot],
    head: &[Snapshot],
    diff: &[FileDiff],
) -> Impact {
    let before = Side::new(base, diff, false);
    let after = Side::new(head, diff, true);
    let test_changed = |example: &Example| diff.iter().any(|file| file.is(&example.file));

    let mut statements = Vec::new();
    for statement in catalog.values() {
        let anchors: Vec<TouchedAnchor> = statement
            .realized_by
            .iter()
            .filter_map(|anchor| touched_anchor(anchor, &before, &after, diff))
            .collect();
        if anchors.is_empty() {
            continue;
        }
        statements.push(TouchedStatement {
            id: statement.id.clone(),
            kind: ItemKind::of(&statement.kind).as_str(),
            anchors,
            examples: examples
                .iter()
                .filter(|example| example.statement == statement.id)
                .map(|example| (example.id.clone(), test_changed(example)))
                .collect(),
        });
    }

    let anchors: BTreeSet<&str> = catalog
        .values()
        .flat_map(|statement| &statement.realized_by)
        .map(String::as_str)
        .collect();
    let unmapped = [&before, &after]
        .into_iter()
        .flat_map(|side| {
            side.changed_declarations()
                .into_iter()
                .filter(|node| {
                    !anchors.iter().any(|anchor| {
                        side.covers(anchor, node) || *anchor == node.provenance.rel_path
                    }) && !examples.iter().any(|example| is_test_of(node, example))
                })
                .map(|node| node.symbol_key.clone())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    Impact {
        statements,
        unmapped,
        examples: examples
            .iter()
            .filter(|example| test_changed(example))
            .map(|example| TouchedExample {
                id: example.id.clone(),
                statement: example.statement.clone(),
                file: example.file.clone(),
            })
            .collect(),
    }
}

/// How `anchor` changed, when the change touches it.
fn touched_anchor(
    anchor: &str,
    before: &Side<'_>,
    after: &Side<'_>,
    diff: &[FileDiff],
) -> Option<TouchedAnchor> {
    // A path anchor names no symbol: it is touched with its file.
    if !anchor.contains(':') {
        return diff
            .iter()
            .any(|file| file.is(anchor))
            .then(|| TouchedAnchor {
                anchor: anchor.into(),
                change: AnchorChange::Changed,
                location: anchor.into(),
            });
    }
    let (was, is) = (before.nodes(anchor).first(), after.nodes(anchor).first());
    let change = match (was, is) {
        (Some(_), None) => AnchorChange::Removed,
        (None, Some(_)) => AnchorChange::Added,
        (Some(_), Some(_)) if before.touched(anchor) || after.touched(anchor) => {
            AnchorChange::Changed
        }
        _ => return None,
    };
    Some(TouchedAnchor {
        anchor: anchor.into(),
        change,
        location: location(is.or(was)?),
    })
}

impl Impact {
    /// The impact as Markdown, with each statement's standing when
    /// `standings` has one. `compared` names what was compared, such as
    /// "`main` to the working tree".
    pub fn markdown(&self, compared: &str, standings: &BTreeMap<String, String>) -> String {
        let mut out =
            format!("# Intent impact\n\nChanges from {compared}.\n\n## Touched statements\n\n");
        if self.statements.is_empty() {
            out.push_str("None.\n\n");
        }
        for statement in &self.statements {
            let standing = standings
                .get(&statement.id)
                .map_or(String::new(), |standing| format!(": {standing}"));
            let _ = writeln!(
                out,
                "### `{}` ({}){standing}\n",
                statement.id, statement.kind
            );
            for anchor in &statement.anchors {
                let at = if anchor.location == anchor.anchor {
                    String::new()
                } else {
                    format!(" at `{}`", anchor.location)
                };
                let _ = writeln!(
                    out,
                    "- {}: `{}`{at}",
                    capitalized(anchor.change.as_str()),
                    anchor.anchor
                );
            }
            if !statement.examples.is_empty() {
                let examples: Vec<String> = statement
                    .examples
                    .iter()
                    .map(|(id, changed)| {
                        if *changed {
                            format!("`{id}` (test changed)")
                        } else {
                            format!("`{id}`")
                        }
                    })
                    .collect();
                let _ = writeln!(out, "- Examples: {}", examples.join(", "));
            }
            out.push('\n');
        }
        out.push_str("## Changed code no statement maps\n\n");
        if self.unmapped.is_empty() {
            out.push_str("None.\n");
        }
        for key in &self.unmapped {
            let _ = writeln!(out, "- `{key}`");
        }
        out.push_str("\n## Examples whose test changed\n\n");
        if self.examples.is_empty() {
            out.push_str("None.\n");
        }
        for example in &self.examples {
            let _ = writeln!(
                out,
                "- `{}` of `{}` in `{}`",
                example.id, example.statement, example.file
            );
        }
        out
    }

    /// The impact as JSON, with the same data as [`Impact::markdown`].
    pub fn json(&self, standings: &BTreeMap<String, String>) -> Value {
        json!({
            "statements": self.statements.iter().map(|statement| json!({
                "id": statement.id,
                "kind": statement.kind,
                "standing": standings.get(&statement.id),
                "anchors": statement.anchors.iter().map(|anchor| json!({
                    "anchor": anchor.anchor,
                    "change": anchor.change.as_str(),
                    "location": anchor.location,
                })).collect::<Vec<_>>(),
                "examples": statement.examples.iter().map(|(id, changed)| json!({
                    "id": id,
                    "test_changed": changed,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "unmapped": self.unmapped,
            "examples": self.examples.iter().map(|example| json!({
                "id": example.id,
                "statement": example.statement,
                "file": example.file,
            })).collect::<Vec<_>>(),
        })
    }
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}
