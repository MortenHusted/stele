//! Tree-sitter Go syntax extraction (G1: syntax-only).
//!
//! Owns parsing and language-specific extraction only. It never guesses
//! call targets, runs an LSP, or resolves cross-file references — that is
//! [`crate::resolve`]. This module produces raw, source-addressed
//! observations; [`crate::extract`] assigns deterministic IDs and
//! [`crate::resolve`] upgrades resolvable observations into edges.
//!
//! What is emitted directly from syntax (no resolution needed):
//! - **definition nodes** — the `package` clause → Module (one per file,
//!   named by the package); `type X struct` → [`TypeKind::Struct`];
//!   `type X interface` → [`TypeKind::Interface`], with each method it
//!   declares → Method scoped under it; `type X = Y` →
//!   [`TypeKind::TypeAlias`]; any other `type X T` → [`TypeKind::Defined`]
//!   (a defined type with its own method set); `func f()` → Function;
//!   `func (r T) m()` → Method scoped under the receiver's base type `T`.
//!   Every definition is qualified by its package (`pkg::Type::method`),
//!   which is how Go names it.
//! - **lexical `contains`** edges (file → each top-level definition;
//!   interface → its methods). Methods declared outside a type body are
//!   file children, as in Go, and reach their type through their scope.
//! - **`imports`** edges (file → `ImportRef` placeholder) preserving the
//!   import path verbatim, with an alias, dot, or blank name recorded as
//!   `path as name`; never pretending they resolve in-repository.
//!
//! What is collected as observations for later resolution:
//! - **embedded fields** in a struct body → [`RawEmbed`] with
//!   [`EdgeKind::Inherits`] (the struct takes the embedded type's methods);
//!   **embedded interfaces** in an interface body → [`RawEmbed`] with
//!   [`EdgeKind::Implements`]; either is upgraded to an edge only when both
//!   sides resolve to unique in-repository definitions;
//! - **call expressions** → [`RawCall`] observations named by the callee
//!   as written. Syntax-only mode never emits canonical `calls` edges.
//!
//! Honest gaps (never guessed): interface satisfaction is implicit in Go
//! and is never an edge; method values (`f := x.M`), function-typed fields
//! and variables, and calls through func literals surface as call
//! observations only; a conversion `T(x)` parses as a call and is recorded
//! as one, to be classified by the semantic phase; type parameters, struct
//! fields, constants, and variables are not definitions.

use std::sync::LazyLock;

use anyhow::{Context, Result};
use tree_sitter::{Language, Node, Parser};

use crate::clones::Token;
use crate::measures::{
    Form, LineClass, LineCounts, RawMeasure, TokenRules, arms_beyond_first, comment_ranges,
    count_lines, line_classes, operator_count, token_stream,
};
use crate::rust_syntax::{RawCall, RawDef, RawImport, ReceiverShape};
use crate::schema::{EdgeKind, NodeKind, Span, TypeKind};

static GO_LANGUAGE: LazyLock<Language> = LazyLock::new(|| tree_sitter_go::LANGUAGE.into());

/// A type reference as written at an embedding site: `Base`, `pkg.Base`,
/// `*Base`, `List[T]` (the pointer and the type arguments are stripped;
/// they never change which declaration is meant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeReference {
    /// The package selector, when written (`pkg` in `pkg.Base`); the
    /// file's import table maps it to an import path.
    pub package: Option<String>,
    /// The type name.
    pub name: String,
}

/// A raw embedding observation: a struct field without a name, or a type
/// element of an interface body.
#[derive(Debug, Clone)]
pub struct RawEmbed {
    /// `Inherits` for a struct embedding, `Implements` for an interface
    /// embedding.
    pub kind: EdgeKind,
    pub target: TypeReference,
    /// Name of the declaring type, unqualified; it lives in this file's
    /// package.
    pub declarant: String,
    pub span: Span,
}

