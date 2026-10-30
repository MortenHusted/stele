//! Tree-sitter Rust syntax extraction.
//!
//! Owns parsing and language-specific extraction only. It never guesses
//! call targets, runs an LSP, or resolves cross-file references — that is
//! [`crate::resolve`]. This module produces raw, source-addressed
//! observations; [`crate::extract`] assigns deterministic IDs and
//! [`crate::resolve`] upgrades resolvable observations into edges.
//!
//! What is emitted directly from syntax (no resolution needed):
//! - **definition nodes** (struct/enum/union/type-alias/trait/function/method/module/macro);
//! - **lexical `contains`** edges (file/module → nested item);
//! - **`imports`** edges (file → `ImportRef` placeholder), preserving the
//!   syntactic `use` path verbatim rather than pretending it resolves
//!   in-repository.
//!
//! What is collected as observations for later resolution:
//! - **`impl Trait for Type`** → [`RawImpl`], upgraded to an `implements`
//!   edge only when both sides resolve to in-repository nodes;
//! - **call expressions** → [`RawCall`], reported as observations and
//!   upgraded to `calls` edges only by semantic resolution.
//!
//! Measures (counts, never facts about structure) are computed here too,
//! because only the parse tree knows them: cyclomatic complexity per
//! callable under [`cyclomatic`] and line classes per callable span and
//! per file under [`line_classes`]. The extractor emits them only when
//! asked (`--measures`), under the policy named by
//! [`crate::schema::MEASURES_POLICY_RUST_FORM_V1`].

use std::sync::OnceLock;

use anyhow::{Context, Result};

use crate::clones::Token;
use crate::measures::{
    Form, LineClass, LineCounts, RawMeasure, TokenRules, comment_ranges, count_lines, line_classes,
    token_stream,
};
use tree_sitter::{Language, Node, Parser, Query};

use crate::schema::{NodeKind, Span, TypeKind};

const RUST_TAGS: &str = include_str!("queries/rust/tags.scm");
const RUST_EXTENSIONS: &str = include_str!("queries/rust/extensions.scm");

static RUST_LANGUAGE: OnceLock<Language> = OnceLock::new();

fn rust_language() -> &'static Language {
    RUST_LANGUAGE.get_or_init(|| tree_sitter_rust::LANGUAGE.into())
}

/// Compiled query, used only to validate that the vendored query files
/// compile against the pinned grammar. The recursive walker below performs
/// extraction; the query is a compile-time contract check.
static RUST_QUERY: OnceLock<Query> = OnceLock::new();

fn rust_query() -> &'static Query {
    RUST_QUERY.get_or_init(|| {
        let lang = rust_language();
        let combined = format!("{RUST_TAGS}\n{RUST_EXTENSIONS}");
        Query::new(lang, &combined).expect("rust tags + extensions query compiles")
    })
}

/// A raw definition observation, before ID assignment.
#[derive(Debug, Clone)]
pub struct RawDef {
    pub kind: NodeKind,
    pub name: String,
    /// Semantic scope chain (enclosing module names plus, for methods, the
    /// impl type name). Root-first. Drives `symbol_key` and node ID.
    pub scope: Vec<String>,
    pub span: Span,
    /// Index into [`RawFile::defs`] of the lexical parent (a Module), or
    /// `None` when the parent is the file itself.
    pub parent: Option<usize>,
}

/// A raw `use` declaration.
#[derive(Debug, Clone)]
pub struct RawImport {
    /// Verbatim path text, e.g. `std::collections::HashMap`.
    pub path: String,
    pub span: Span,
}

/// A raw `impl` block observation.
#[derive(Debug, Clone)]
pub struct RawImpl {
    /// `Some(trait_name)` for `impl Trait for Type`; `None` for inherent impls.
    pub trait_name: Option<String>,
    pub type_name: String,
    pub scope: Vec<String>,
    pub trait_name_start: Option<usize>,
    pub type_name_start: Option<usize>,
    pub generic: bool,
    pub negative: bool,
    pub imported_binding: bool,
    pub visible_glob: bool,
    pub local_scope: bool,
    pub span: Span,
}

