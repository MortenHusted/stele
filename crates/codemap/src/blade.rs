use std::sync::LazyLock;

use anyhow::{Context, Result};
use tree_sitter::{Language, Node, Parser};
use tree_sitter_language::LanguageFn;

use crate::schema::{BladeReferenceKind, Span};

unsafe extern "C" {
    fn tree_sitter_blade() -> *const ();
}

static LANGUAGE: LazyLock<Language> =
    LazyLock::new(|| Language::new(unsafe { LanguageFn::from_raw(tree_sitter_blade) }));

#[derive(Debug)]
pub struct Reference {
    pub kind: BladeReferenceKind,
    pub target: Option<String>,
    pub span: Span,
}

pub fn extract_file(source: &[u8]) -> Result<Vec<Reference>> {
    let mut parser = Parser::new();
    parser
        .set_language(&LANGUAGE)
        .context("set Blade grammar")?;
    let tree = parser.parse(source, None).context("parse Blade source")?;
    let mut references = Vec::new();
    visit(tree.root_node(), source, &mut references);
    if tree.root_node().has_error() {
        references.clear();
        references.push(Reference {
            kind: BladeReferenceKind::ParseError,
            target: None,
            span: node_span(tree.root_node()),
        });
    }
    references.sort_by_key(|reference| reference.span.start_byte);
    Ok(references)
}

fn visit(node: Node, source: &[u8], references: &mut Vec<Reference>) {
    if matches!(
        node.kind(),
        "comment" | "verbatim" | "php_statement" | "ERROR"
    ) {
        return;
    }
    if matches!(node.kind(), "directive" | "directive_start") {
        references.extend(directive(node, source));
    }
    if matches!(node.kind(), "start_tag" | "self_closing_tag") && !node.has_error() {
        references.extend(tag(node, source));
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit(child, source, references);
    }
}

fn directive_kind(text: &str) -> Option<BladeReferenceKind> {
    match text {
        "@extends" => Some(BladeReferenceKind::Extends),
        "@include" => Some(BladeReferenceKind::Include),
        "@includeIf" => Some(BladeReferenceKind::IncludeIf),
        "@includeWhen" => Some(BladeReferenceKind::IncludeWhen),
        "@includeUnless" => Some(BladeReferenceKind::IncludeUnless),
        "@includeFirst" => Some(BladeReferenceKind::IncludeFirst),
        "@component" => Some(BladeReferenceKind::Component),
        _ => None,
    }
}

fn directive(node: Node, source: &[u8]) -> Option<Reference> {
    let kind = directive_kind(node.utf8_text(source).unwrap_or_default())?;
    let parameter = node
        .next_named_sibling()
        .filter(|n| n.kind() == "parameter");
    let parsed = parameter.and_then(|_| directive_arguments(source, node.end_byte()));
    let target = parsed.as_ref().and_then(|(target, _)| target.clone());
    let mut span = node_span(node);
    if let Some((_, end)) = parsed {
        span.end_byte = end;
        let preceding = &source[..end];
        span.end_line =
            u32::try_from(preceding.split(|&byte| byte == b'\n').count()).unwrap_or(u32::MAX);
        span.end_col = u32::try_from(
            preceding
                .iter()
                .rposition(|&byte| byte == b'\n')
                .map_or(end, |newline| end - newline - 1),
        )
        .unwrap_or(u32::MAX);
    }
    Some(Reference { kind, target, span })
}

fn tag(node: Node, source: &[u8]) -> Option<Reference> {
    let mut cursor = node.walk();
    let name = node
        .named_children(&mut cursor)
        .find(|n| n.kind() == "tag_name")?;
    let name = name
        .utf8_text(source)
        .unwrap_or_default()
        .strip_prefix("x-")?;
    let dynamic = name == "dynamic-component";
    Some(Reference {
        kind: if dynamic {
            BladeReferenceKind::DynamicComponent
        } else {
            BladeReferenceKind::Tag
        },
        target: (!dynamic).then(|| name.to_owned()),
        span: node_span(node),
    })
}

fn directive_arguments(source: &[u8], after_directive: usize) -> Option<(Option<String>, usize)> {
    const PREFIX: &str = "<?php __stele";
    let mut php = PREFIX.as_bytes().to_vec();
    php.extend_from_slice(&source[after_directive..]);
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_php::LANGUAGE_PHP_ONLY.into())
        .ok()?;
    let tree = parser.parse(&php, None)?;
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_call_expression" && node.start_byte() == 6 {
            let arguments = node.child_by_field_name("arguments")?;
            if arguments.has_error()
                || node.has_error()
                || php.get(arguments.end_byte() - 1) != Some(&b')')
            {
                return None;
            }
            let value = arguments
                .named_child(0)
                .and_then(|argument| argument.named_child(0))
                .and_then(|argument| crate::php_syntax::string_node(argument, &php));
            return Some((value, after_directive + node.end_byte() - PREFIX.len()));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

fn node_span(node: Node) -> Span {
    Span::from_ts(
        node.start_byte(),
        node.end_byte(),
        (node.start_position().row, node.start_position().column),
        (node.end_position().row, node.end_position().column),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_distinguishes_literals_from_blade_text() {
        let source = b"@extends('layouts.base')\n@include('shared.nav', ['x' => call(1)])\n{{-- @include('comment') --}}\n@@include('escaped')\n@verbatim @include('verbatim') @endverbatim\n<x-card />\n@include($dynamic)\n";
        let references = extract_file(source).unwrap();
        let targets: Vec<_> = references
            .iter()
            .filter_map(|r| r.target.as_deref())
            .collect();
        assert_eq!(targets, ["layouts.base", "shared.nav", "card"]);
        assert_eq!(
            references
                .iter()
                .filter(|r| r.kind == BladeReferenceKind::Include && r.target.is_none())
                .count(),
            1
        );
    }
}