/// Per-file Go extraction output.
#[derive(Debug, Clone)]
pub struct RawFile {
    /// `(def index, declaration name start byte)` for functions and methods.
    pub callable_declarations: Vec<(usize, usize)>,
    pub defs: Vec<RawDef>,
    pub imports: Vec<RawImport>,
    pub calls: Vec<RawCall>,
    pub embeds: Vec<RawEmbed>,
    /// Local package names in force and the import paths they stand for
    /// (`svc` → `example.invalid/app/internal/svc`): an explicit alias, or
    /// the last path segment. Dot and blank imports bind no name.
    pub package_imports: Vec<(String, String)>,
    pub parse_errors: usize,
    /// One entry per callable def (function, method, interface method).
    pub measures: Vec<RawMeasure>,
    /// Line classes of the whole file.
    pub file_lines: LineCounts,
    /// Every line's class, for clone occurrence counts.
    pub line_classes: Vec<LineClass>,
    /// The normalised token stream clone detection reads.
    pub tokens: Vec<Token>,
}

/// How Go leaves become the normalised token stream: comments dropped,
/// identifiers of every flavour `$id`, string, raw string, rune, and
/// number literals `$lit`; keywords, `true`, `false`, `nil`, `_`, and
/// punctuation stay as written.
pub const GO_TOKENS: TokenRules = TokenRules {
    comments: &["comment"],
    identifiers: &[
        "identifier",
        "field_identifier",
        "type_identifier",
        "package_identifier",
        "label_name",
    ],
    literals: &[
        "interpreted_string_literal",
        "raw_string_literal",
        "rune_literal",
        "int_literal",
        "float_literal",
        "imaginary_literal",
    ],
};

/// Extract structural observations from a single Go source file.
pub fn extract_file(rel_path: &str, source: &[u8]) -> Result<RawFile> {
    let _ = rel_path; // matches the other adapters' signature
    let mut parser = Parser::new();
    parser
        .set_language(&GO_LANGUAGE)
        .context("set tree-sitter-go language")?;
    let tree = parser
        .parse(source, None)
        .context("tree-sitter parse returned None")?;

    let root = tree.root_node();
    let mut walker = Walker::new(source);
    let mut ctx = Ctx::default();
    walker.visit_children(root, &mut ctx);

    let classes = line_classes(source, &comment_ranges(root, GO_TOKENS.comments));
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
    let tokens = token_stream(root, source, &GO_TOKENS);

    Ok(RawFile {
        callable_declarations: walker.callable_declarations,
        defs: walker.defs,
        imports: walker.imports,
        calls: walker.calls,
        embeds: walker.embeds,
        package_imports: walker.package_imports,
        parse_errors: walker.errors,
        measures,
        file_lines,
        line_classes: classes,
        tokens,
    })
}

/// Mutable traversal context: the package every definition is qualified
/// by, and the lexical parent def.
#[derive(Default)]
struct Ctx {
    /// The package clause's name; `None` before the clause (or without one).
    package: Option<String>,
    /// Index into `defs` of the lexical parent node (`None` = file).
    parent: Option<usize>,
}

struct Walker<'a> {
    callable_declarations: Vec<(usize, usize)>,
    source: &'a [u8],
    defs: Vec<RawDef>,
    imports: Vec<RawImport>,
    calls: Vec<RawCall>,
    embeds: Vec<RawEmbed>,
    package_imports: Vec<(String, String)>,
    /// `(def index, cyclomatic complexity)` per callable, in def order.
    callables: Vec<(usize, u32, Form)>,
    errors: usize,
}

fn text(source: &[u8], node: Node) -> String {
    node.utf8_text(source).unwrap_or_default().to_string()
}

fn span(node: Node) -> Span {
    Span::from_ts(
        node.start_byte(),
        node.end_byte(),
        (node.start_position().row, node.start_position().column),
        (node.end_position().row, node.end_position().column),
    )
}

/// Strip pointer and type-argument wrappers from a written type and read
/// the package selector and name, when the type is a named type at all.
fn type_reference(source: &[u8], node: Node) -> Option<TypeReference> {
    match node.kind() {
        "type_identifier" => Some(TypeReference {
            package: None,
            name: text(source, node),
        }),
        "qualified_type" => Some(TypeReference {
            package: node
                .child_by_field_name("package")
                .map(|package| text(source, package)),
            name: node
                .child_by_field_name("name")
                .map(|name| text(source, name))?,
        }),
        "pointer_type" | "parenthesized_type" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find_map(|child| type_reference(source, child))
        }
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|inner| type_reference(source, inner)),
        _ => None,
    }
}