/// Receiver shape of a call site, recorded for the semantic edge policy.
/// Ruby semantic resolution admits constant, implicit-self and verified
/// direct-constructor call shapes. Every other shape is guess regime and never becomes a
/// canonical edge. Rust extraction records [`ReceiverShape::Other`] —
/// rust-analyzer infers receivers itself, so the shape is irrelevant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiverShape {
    /// Bare call with no explicit receiver (`save`, `run`) or an explicit
    /// `self` receiver (`self.save`): implicit self in the enclosing scope.
    ImplicitSelf,
    /// Constant receiver (`User.find`, `Greeter.create`), recorded as
    /// written (`User`, `Admin::User`).
    Constant(String),
    /// Ruby instance returned by a direct constant-receiver construction.
    /// The `new` site must separately resolve to the default Class#new provider.
    Constructed {
        class_name: String,
        constructor_start: usize,
    },
    /// `parent::m()` — resolved through the verified superclass of the
    /// enclosing class. PHP-only: policy maps this to the enclosing
    /// class's `inherits` target before admitting.
    Parent,
    /// A receiver whose class is declared in the source, recorded as the
    /// namespace-qualified declared type: a typed property reached as
    /// `$this->prop->m()`, or a typed parameter `$param->m()` that the
    /// callable body never assigns. PHP-only: the runtime enforces
    /// declared types, so the receiver holds an instance of that class, a
    /// subclass, or an implementation.
    Declared(String),
    /// Any other receiver: local variable, ivar, chained call, or a
    /// language (Rust) where the shape carries no policy meaning.
    Other,
}

/// A raw call-site observation.
#[derive(Debug, Clone)]
pub struct RawCall {
    pub raw_name: String,
    /// Root of a `crate::path::call` target, when syntactically explicit.
    /// This is an observation hint only; it never creates a call edge.
    pub path_root: Option<String>,
    /// Whole call-expression span (for reporting).
    pub span: Span,
    /// Start byte of the called identifier (for LSP goto-definition).
    pub name_start_byte: usize,
    /// Receiver shape used by the semantic edge policy (see above).
    pub receiver: ReceiverShape,
    /// Enclosing class/module scope chain of the call site (innermost
    /// last). Ruby-only policy input (implicit-self verification); empty
    /// for Rust calls.
    pub scope: Vec<String>,
}

/// How Rust leaves become the normalised token stream: comments dropped,
/// identifiers of every flavour `$id`, string, raw string, char, number,
/// and boolean literals `$lit`; lifetimes keep their tick and normalise
/// their name; primitive type names and keywords stay as written.
pub const RUST_TOKENS: TokenRules = TokenRules {
    comments: &["line_comment", "block_comment"],
    identifiers: &[
        "identifier",
        "field_identifier",
        "type_identifier",
        "shorthand_field_identifier",
        "metavariable",
    ],
    literals: &[
        "string_literal",
        "raw_string_literal",
        "char_literal",
        "integer_literal",
        "float_literal",
        "boolean_literal",
    ],
};

/// Per-file extraction output.
#[derive(Debug, Clone)]
pub struct RawFile {
    /// `(def index, declaration name start byte)` for functions and methods.
    pub callable_declarations: Vec<(usize, usize)>,
    pub type_declarations: Vec<(usize, usize)>,
    pub defs: Vec<RawDef>,
    pub imports: Vec<RawImport>,
    pub impls: Vec<RawImpl>,
    pub calls: Vec<RawCall>,
    /// Error and missing nodes anywhere in the syntax tree.
    pub parse_errors: usize,
    /// The first of those nodes in source order.
    pub first_parse_error: Option<Span>,
    /// One entry per callable def (function, method, macro).
    pub measures: Vec<RawMeasure>,
    /// Line classes of the whole file.
    pub file_lines: LineCounts,
    /// Every line's class, for clone occurrence counts.
    pub line_classes: Vec<LineClass>,
    /// The normalised token stream clone detection reads.
    pub tokens: Vec<Token>,
}

/// Extract structural observations from a single Rust source file.
pub fn extract_file(rel_path: &str, source: &[u8]) -> Result<RawFile> {
    // Validate the vendored query compiles (cheap, cached).
    let _ = rust_query();

    let mut parser = Parser::new();
    parser
        .set_language(rust_language())
        .context("set tree-sitter-rust language")?;
    let tree = parser
        .parse(source, None)
        .context("tree-sitter parse returned None")?;
    let root = tree.root_node();

    let (parse_errors, first_parse_error) = syntax_errors(root);
    let mut walker = Walker::new(rel_path, source);
    walker.walk(root, None, &[], None);
    for implementation in &mut walker.impls {
        implementation.local_scope = walker.defs.iter().any(|definition| {
            matches!(definition.kind, NodeKind::Function | NodeKind::Method)
                && definition.span.start_byte <= implementation.span.start_byte
                && implementation.span.end_byte <= definition.span.end_byte
        });
        for (scope, path) in &walker.scoped_uses {
            if !implementation.scope.starts_with(scope) {
                continue;
            }
            implementation.visible_glob |= path.contains('*');
            implementation.imported_binding |= path
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|name| {
                    name == implementation.type_name
                        || implementation.trait_name.as_deref() == Some(name)
                });
        }
    }

    let classes = line_classes(source, &comment_ranges(root, RUST_TOKENS.comments));
    let file_lines = count_lines(
        &classes,
        1,
        u32::try_from(classes.len()).unwrap_or(u32::MAX),
    );
    let measures = walker
        .callables
        .iter()
        .map(|&(def, cc, form)| {
            let span = walker.defs[def].span;
            RawMeasure {
                def,
                cc,
                lines: count_lines(&classes, span.start_line, span.end_line),
                form: Some(form),
            }
        })
        .collect();

    let tokens = token_stream(root, source, &RUST_TOKENS);

    Ok(RawFile {
        callable_declarations: walker.callable_declarations,
        type_declarations: walker.type_declarations,
        defs: walker.defs,
        imports: walker.imports,
        impls: walker.impls,
        calls: walker.calls,
        parse_errors,
        first_parse_error,
        measures,
        file_lines,
        line_classes: classes,
        tokens,
    })
}

