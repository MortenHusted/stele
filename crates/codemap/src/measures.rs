//! What every tree-sitter adapter's measures share: the one line
//! eligibility rule, the per-callable measure record, and the normalised
//! token stream clone detection reads.
//!
//! The counting rules themselves (which nodes are decision points) live in
//! each language module next to its grammar, under the policy name the
//! identity records.

use tree_sitter::Node;

use crate::clones::Token;

/// The class of one source line under the one eligibility rule: blank
/// (whitespace only), comment-only (its only non-whitespace bytes lie
/// inside comment nodes), or source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineClass {
    Blank,
    Comment,
    Source,
}

/// Line classes of a span; `loc` is the sum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineCounts {
    pub loc: u32,
    pub sloc: u32,
    pub blank: u32,
    pub comment: u32,
}

/// The form measures of one callable: its signature width, its depth,
/// its longest call chain, and the instance state it writes. Counted by
/// the adapters whose policy carries them (`schema::form_measures_available`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Form {
    pub params: u32,
    pub nesting: u32,
    pub chain: u32,
    /// Absent for a language without instance variables.
    pub ivars: Option<u32>,
}

/// Measures of one callable definition, keyed by its index in the
/// adapter's `defs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMeasure {
    pub def: usize,
    pub cc: u32,
    pub lines: LineCounts,
    /// `None` under a policy without form measures.
    pub form: Option<Form>,
}

/// Byte ranges of every comment node in the tree, in document order.
pub fn comment_ranges(root: Node, comment_kinds: &[&str]) -> Vec<(usize, usize)> {
    fn visit(node: Node, comment_kinds: &[&str], out: &mut Vec<(usize, usize)>) {
        if comment_kinds.contains(&node.kind()) {
            out.push((node.start_byte(), node.end_byte()));
            return;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(child, comment_kinds, out);
        }
    }
    let mut out = Vec::new();
    visit(root, comment_kinds, &mut out);
    out
}

/// Classify every line of `source`. A line ends at `\n`; a final segment
/// without one still counts when it holds bytes, and a file that ends in
/// `\n` has no empty extra line after it. A line is source when any
/// non-whitespace byte lies outside every comment range, comment-only when
/// its non-whitespace bytes all lie inside comment ranges, blank otherwise.
pub fn line_classes(source: &[u8], comments: &[(usize, usize)]) -> Vec<LineClass> {
    let mut classes = Vec::new();
    let mut has_source = false;
    let mut has_comment = false;
    let mut next_comment = 0;
    let mut line_started = false;
    for (offset, byte) in source.iter().enumerate() {
        if *byte == b'\n' {
            classes.push(classify(has_source, has_comment));
            has_source = false;
            has_comment = false;
            line_started = false;
            continue;
        }
        line_started = true;
        if byte.is_ascii_whitespace() {
            continue;
        }
        while next_comment < comments.len() && comments[next_comment].1 <= offset {
            next_comment += 1;
        }
        let in_comment = comments
            .get(next_comment)
            .is_some_and(|&(start, end)| start <= offset && offset < end);
        if in_comment {
            has_comment = true;
        } else {
            has_source = true;
        }
    }
    if line_started {
        classes.push(classify(has_source, has_comment));
    }
    classes
}

const fn classify(has_source: bool, has_comment: bool) -> LineClass {
    if has_source {
        LineClass::Source
    } else if has_comment {
        LineClass::Comment
    } else {
        LineClass::Blank
    }
}

/// Line counts over `start_line..=end_line` (1-indexed, as in `Span`),
/// clamped to the classified lines.
pub fn count_lines(classes: &[LineClass], start_line: u32, end_line: u32) -> LineCounts {
    let mut counts = LineCounts::default();
    let first = start_line.saturating_sub(1) as usize;
    let last = (end_line as usize).min(classes.len());
    for class in classes.iter().take(last).skip(first) {
        counts.loc += 1;
        match class {
            LineClass::Blank => counts.blank += 1,
            LineClass::Comment => counts.comment += 1,
            LineClass::Source => counts.sloc += 1,
        }
    }
    counts
}

/// Which node kinds a language drops, names, or quotes when its leaves
/// become the normalised token stream.
#[derive(Debug, Clone, Copy)]
pub struct TokenRules {
    /// Dropped entirely.
    pub comments: &'static [&'static str],
    /// Emitted as `$id`.
    pub identifiers: &'static [&'static str],
    /// Emitted as `$lit`, one token whatever the node contains.
    pub literals: &'static [&'static str],
}

/// The file's leaves in document order as clone detection reads them:
/// comments dropped, every identifier `$id`, every literal `$lit`, every
/// other leaf its own text.
pub fn token_stream(root: Node, source: &[u8], rules: &TokenRules) -> Vec<Token> {
    fn visit(node: Node, source: &[u8], rules: &TokenRules, out: &mut Vec<Token>) {
        let kind = node.kind();
        if rules.comments.contains(&kind) {
            return;
        }
        let placeholder = if rules.literals.contains(&kind) {
            Some("$lit")
        } else if rules.identifiers.contains(&kind) {
            Some("$id")
        } else {
            None
        };
        if let Some(text) = placeholder {
            push_token(node, text.to_string(), out);
            return;
        }
        if node.child_count() == 0 {
            let text = String::from_utf8_lossy(&source[node.byte_range()]).into_owned();
            if !text.trim().is_empty() {
                push_token(node, text, out);
            }
            return;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(child, source, rules, out);
        }
    }
    fn push_token(node: Node, text: String, out: &mut Vec<Token>) {
        out.push(Token {
            text,
            start_line: u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX),
            end_line: u32::try_from(node.end_position().row + 1).unwrap_or(u32::MAX),
        });
    }
    let mut out = Vec::new();
    visit(root, source, rules, &mut out);
    out
}

/// How many of `node`'s direct children are operator tokens in `ops`.
pub fn operator_count(node: Node, ops: &[&str]) -> u32 {
    let mut cursor = node.walk();
    u32::try_from(
        node.children(&mut cursor)
            .filter(|token| ops.contains(&token.kind()))
            .count(),
    )
    .unwrap_or(u32::MAX)
}

/// Named children of `node` whose kind is in `kinds`, minus one (the first
/// arm or case of a branch is not a decision), saturating at zero.
pub fn arms_beyond_first(node: Node, kinds: &[&str]) -> u32 {
    let mut cursor = node.walk();
    let arms = node
        .named_children(&mut cursor)
        .filter(|child| kinds.contains(&child.kind()))
        .count();
    u32::try_from(arms.saturating_sub(1)).unwrap_or(u32::MAX)
}
