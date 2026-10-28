//! Digests of catalog statements and examples: SHA-256 over a versioned
//! canonical rendering, so an approval or a run names exactly what it
//! approved or ran.

use proc_macro2::{Delimiter, Group, Spacing, TokenStream, TokenTree};
use quote::ToTokens;
use sha2::{Digest, Sha256};
use syn::{Attribute, ItemFn, Lit};

use crate::{Statement, StatementKind};

/// The version of the canonical renderings, `intent-digest/1`. Approvals
/// and runs record it beside each digest.
pub const DIGEST_VERSION: u64 = 1;

/// One `key: value` line each for the statement's kind, its prose with
/// whitespace runs collapsed, its links as resolved IDs, and an exception's
/// kind and sorted channels. File, line, alias spelling and comments are not
/// part of a statement, so moving or re-aliasing it keeps its digest.
pub fn canonical_statement(statement: &Statement) -> String {
    let text = statement
        .text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let (kind, links) = match &statement.kind {
        StatementKind::Intent => ("intent", String::new()),
        StatementKind::Channel => ("channel", String::new()),
        StatementKind::Definition { refines } => ("definition", format!("refines: {refines}\n")),
        StatementKind::Exception {
            narrows,
            kind,
            channels,
        } => {
            let mut channels = channels.clone();
            channels.sort();
            (
                "exception",
                format!(
                    "narrows: {narrows}\nkind: {}\nchannels: {}\n",
                    kind.as_str(),
                    channels.join(", ")
                ),
            )
        }
    };
    format!("intent-digest/{DIGEST_VERSION}\nstatement: {kind}\ntext: {text}\n{links}")
}

pub fn statement_digest(statement: &Statement) -> String {
    sha256(&canonical_statement(statement))
}

/// The test function's tokens, its attributes and doc comments included, one
/// space apart. An identifier is its name, a punct its char followed by `~`
/// when joined to the next token, a literal its source text, and a group its
/// tokens between its delimiters. Doc comment text is quoted here rather than
/// by proc-macro2, which synthesizes those literals. Comments and formatting
/// are not tokens, so they do not count. The attributes of the file and of
/// the modules around the function, when there are any, come first on a
/// `context:` line.
pub fn canonical_example(context: &[&Attribute], item: &ItemFn) -> String {
    let mut context_tokens = Vec::new();
    for attribute in context {
        render(attribute.to_token_stream(), &mut context_tokens);
    }
    let context = if context_tokens.is_empty() {
        String::new()
    } else {
        format!("context: {}\n", context_tokens.join(" "))
    };
    let mut tokens = Vec::new();
    render(item.to_token_stream(), &mut tokens);
    format!(
        "intent-digest/{DIGEST_VERSION}\n{context}{}",
        tokens.join(" ")
    )
}

pub fn example_digest(context: &[&Attribute], item: &ItemFn) -> String {
    sha256(&canonical_example(context, item))
}

pub(crate) fn sha256(text: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(text.as_bytes()))
}

fn render(stream: TokenStream, tokens: &mut Vec<String>) {
    for tree in stream {
        match tree {
            TokenTree::Ident(ident) => tokens.push(ident.to_string()),
            TokenTree::Punct(punct) => tokens.push(match punct.spacing() {
                Spacing::Alone => punct.as_char().to_string(),
                Spacing::Joint => format!("{}~", punct.as_char()),
            }),
            TokenTree::Literal(literal) => tokens.push(literal.to_string()),
            TokenTree::Group(group) => {
                let (open, close) = match group.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::None => ("«", "»"),
                };
                tokens.push(open.into());
                match doc_text(&group) {
                    Some(text) => tokens.extend(["doc".into(), "=".into(), doc_literal(&text)]),
                    None => render(group.stream(), tokens),
                }
                tokens.push(close.into());
            }
        }
    }
}

fn doc_text(group: &Group) -> Option<String> {
    if group.delimiter() != Delimiter::Bracket {
        return None;
    }
    let trees: Vec<TokenTree> = group.stream().into_iter().collect();
    match trees.as_slice() {
        [
            TokenTree::Ident(doc),
            TokenTree::Punct(eq),
            TokenTree::Literal(literal),
        ] if doc == "doc" && eq.as_char() == '=' => match Lit::new(literal.clone()) {
            Lit::Str(text) => Some(text.value()),
            _ => None,
        },
        _ => None,
    }
}

/// Doc comment text as the canonical form spells it. Unlike the catalog's
/// string syntax it leaves CR and tab as they are, and changing that would
/// change existing example digests.
fn doc_literal(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}