struct Walker<'a> {
    callable_declarations: Vec<(usize, usize)>,
    type_declarations: Vec<(usize, usize)>,
    scoped_uses: Vec<(Vec<String>, String)>,
    source: &'a [u8],
    defs: Vec<RawDef>,
    imports: Vec<RawImport>,
    impls: Vec<RawImpl>,
    calls: Vec<RawCall>,
    /// `(def index, cyclomatic complexity)` for every callable def.
    callables: Vec<(usize, u32, Form)>,
}

impl<'a> Walker<'a> {
    const fn new(_rel_path: &'a str, source: &'a [u8]) -> Self {
        Self {
            source,
            callable_declarations: Vec::new(),
            type_declarations: Vec::new(),
            scoped_uses: Vec::new(),
            defs: Vec::new(),
            imports: Vec::new(),
            impls: Vec::new(),
            calls: Vec::new(),
            callables: Vec::new(),
        }
    }

    /// Recursive walk. `parent_idx` is the index of the enclosing Module def
    /// (or None for file-level). `mod_stack` is the chain of enclosing module
    /// names. `impl_type` is the current impl's type name (for method scope).
    fn walk(
        &mut self,
        node: Node,
        parent_idx: Option<usize>,
        mod_stack: &[String],
        impl_type: Option<&str>,
    ) {
        match node.kind() {
            "mod_item" => self.walk_mod(node, parent_idx, mod_stack),
            "struct_item" => {
                self.walk_type(node, TypeKind::Struct, parent_idx, mod_stack, impl_type);
            }
            "enum_item" => self.walk_type(node, TypeKind::Enum, parent_idx, mod_stack, impl_type),
            "union_item" => self.walk_type(node, TypeKind::Union, parent_idx, mod_stack, impl_type),
            "type_item" => {
                self.walk_type(node, TypeKind::TypeAlias, parent_idx, mod_stack, impl_type);
            }
            "trait_item" => self.walk_trait(node, parent_idx, mod_stack),
            "function_item" => self.walk_function(node, parent_idx, mod_stack, impl_type),
            "impl_item" => self.walk_impl(node, parent_idx, mod_stack),
            "use_declaration" => self.walk_use(node, mod_stack),
            "macro_definition" => self.walk_macro(node, parent_idx, mod_stack),
            "call_expression" => self.walk_call(node),
            "macro_invocation" => self.walk_macro_call(node),
            _ => {}
        }

        // Recurse into children for nested items. Definitions above already
        // recurse into their own bodies where nesting matters, so we skip
        // re-entering them here to avoid double-processing.
        let handled = matches!(
            node.kind(),
            "mod_item"
                | "struct_item"
                | "enum_item"
                | "union_item"
                | "type_item"
                | "trait_item"
                | "impl_item"
                | "function_item"
        );
        if !handled {
            let count = node.child_count();
            for i in 0..count {
                if let Some(child) = node.child(i) {
                    self.walk(child, parent_idx, mod_stack, impl_type);
                }
            }
        }
    }

    fn walk_mod(&mut self, node: Node, parent_idx: Option<usize>, mod_stack: &[String]) {
        let name = self.field_text(node, "name").unwrap_or_default();
        let span = ts_span(node);
        let idx = self.defs.len();
        self.defs.push(RawDef {
            kind: NodeKind::Module,
            name: name.clone(),
            scope: mod_stack.to_vec(),
            span,
            parent: parent_idx,
        });

        let mut new_stack: Vec<String> = mod_stack.to_vec();
        new_stack.push(name);
        // Recurse into the body (declaration_list) if present.
        let body = node.child_by_field_name("body");
        if let Some(body) = body {
            let count = body.child_count();
            for i in 0..count {
                if let Some(child) = body.child(i) {
                    self.walk(child, Some(idx), &new_stack, None);
                }
            }
        }
    }