impl<'a> Walker<'a> {
    const fn new(source: &'a [u8]) -> Self {
        Self {
            source,
            callable_declarations: Vec::new(),
            defs: Vec::new(),
            imports: Vec::new(),
            calls: Vec::new(),
            embeds: Vec::new(),
            package_imports: Vec::new(),
            callables: Vec::new(),
            errors: 0,
        }
    }

    fn text(&self, node: Node) -> String {
        text(self.source, node)
    }

    /// Push a definition qualified by the package (and, for methods, the
    /// receiver type) with the lexical parent from `ctx`.
    fn push_def(
        &mut self,
        kind: NodeKind,
        name: String,
        scope_tail: &[String],
        span_of: Node,
        ctx: &Ctx,
    ) -> usize {
        let mut scope: Vec<String> = ctx.package.iter().cloned().collect();
        scope.extend_from_slice(scope_tail);
        if matches!(kind, NodeKind::Function | NodeKind::Method)
            && let Some(name) = span_of.child_by_field_name("name")
        {
            self.callable_declarations
                .push((self.defs.len(), name.start_byte()));
        }
        self.defs.push(RawDef {
            kind,
            name,
            scope,
            span: span(span_of),
            parent: ctx.parent,
        });
        self.defs.len() - 1
    }

    fn visit_children(&mut self, node: Node, ctx: &mut Ctx) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.visit(child, ctx);
        }
    }

    fn visit(&mut self, node: Node, ctx: &mut Ctx) {
        if node.kind() == "ERROR" {
            self.errors += 1;
        }
        match node.kind() {
            "package_clause" => {
                let mut cursor = node.walk();
                if let Some(name) = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "package_identifier")
                {
                    let package = self.text(name);
                    self.push_def(NodeKind::Module, package.clone(), &[], node, ctx);
                    ctx.package = Some(package);
                }
            }
            "import_declaration" => self.visit_import_declaration(node),
            "function_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = self.text(name_node);
                    let idx = self.push_def(NodeKind::Function, name, &[], node, ctx);
                    self.callables.push((idx, cyclomatic(node), form(node)));
                    let saved_parent = ctx.parent.replace(idx);
                    self.visit_children(node, ctx);
                    ctx.parent = saved_parent;
                } else {
                    self.visit_children(node, ctx);
                }
            }
            "method_declaration" => {
                let receiver = node
                    .child_by_field_name("receiver")
                    .and_then(|list| receiver_base_type(self.source, list));
                match (node.child_by_field_name("name"), receiver) {
                    (Some(name_node), Some(receiver)) => {
                        let name = self.text(name_node);
                        let idx = self.push_def(NodeKind::Method, name, &[receiver], node, ctx);
                        self.callables.push((idx, cyclomatic(node), form(node)));
                        let saved_parent = ctx.parent.replace(idx);
                        self.visit_children(node, ctx);
                        ctx.parent = saved_parent;
                    }
                    _ => self.visit_children(node, ctx),
                }
            }
            "type_declaration" => {
                let mut cursor = node.walk();
                for spec in node.named_children(&mut cursor) {
                    match spec.kind() {
                        "type_spec" => self.visit_type_spec(spec, ctx),
                        "type_alias" => {
                            if let Some(name_node) = spec.child_by_field_name("name") {
                                let name = self.text(name_node);
                                self.push_def(
                                    NodeKind::Type(TypeKind::TypeAlias),
                                    name,
                                    &[],
                                    spec,
                                    ctx,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            "call_expression" => {
                if let Some(callee) = node.child_by_field_name("function") {
                    self.record_call(node, callee);
                }
                self.visit_children(node, ctx);
            }
            "type_conversion_expression" => {
                // `Map[int](x)` and `pkg.T(x)` parse as conversions to a
                // named type, yet may call a generic function or a
                // function-typed name only the type checker can tell
                // apart. Record them as call observations named by the
                // written type; `[]byte(x)` or `(*T)(x)` name no
                // declaration and are conversions by syntax alone.
                if let Some(ty) = node.child_by_field_name("type") {
                    if matches!(
                        ty.kind(),
                        "type_identifier" | "qualified_type" | "generic_type"
                    ) {
                        self.record_call(node, ty);
                    }
                }
                self.visit_children(node, ctx);
            }
            _ => self.visit_children(node, ctx),
        }
    }

    /// One `type_spec`: the named type, its kind by underlying type, and
    /// the embeddings and interface methods its body declares.
    fn visit_type_spec(&mut self, spec: Node, ctx: &mut Ctx) {
        let Some(name_node) = spec.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node);
        let underlying = spec.child_by_field_name("type");
        let kind = match underlying.map(|u| u.kind()) {
            Some("struct_type") => NodeKind::Type(TypeKind::Struct),
            Some("interface_type") => NodeKind::Type(TypeKind::Interface),
            _ => NodeKind::Type(TypeKind::Defined),
        };
        let idx = self.push_def(kind, name.clone(), &[], spec, ctx);
        let Some(underlying) = underlying else {
            return;
        };
        match underlying.kind() {
            "struct_type" => {
                let mut cursor = underlying.walk();
                for list in underlying.named_children(&mut cursor) {
                    if list.kind() != "field_declaration_list" {
                        continue;
                    }
                    let mut fields = list.walk();
                    for field in list.named_children(&mut fields) {
                        // An embedded field has no name field: its type is
                        // the field.
                        if field.kind() != "field_declaration"
                            || field.child_by_field_name("name").is_some()
                        {
                            continue;
                        }
                        if let Some(target) = field
                            .child_by_field_name("type")
                            .and_then(|t| type_reference(self.source, t))
                        {
                            self.embeds.push(RawEmbed {
                                kind: EdgeKind::Inherits,
                                target,
                                declarant: name.clone(),
                                span: span(field),
                            });
                        }
                    }
                }
            }
            "interface_type" => {
                let saved_parent = ctx.parent.replace(idx);
                let mut cursor = underlying.walk();
                for elem in underlying.named_children(&mut cursor) {
                    match elem.kind() {
                        "method_elem" => {
                            if let Some(method_name) = elem.child_by_field_name("name") {
                                let method = self.text(method_name);
                                let idx = self.push_def(
                                    NodeKind::Method,
                                    method,
                                    std::slice::from_ref(&name),
                                    elem,
                                    ctx,
                                );
                                // An interface method has no body: one
                                // path, no decisions, nesting and chain 0;
                                // its parameters still count.
                                self.callables.push((idx, 1, form(elem)));
                            }
                        }
                        "type_elem" => {
                            // A single named type is an embedded interface;
                            // a union or an approximation (`~int | string`)
                            // is a constraint and names no method set.
                            let mut elems = elem.walk();
                            let named: Vec<Node> = elem.named_children(&mut elems).collect();
                            if let [only] = named.as_slice() {
                                if let Some(target) = type_reference(self.source, *only) {
                                    self.embeds.push(RawEmbed {
                                        kind: EdgeKind::Implements,
                                        target,
                                        declarant: name.clone(),
                                        span: span(elem),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
                ctx.parent = saved_parent;
            }
            _ => {}
        }
    }

    /// Every `import_spec` under the declaration, in source order (a
    /// grouped import nests them one level deeper).
    fn visit_import_declaration(&mut self, node: Node) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "import_spec" => self.record_import(child),
                "import_spec_list" => {
                    let mut specs = child.walk();
                    for spec in child.named_children(&mut specs) {
                        if spec.kind() == "import_spec" {
                            self.record_import(spec);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn record_import(&mut self, spec: Node) {
        let Some(path_node) = spec.child_by_field_name("path") else {
            return;
        };
        let path = self
            .text(path_node)
            .trim_matches(|c| c == '"' || c == '`')
            .to_string();
        let name = spec.child_by_field_name("name").map(|n| self.text(n));
        let local = match name.as_deref() {
            Some("." | "_") => None,
            Some(alias) => Some(alias.to_string()),
            None => path.rsplit('/').next().map(str::to_string),
        };
        if let Some(local) = local {
            self.package_imports.push((local, path.clone()));
        }
        self.imports.push(RawImport {
            path: name.map_or_else(|| path.clone(), |n| format!("{path} as {n}")),
            span: span(spec),
        });
    }

    /// Record a call named by its callee as written. The name position is
    /// the identifier gopls resolves: the selector's field for `x.f()`, the
    /// operand's name for an explicit instantiation `f[T]()`, the type
    /// name for a conversion-shaped call, the callee itself otherwise. A
    /// callee that names nothing (a func literal, a parenthesized
    /// expression, a call result) is recorded by its first line, so the
    /// observation still reads like the source.
    fn record_call(&mut self, call: Node, callee: Node) {
        let name_node = callee_name(callee);
        let raw_name = match callee.kind() {
            "identifier"
            | "selector_expression"
            | "index_expression"
            | "type_identifier"
            | "qualified_type"
            | "generic_type" => self.text(callee),
            _ => first_line(&self.text(callee)),
        };
        self.calls.push(RawCall {
            raw_name,
            path_root: None,
            span: span(call),
            name_start_byte: name_node.start_byte(),
            receiver: ReceiverShape::Other,
            scope: Vec::new(),
        });
    }
}

/// The identifier a callee resolves at.
fn callee_name(callee: Node) -> Node {
    match callee.kind() {
        "selector_expression" => callee.child_by_field_name("field").unwrap_or(callee),
        "index_expression" => callee
            .child_by_field_name("operand")
            .map_or(callee, callee_name),
        "qualified_type" => callee.child_by_field_name("name").unwrap_or(callee),
        "generic_type" => callee
            .child_by_field_name("type")
            .map_or(callee, callee_name),
        _ => callee,
    }
}

/// The first line of a callee that names nothing, bounded so a long func
/// literal does not become the observation's name.
fn first_line(text: &str) -> String {
    const LIMIT: usize = 40;
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() <= LIMIT {
        line.to_string()
    } else {
        let head: String = line.chars().take(LIMIT).collect();
        format!("{head}…")
    }
}

/// Base type name of a method receiver: `T` for `(t T)`, `(t *T)`,
/// `(T)`, `(t *List[K])`.
fn receiver_base_type(source: &[u8], list: Node) -> Option<String> {
    let mut cursor = list.walk();
    let declaration = list
        .named_children(&mut cursor)
        .find(|child| child.kind() == "parameter_declaration")?;
    let ty = declaration.child_by_field_name("type")?;
    type_reference(source, ty).map(|reference| reference.name)
}

/// Nested named definitions have their own measures and are skipped by
/// every counter here.
const fn is_named_definition(kind: &str) -> bool {
    matches!(
        kind.as_bytes(),
        b"function_declaration" | b"method_declaration" | b"type_declaration"
    )
}

/// The form measures of one callable under `cc-sloc-clones-form/go/1`,
/// over the same body [`cyclomatic`] reads: nested named definitions are
/// left to their own measures, function literals belong to the function
/// that encloses them. Go has no instance variables, so `ivars` is
/// absent. An interface method element has parameters and no body.
pub fn form(callable: Node) -> Form {
    Form {
        params: params(callable),
        nesting: nesting(callable),
        chain: chain(callable),
        ivars: None,
    }
}

/// Parameters the definition declares under the `parameters` field: each
/// `parameter_declaration` counts its names (`a, b int` is two; an
/// unnamed type is one), each `variadic_parameter_declaration` one. The
/// `receiver` field is not a parameter.
fn params(callable: Node) -> u32 {
    let Some(list) = callable.child_by_field_name("parameters") else {
        return 0;
    };
    let mut cursor = list.walk();
    let mut count = 0usize;
    for child in list.named_children(&mut cursor) {
        match child.kind() {
            "parameter_declaration" => {
                let mut names = child.walk();
                count += child
                    .children_by_field_name("name", &mut names)
                    .count()
                    .max(1);
            }
            "variadic_parameter_declaration" => count += 1,
            _ => {}
        }
    }
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The deepest stack of control structures any node of the body sits
/// under: `if` (an `if` that is the `alternative` of an `if` stays at its
/// parent's depth, so an `else if` chain is one level), `for` in every
/// form, `switch`, type switch, and `select` (cases add nothing), and
/// function literal bodies. A straight body is 0.
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
            "if_statement" => u32::from(node.kind() != "if_statement"),
            "for_statement"
            | "expression_switch_statement"
            | "type_switch_statement"
            | "select_statement"
            | "func_literal" => 1,
            _ => 0,
        };
        deepest = deepest.max(depth_below(child, depth + level));
    }
    deepest
}

/// The longest selector spine of any expression in the body ([`spine`]);
/// a body without a selector is 0.
fn chain(callable: Node) -> u32 {
    let mut longest = 0;
    let mut cursor = callable.walk();
    for child in callable.children(&mut cursor) {
        if is_named_definition(child.kind()) {
            continue;
        }
        if child.kind() == "selector_expression" {
            longest = longest.max(spine(child));
        }
        longest = longest.max(chain(child));
    }
    longest
}

/// Hops along one expression's operand spine: each `selector_expression`
/// is one (`x.y`, a package selector included); a call, an index, a type
/// assertion, and parentheses pass the spine through; anything else ends
/// it at 0. `s.repo.Find(id).Items()` is 3, `m["k"].items` is 1,
/// `(s.repo).items` is 2.
fn spine(node: Node) -> u32 {
    match node.kind() {
        "selector_expression" => 1 + node.child_by_field_name("operand").map_or(0, spine),
        "call_expression" => node.child_by_field_name("function").map_or(0, spine),
        "index_expression" | "type_assertion_expression" => {
            node.child_by_field_name("operand").map_or(0, spine)
        }
        "parenthesized_expression" => node.named_child(0).map_or(0, spine),
        _ => 0,
    }
}

/// Cyclomatic complexity of one callable: 1 plus its decision points.
///
/// Decision points (policy `cc-sloc-clones-form/go/1`, plan section 4): `if`
/// (an `else if` is an `if` inside the `else`), `for` (every form), each
/// `case` of a `switch`, type switch, or `select` beyond the first (a
/// `default` is not a decision), and each `&&` and `||` operator. Function
/// literals are part of the callable that encloses them, so their
/// branches count here; nested named declarations are skipped. An
/// interface method element has no body and measures 1.
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
            "if_statement" | "for_statement" => 1,
            "expression_switch_statement" | "type_switch_statement" | "select_statement" => {
                arms_beyond_first(
                    child,
                    &["expression_case", "type_case", "communication_case"],
                )
            }
            "binary_expression" => operator_count(child, &["&&", "||"]),
            _ => 0,
        };
        count += decision_points(child);
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> RawFile {
        extract_file("f.go", source.as_bytes()).expect("go source parses")
    }

    fn qualified(file: &RawFile) -> Vec<String> {
        file.defs
            .iter()
            .map(|d| {
                let mut parts = d.scope.clone();
                parts.push(d.name.clone());
                format!("{}:{}", d.kind, parts.join("::"))
            })
            .collect()
    }

    #[test]
    fn definitions_are_qualified_by_package_and_receiver() {
        let source = "package svc\n\ntype Runner interface {\n\tRun() error\n\tBase\n}\n\ntype Base struct{ Name string }\n\nfunc (b *Base) Hello() string { return b.Name }\n\ntype Service struct {\n\tBase\n\t*Other\n\tName string\n\tlist List[int]\n}\n\ntype ID int\ntype Alias = Base\ntype List[T any] struct{ items []T }\n\nfunc (l *List[T]) Push(t T) {}\nfunc New(name string) *Service { return nil }\n";
        let file = extract(source);
        assert_eq!(
            qualified(&file),
            [
                "module:svc",
                "type:interface:svc::Runner",
                "method:svc::Runner::Run",
                "type:struct:svc::Base",
                "method:svc::Base::Hello",
                "type:struct:svc::Service",
                "type:defined:svc::ID",
                "type:alias:svc::Alias",
                "type:struct:svc::List",
                "method:svc::List::Push",
                "function:svc::New",
            ]
        );
        // The interface owns its method lexically; everything else is a
        // file child.
        let run = &file.defs[2];
        assert_eq!(run.parent, Some(1));
        assert!(file.defs[4].parent.is_none());

        let embeds: Vec<(EdgeKind, &str, Option<&str>, &str)> = file
            .embeds
            .iter()
            .map(|e| {
                (
                    e.kind,
                    e.declarant.as_str(),
                    e.target.package.as_deref(),
                    e.target.name.as_str(),
                )
            })
            .collect();
        assert_eq!(
            embeds,
            [
                (EdgeKind::Implements, "Runner", None, "Base"),
                (EdgeKind::Inherits, "Service", None, "Base"),
                (EdgeKind::Inherits, "Service", None, "Other"),
            ],
            "named fields and type arguments are never embeddings"
        );
        assert_eq!(file.parse_errors, 0);
    }

    #[test]
    fn imports_keep_the_path_and_record_the_local_name() {
        let source = "package main\n\nimport (\n\t\"os\"\n\tsvc \"example.invalid/app/internal/svc\"\n\t. \"fmt\"\n\t_ \"embed\"\n\t\"github.com/x/y\"\n)\n\nimport \"strings\"\n";
        let file = extract(source);
        let paths: Vec<&str> = file.imports.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "os",
                "example.invalid/app/internal/svc as svc",
                "fmt as .",
                "embed as _",
                "github.com/x/y",
                "strings",
            ]
        );
        assert_eq!(
            file.package_imports,
            [
                ("os".to_string(), "os".to_string()),
                (
                    "svc".to_string(),
                    "example.invalid/app/internal/svc".to_string()
                ),
                ("y".to_string(), "github.com/x/y".to_string()),
                ("strings".to_string(), "strings".to_string()),
            ],
            "dot and blank imports bind no local name"
        );
    }

    #[test]
    fn calls_are_named_by_their_callee_and_positioned_at_the_resolving_identifier() {
        let source = "package main\n\nfunc run(s *Service) {\n\tsvc.New(\"x\")\n\ts.Hello()\n\tlen(s.items)\n\tMap[int](s)\n\tfunc() {}()\n\tgo s.Start()\n\tdefer s.Stop()\n\tT(s)\n}\n";
        let file = extract(source);
        let calls: Vec<(&str, &str)> = file
            .calls
            .iter()
            .map(|c| {
                (
                    c.raw_name.as_str(),
                    std::str::from_utf8(&source.as_bytes()[c.name_start_byte..])
                        .unwrap()
                        .split(['(', '[', ' ', '\n'])
                        .next()
                        .unwrap(),
                )
            })
            .collect();
        assert_eq!(
            calls,
            [
                ("svc.New", "New"),
                ("s.Hello", "Hello"),
                ("len", "len"),
                ("Map[int]", "Map"),
                ("func() {}", "func"),
                ("s.Start", "Start"),
                ("s.Stop", "Stop"),
                ("T", "T"),
            ]
        );
        assert!(
            file.calls
                .iter()
                .all(|c| c.receiver == ReceiverShape::Other && c.scope.is_empty())
        );
    }

    #[test]
    fn conversions_are_observations_only_when_they_name_a_declaration() {
        let file = extract(
            "package p\n\nfunc f(x string) {\n\t_ = []byte(x)\n\t_ = pkg.Kind(x)\n\t_ = (*T)(nil)\n\t_ = func() int { return 1 }()\n}\n",
        );
        let names: Vec<&str> = file.calls.iter().map(|c| c.raw_name.as_str()).collect();
        // `[]byte(x)` names no declaration and is absent; `pkg.Kind(x)` may
        // be a generic function call and is kept; `(*T)(nil)` parses as a
        // call of a parenthesized expression and stays an observation by
        // its text, as does the immediately invoked literal.
        assert_eq!(names, ["pkg.Kind", "(*T)", "func() int { return 1 }"]);
    }

    #[test]
    fn a_malformed_file_reports_errors_without_aborting() {
        let file = extract("package p\n\nfunc broken( {\n\ttype\n}\n");
        assert!(file.parse_errors > 0);
        assert_eq!(file.defs[0].name, "p");
    }
}

// ───────────────────────── measures ─────────────────────────