    fn walk_type(
        &mut self,
        node: Node,
        subtype: TypeKind,
        parent_idx: Option<usize>,
        mod_stack: &[String],
        impl_type: Option<&str>,
    ) {
        let name = self.field_text(node, "name").unwrap_or_default();
        let span = ts_span(node);
        if subtype != TypeKind::TypeAlias
            && let Some(name) = node.child_by_field_name("name")
        {
            self.type_declarations
                .push((self.defs.len(), name.start_byte()));
        }
        self.defs.push(RawDef {
            kind: NodeKind::Type(subtype),
            name,
            scope: scope_for(mod_stack, impl_type),
            span,
            parent: parent_idx,
        });
        // Types can contain nested items (associated types, nested mods,
        // inner functions). Recurse into the body with the type as a
        // non-module lexical parent (parent stays the enclosing module).
        let body = Self::body_node(node);
        if let Some(body) = body {
            let count = body.child_count();
            for i in 0..count {
                if let Some(child) = body.child(i) {
                    self.walk(child, parent_idx, mod_stack, impl_type);
                }
            }
        }
    }

    fn walk_trait(&mut self, node: Node, parent_idx: Option<usize>, mod_stack: &[String]) {
        let name = self.field_text(node, "name").unwrap_or_default();
        let span = ts_span(node);
        if let Some(name) = node.child_by_field_name("name") {
            self.type_declarations
                .push((self.defs.len(), name.start_byte()));
        }
        self.defs.push(RawDef {
            kind: NodeKind::Trait,
            name,
            scope: mod_stack.to_vec(),
            span,
            parent: parent_idx,
        });
        let body = node.child_by_field_name("body");
        if let Some(body) = body {
            let count = body.child_count();
            for i in 0..count {
                if let Some(child) = body.child(i) {
                    self.walk(child, parent_idx, mod_stack, None);
                }
            }
        }
    }

    fn walk_function(
        &mut self,
        node: Node,
        parent_idx: Option<usize>,
        mod_stack: &[String],
        impl_type: Option<&str>,
    ) {
        let name = self.field_text(node, "name").unwrap_or_default();
        let span = ts_span(node);
        let is_method = impl_type.is_some();
        if let Some(name) = node.child_by_field_name("name") {
            self.callable_declarations
                .push((self.defs.len(), name.start_byte()));
        }
        self.callables
            .push((self.defs.len(), cyclomatic(node), form(node)));
        self.defs.push(RawDef {
            kind: if is_method {
                NodeKind::Method
            } else {
                NodeKind::Function
            },
            name,
            scope: scope_for(mod_stack, impl_type),
            span,
            parent: parent_idx,
        });
        // Functions can contain nested items (inner functions, mods).
        let body = node.child_by_field_name("body");
        if let Some(body) = body {
            let count = body.child_count();
            for i in 0..count {
                if let Some(child) = body.child(i) {
                    // Inner items are lexically parented to the enclosing
                    // module/file, not the function, to keep containment
                    // at the module granularity. The function is not pushed
                    // onto the module stack.
                    self.walk(child, parent_idx, mod_stack, None);
                }
            }
        }
    }

    fn walk_impl(&mut self, node: Node, parent_idx: Option<usize>, mod_stack: &[String]) {
        let trait_name = node.child_by_field_name("trait").and_then(|n| self.text(n));
        let type_name = self.field_text(node, "type").unwrap_or_default();
        self.impls.push(RawImpl {
            trait_name,
            type_name: type_name.clone(),
            scope: mod_stack.to_vec(),
            trait_name_start: node
                .child_by_field_name("trait")
                .and_then(implementation_name_start),
            type_name_start: node
                .child_by_field_name("type")
                .and_then(implementation_name_start),
            generic: node.child_by_field_name("type_parameters").is_some(),
            negative: (0..node.child_count())
                .any(|index| node.child(index).is_some_and(|child| child.kind() == "!")),
            imported_binding: false,
            visible_glob: false,
            local_scope: false,
            span: ts_span(node),
        });

        let body = node.child_by_field_name("body");
        if let Some(body) = body {
            let count = body.child_count();
            for i in 0..count {
                if let Some(child) = body.child(i) {
                    self.walk(child, parent_idx, mod_stack, Some(&type_name));
                }
            }
        }
    }

    fn walk_use(&mut self, node: Node, scope: &[String]) {
        if let Some(arg) = node.child_by_field_name("argument") {
            let path = self.text(arg).unwrap_or_default();
            self.scoped_uses.push((scope.to_vec(), path.clone()));
            self.imports.push(RawImport {
                path: collapse_whitespace(&path),
                span: ts_span(arg),
            });
        }
    }

    fn walk_macro(&mut self, node: Node, parent_idx: Option<usize>, mod_stack: &[String]) {
        let name = self.field_text(node, "name").unwrap_or_default();
        let span = ts_span(node);
        // A macro body is token trees: no expression nodes, so the count is
        // the base 1. Measured all the same so every callable has a record.
        self.callables
            .push((self.defs.len(), cyclomatic(node), form(node)));
        self.defs.push(RawDef {
            kind: NodeKind::Macro,
            name,
            scope: mod_stack.to_vec(),
            span,
            parent: parent_idx,
        });
    }

    fn walk_call(&mut self, node: Node) {
        if let Some((raw, name_start, path_root)) = self.call_name_with_pos(node) {
            self.calls.push(RawCall {
                raw_name: raw,
                path_root,
                span: ts_span(node),
                name_start_byte: name_start,
                receiver: ReceiverShape::Other,
                scope: Vec::new(),
            });
        }
    }

    fn walk_macro_call(&mut self, node: Node) {
        if let Some(macro_node) = node.child_by_field_name("macro") {
            if let Some(macro_name) = self.text(macro_node) {
                self.calls.push(RawCall {
                    raw_name: strip_bang(&macro_name),
                    path_root: None,
                    span: ts_span(node),
                    name_start_byte: macro_node.start_byte(),
                    receiver: ReceiverShape::Other,
                    scope: Vec::new(),
                });
            }
        }
        // The grammar does not parse macro arguments, so the calls inside
        // `format!("{}", plural(n))` are tokens, not call expressions.
        for i in 0..node.child_count() {
            if let Some(tree) = node.child(i).filter(|child| child.kind() == "token_tree") {
                self.walk_token_tree(tree);
            }
        }
    }

    /// Observe the call-shaped token runs inside a macro's argument tokens:
    /// an identifier followed by a parenthesised group is a call, and an
    /// identifier followed by `!` and a group is a nested macro. Nested
    /// groups are walked too, so `outer(inner(x))` yields both.
    fn walk_token_tree(&mut self, tree: Node) {
        let tokens: Vec<Node> = (0..tree.child_count())
            .filter_map(|i| tree.child(i))
            .collect();
        for (i, token) in tokens.iter().enumerate() {
            if token.kind() == "token_tree" {
                self.walk_token_tree(*token);
                continue;
            }
            if token.kind() != "identifier" {
                continue;
            }
            let Some(next) = tokens.get(i + 1) else {
                continue;
            };
            let (group, is_macro) = match next.kind() {
                "token_tree" => (*next, false),
                "!" => match tokens.get(i + 2) {
                    Some(group) if group.kind() == "token_tree" => (*group, true),
                    _ => continue,
                },
                _ => continue,
            };
            if !is_macro && self.source.get(group.start_byte()) != Some(&b'(') {
                continue;
            }
            let Some(raw_name) = self.text(*token) else {
                continue;
            };
            let path_root = (!is_macro)
                .then(|| Self::token_path_root(&tokens, i))
                .flatten()
                .and_then(|root| self.text(root));
            self.calls.push(RawCall {
                raw_name,
                path_root,
                span: Span::from_ts(
                    token.start_byte(),
                    group.end_byte(),
                    (token.start_position().row, token.start_position().column),
                    (group.end_position().row, group.end_position().column),
                ),
                name_start_byte: token.start_byte(),
                receiver: ReceiverShape::Other,
                scope: Vec::new(),
            });
        }
    }

    /// The first segment of a `root::path::name` token run ending at
    /// `index`, when the name is path-qualified.
    fn token_path_root<'t>(tokens: &[Node<'t>], index: usize) -> Option<Node<'t>> {
        let mut root = None;
        let mut cursor = index;
        while cursor >= 2 && tokens[cursor - 1].kind() == "::" {
            let segment = tokens[cursor - 2];
            if !matches!(segment.kind(), "identifier" | "crate" | "self" | "super") {
                break;
            }
            root = Some(segment);
            cursor -= 2;
        }
        root
    }

    // ── helpers ──

    fn field_text(&self, node: Node, field: &str) -> Option<String> {
        node.child_by_field_name(field).and_then(|n| self.text(n))
    }

    fn text(&self, node: Node) -> Option<String> {
        node.utf8_text(self.source)
            .ok()
            .map(std::string::ToString::to_string)
    }

    fn body_node(node: Node) -> Option<Node> {
        // struct/enum/union bodies are `declaration_list` or `field_declaration_list`;
        // type aliases have no body.
        node.child_by_field_name("body")
            .or_else(|| node.child_by_field_name("fields"))
    }

    /// Return the called identifier text plus its start byte, for LSP
    /// `textDocument/definition` positioning.
    fn call_name_with_pos(&self, node: Node) -> Option<(String, usize, Option<String>)> {
        let func = node.child_by_field_name("function")?;
        let path_root = (func.kind() == "scoped_identifier")
            .then(|| self.text(func))
            .flatten()
            .and_then(|path| path.split("::").next().map(str::to_string));
        let name_node = match func.kind() {
            "field_expression" => func.child_by_field_name("field")?,
            "scoped_identifier" => func.child_by_field_name("name")?,
            _ => func,
        };
        self.text(name_node)
            .map(|s| (s, name_node.start_byte(), path_root))
    }
}

fn scope_for(mod_stack: &[String], impl_type: Option<&str>) -> Vec<String> {
    let mut s = mod_stack.to_vec();
    if let Some(t) = impl_type {
        s.push(t.to_string());
    }
    s
}

/// Only a spelled, nongeneric path names one concrete declaration here.
fn implementation_name_start(node: Node<'_>) -> Option<usize> {
    match node.kind() {
        "type_identifier" => Some(node.start_byte()),
        "scoped_type_identifier" => node
            .child_by_field_name("name")
            .and_then(implementation_name_start),
        _ => None,
    }
}

// ───────────────────────── measures ─────────────────────────

/// Cyclomatic complexity of one callable: 1 plus its decision points.
///
/// Decision points (policy `cc-sloc/rust/1`): `if` (including `if let`
/// and `else if`), `while` (including `while let`), `loop`, `for`; each
/// `match` arm beyond the first, plus one more per arm guard; each `&&`
/// and `||` operator in an expression; each `?`. Closures are part of the
/// callable that encloses them, so their branches count here. Nested
/// named definitions (`fn`, `impl`, `mod`, `trait`, `macro_rules!`) are
/// callables or containers of their own and are skipped. Macro
/// invocations carry token trees, not expressions, so nothing inside
/// `assert!(a || b)` counts: the grammar never parsed it as a branch.
pub fn cyclomatic(callable: Node) -> u32 {
    1 + decision_points(callable)
}

fn decision_points(node: Node) -> u32 {
    let mut count = 0;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if is_named_definition(child.kind()) {
            continue;
        }
        count += match child.kind() {
            "if_expression" | "while_expression" | "loop_expression" | "for_expression"
            | "try_expression" => 1,
            "binary_expression" | "let_chain" => {
                let mut operators = child.walk();
                u32::try_from(
                    child
                        .children(&mut operators)
                        .filter(|token| matches!(token.kind(), "&&" | "||"))
                        .count(),
                )
                .unwrap_or(u32::MAX)
            }
            "match_block" => match_block_points(child),
            _ => 0,
        };
        count += decision_points(child);
    }
    count
}

/// Arms beyond the first, plus one per guarded arm.
fn match_block_points(block: Node) -> u32 {
    let mut cursor = block.walk();
    let arms: Vec<Node> = block
        .children(&mut cursor)
        .filter(|child| child.kind() == "match_arm")
        .collect();
    let guards = arms.iter().filter(|arm| has_guard(arm)).count();
    u32::try_from(arms.len().saturating_sub(1) + guards).unwrap_or(u32::MAX)
}

/// tree-sitter-rust parses `pattern if guard` as a `match_pattern` whose
/// children include the `if` keyword.
fn has_guard(arm: &Node) -> bool {
    let mut cursor = arm.walk();
    arm.children(&mut cursor).any(|child| {
        child.kind() == "match_pattern" && {
            let mut inner = child.walk();
            child.children(&mut inner).any(|token| token.kind() == "if")
        }
    })
}

/// The form measures of one callable under `cc-sloc-clones-form/rust/1`,
/// over the same body [`cyclomatic`] reads: nested named definitions are
/// left to their own measures, closures belong to the function that
/// encloses them, and macro invocations are token trees in which nothing
/// counts. Rust has no instance variables, so `ivars` is absent.
pub fn form(callable: Node) -> Form {
    Form {
        params: params(callable),
        nesting: nesting(callable),
        chain: chain(callable),
        ivars: None,
    }
}

/// Parameters the definition declares: each `parameter` and
/// `variadic_parameter` in the `parameters` field. A `self_parameter` and
/// a `parameter` whose pattern is `self` (a typed receiver) are the
/// receiver, not parameters; a `macro_rules!` definition has none.
fn params(callable: Node) -> u32 {
    let Some(parameters) = callable.child_by_field_name("parameters") else {
        return 0;
    };
    let mut cursor = parameters.walk();
    let count = parameters
        .named_children(&mut cursor)
        .filter(|child| match child.kind() {
            "variadic_parameter" => true,
            "parameter" => child
                .child_by_field_name("pattern")
                .is_none_or(|pattern| pattern.kind() != "self"),
            _ => false,
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The deepest stack of control structures any node of the body sits
/// under: `if` (an `if` that is the direct child of an `else_clause` stays
/// at its parent's depth, so an `else if` chain is one level; the else
/// block sits one level in, like the consequence), `while`, `loop`,
/// `for`, `match` (arms add nothing), and closure bodies. Plain,
/// `unsafe`, and `async` blocks add nothing. A straight body is 0.
fn nesting(callable: Node) -> u32 {
    depth_below(callable, 0)
}

fn depth_below(node: Node, depth: u32) -> u32 {
    let mut deepest = depth;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if is_named_definition(child.kind()) {
            continue;
        }
        let level = match child.kind() {
            "if_expression" => u32::from(node.kind() != "else_clause"),
            "while_expression" | "loop_expression" | "for_expression" | "match_expression"
            | "closure_expression" => 1,
            _ => 0,
        };
        deepest = deepest.max(depth_below(child, depth + level));
    }
    deepest
}

/// The longest dot spine of any expression in the body ([`spine`]); a
/// body without a field access or method call is 0.
fn chain(callable: Node) -> u32 {
    let mut longest = 0;
    let mut cursor = callable.walk();
    for child in callable.children(&mut cursor) {
        if is_named_definition(child.kind()) {
            continue;
        }
        if child.kind() == "field_expression" {
            longest = longest.max(spine(child));
        }
        longest = longest.max(chain(child));
    }
    longest
}

/// Dots along one expression's receiver spine: each `field_expression` is
/// one hop (`.field`, or the `.method` of a call whose function is a field
/// expression); a call, a turbofish (`generic_function`), `?`, `.await`,
/// an index, and parentheses pass the spine through without counting; a
/// `::` path, a bare call, or an operator ends it at 0. `self.a.b.len()`
/// is 3, `values.iter().map(f).collect()` is 3.
fn spine(node: Node) -> u32 {
    match node.kind() {
        "field_expression" => 1 + node.child_by_field_name("value").map_or(0, spine),
        "call_expression" | "generic_function" => {
            node.child_by_field_name("function").map_or(0, spine)
        }
        "try_expression" | "await_expression" | "index_expression" | "parenthesized_expression" => {
            node.named_child(0).map_or(0, spine)
        }
        _ => 0,
    }
}

const fn is_named_definition(kind: &str) -> bool {
    matches!(
        kind.as_bytes(),
        b"function_item"
            | b"impl_item"
            | b"mod_item"
            | b"trait_item"
            | b"macro_definition"
            | b"struct_item"
            | b"enum_item"
            | b"union_item"
            | b"type_item"
    )
}

/// Count the error and missing nodes of the whole tree and find the first
/// in source order. The walk covers every node, not only the ones
/// extraction visits, so a broken signature or struct body still counts.
fn syntax_errors(root: Node) -> (usize, Option<Span>) {
    let mut count = 0;
    let mut first = None;
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            count += 1;
            first.get_or_insert_with(|| ts_span(node));
        }
        if node.has_error() && cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return (count, first);
            }
        }
    }
}

fn ts_span(node: Node) -> Span {
    Span::from_ts(
        node.start_byte(),
        node.end_byte(),
        (node.start_position().row, node.start_position().column),
        (node.end_position().row, node.end_position().column),
    )
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_bang(s: &str) -> String {
    s.trim_end_matches('!').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(src: &str) -> RawFile {
        extract_file("src/test.rs", src.as_bytes()).unwrap()
    }

    #[test]
    fn extracts_struct_function_and_method() {
        let f = extract(
            r"
            struct Repository { path: String }
            impl Repository {
                fn commit_pending(&self) {}
            }
            fn free_fn() {}
            ",
        );
        let kinds: Vec<_> = f.defs.iter().map(|d| (d.kind, d.name.as_str())).collect();
        assert!(kinds.contains(&(NodeKind::Type(TypeKind::Struct), "Repository")));
        assert!(kinds.contains(&(NodeKind::Method, "commit_pending")));
        assert!(kinds.contains(&(NodeKind::Function, "free_fn")));

        let method = f.defs.iter().find(|d| d.name == "commit_pending").unwrap();
        assert_eq!(method.scope, vec!["Repository".to_string()]);
        assert_eq!(method.kind, NodeKind::Method);

        let imp = &f.impls[0];
        assert_eq!(imp.type_name, "Repository");
        assert!(imp.trait_name.is_none());
    }

    #[test]
    fn extracts_trait_impl() {
        let f = extract("impl Display for Foo {}");
        let imp = &f.impls[0];
        assert_eq!(imp.trait_name.as_deref(), Some("Display"));
        assert_eq!(imp.type_name, "Foo");
    }

    #[test]
    fn extracts_module_nesting_and_containment() {
        let f = extract("mod outer { mod inner { struct S; } }");
        let outer = f.defs.iter().find(|d| d.name == "outer").unwrap();
        let inner = f.defs.iter().find(|d| d.name == "inner").unwrap();
        let s = f.defs.iter().find(|d| d.name == "S").unwrap();
        assert!(outer.parent.is_none());
        assert_eq!(inner.parent, Some(0)); // outer is defs[0]
        assert_eq!(s.scope, vec!["outer".to_string(), "inner".to_string()]);
    }

    #[test]
    fn extracts_use_imports_verbatim() {
        let f = extract("use std::collections::HashMap;");
        assert_eq!(f.imports.len(), 1);
        assert_eq!(f.imports[0].path, "std::collections::HashMap");
    }

    #[test]
    fn collects_call_sites_as_observations() {
        let f = extract("fn main() { foo(); bar.baz(); println!(\"x\"); }");
        let names: Vec<_> = f.calls.iter().map(|c| c.raw_name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"baz"));
        assert!(names.contains(&"println"));
    }

    #[test]
    fn calls_inside_macro_arguments_are_observed_at_their_identifier() {
        let source = "fn main() { println!(\"{} {}\", plural(n), item.len()); assert_eq!(util::count(items::all(3)), vec![g(1)].len()); write!(f, \"{}\", inner!(h(2))); }";
        let f = extract(source);
        let call = |name: &str| {
            f.calls
                .iter()
                .find(|call| call.raw_name == name)
                .unwrap_or_else(|| panic!("no call {name} in {:?}", f.calls))
        };
        for name in ["plural", "len", "count", "all", "g", "h", "inner"] {
            let observed = call(name);
            assert_eq!(
                &source[observed.name_start_byte..observed.name_start_byte + name.len()],
                name,
                "{name} must be positioned at its identifier"
            );
        }
        assert_eq!(call("count").path_root.as_deref(), Some("util"));
        assert_eq!(call("all").path_root.as_deref(), Some("items"));
        assert_eq!(call("plural").path_root, None);
        // A bare identifier in the argument list is a value, not a call.
        assert!(
            f.calls
                .iter()
                .all(|call| call.raw_name != "n" && call.raw_name != "f")
        );
        assert_eq!(
            f.calls.iter().filter(|call| call.raw_name == "len").count(),
            2,
            "both len calls are distinct observations"
        );
    }

    #[test]
    fn scoped_call_targets_the_called_name() {
        let source = "fn main() { module::function(); }";
        let f = extract(source);
        let call = f
            .calls
            .iter()
            .find(|call| call.raw_name == "function")
            .unwrap();
        assert_eq!(
            &source[call.name_start_byte..call.name_start_byte + "function".len()],
            "function"
        );
        assert_eq!(call.path_root.as_deref(), Some("module"));
    }

    #[test]
    fn reports_parse_errors_on_malformed_input() {
        let f = extract("struct Missing { ");
        assert!(
            f.parse_errors > 0,
            "expected parse errors, got {}",
            f.parse_errors
        );
    }

    /// A missing `)` in a signature and a missing `}` in a struct body both
    /// sit outside the bodies extraction walks, and both count.
    #[test]
    fn parse_errors_count_the_whole_tree() {
        let f = extract("fn ok() {}\nfn broken( {}\nstruct Missing {\n");
        assert_eq!(f.parse_errors, 2);
    }

    #[test]
    fn the_first_parse_error_is_the_earliest_error_node() {
        let clean = extract("fn ok() {}\n");
        assert_eq!((clean.parse_errors, clean.first_parse_error), (0, None));

        let f = extract("fn ok() {}\nfn broken( {}\nstruct Missing {\n");
        let first = f.first_parse_error.expect("first parse error");
        assert_eq!((first.start_line, first.start_col), (2, 10));
    }

    /// rustc accepts this with or without the feature, but the pinned
    /// tree-sitter-rust 0.24.2 grammar has no attributes in `field_pattern`,
    /// so the struct pattern becomes an error node. A grammar that accepts
    /// it fails this test and should be re-pinned.
    #[test]
    fn an_attribute_on_a_struct_pattern_field_is_a_known_grammar_gap() {
        let f = extract(
            "pub enum Shape {\n    Square {\n        side: u32,\n        #[cfg(feature = \"labels\")]\n        label: String,\n    },\n}\n\npub fn area(shape: Shape) -> u32 {\n    match shape {\n        Shape::Square {\n            side,\n            #[cfg(feature = \"labels\")]\n            label,\n        } => side * side,\n    }\n}\n",
        );
        assert!(f.parse_errors > 0);
        let first = f.first_parse_error.expect("first parse error");
        assert_eq!((first.start_line, first.start_col), (11, 8));
    }

    #[test]
    fn vendored_query_compiles() {
        // Panics inside the lock init if the query is invalid.
        let _ = rust_query();
    }
}
