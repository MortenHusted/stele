//! Tree-sitter PHP syntax extraction (P1: syntax-only).
//!
//! Owns parsing and language-specific extraction only. It never guesses
//! call targets, runs an LSP, or resolves cross-file references — that is
//! [`crate::resolve`]. This module produces raw, source-addressed
//! observations; [`crate::extract`] assigns deterministic IDs and
//! [`crate::resolve`] upgrades resolvable observations into edges.
//!
//! What is emitted directly from syntax (no resolution needed):
//! - **definition nodes** — namespace → Module; class/enum →
//!   [`NodeKind::Type`]; interface → [`TypeKind::Interface`]; trait →
//!   [`NodeKind::Trait`]; function → Function; method → Method (constants
//!   are values, not structure; the schema intentionally has no constant
//!   node kind);
//! - **lexical `contains`** edges (file/namespace/class → nested item);
//! - **`imports`** edges (file → `ImportRef` placeholder) preserving the
//!   namespace-use path verbatim, with aliases recorded as `Path as Alias`
//!   and group imports flattened member-by-member; never pretending they
//!   resolve in-repository.
//!
//! What is collected as observations for later resolution:
//! - **`extends` / `implements` clauses** → [`RawHeritage`], upgraded to
//!   `inherits` / `implements` edges only when both sides resolve to unique
//!   in-repository definitions;
//! - **trait `use` inside a class body** → [`RawTraitUse`], upgraded to an
//!   `implements` edge under the same uniqueness discipline;
//! - **call expressions** (`member_call`, `scoped_call`,
//!   `function_call`, `nullsafe_member_call`, `object_creation`) →
//!   [`RawCall`] observations. Syntax-only mode never emits canonical
//!   `calls` edges for PHP.
//! - **declared receiver types** (P3): a member call whose receiver is a
//!   typed parameter of the enclosing callable (`$param->m()`) or a
//!   typed property of the enclosing class reached through `$this`
//!   (`$this->prop->m()`, including constructor-promoted properties) is
//!   recorded as [`ReceiverShape::Declared`] with the qualified declared
//!   class. Only a plain or nullable single class name counts; a
//!   parameter the callable body assigns (plain, compound, or reference
//!   assignment, destructuring, `foreach`, `catch`, `global`, `static`,
//!   `unset`), a body that uses variable variables or `extract`, a
//!   variadic parameter, a closure's captured variable, a static
//!   property, and a property the class does not declare itself (an
//!   inherited or trait property) all stay [`ReceiverShape::Other`].
//!
//! Honest gaps (never guessed): magic dispatch (`__call`, `__callStatic`,
//! `__get`), dynamic calls (`$x->$m()`, `call_user_func*`), dynamic object
//! creation (`new $classname`), and `include`/`require` expressions never
//! produce edges; they surface as observations or file-level import
//! placeholders only (require/include recorded as imports verbatim).
//! Locals assigned from `new` or from a call are not declared receivers.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use tree_sitter::{Language, Node, Parser};

use crate::clones::Token;
use crate::measures::{
    Form, LineClass, LineCounts, RawMeasure, TokenRules, comment_ranges, count_lines, line_classes,
    operator_count, token_stream,
};
use crate::rust_syntax::{RawCall, RawDef, RawImport, ReceiverShape};
use crate::schema::{EdgeKind, NodeKind, Span, TypeKind};

static PHP_LANGUAGE: LazyLock<Language> = LazyLock::new(|| tree_sitter_php::LANGUAGE_PHP.into());
static PHP_ONLY_LANGUAGE: LazyLock<Language> =
    LazyLock::new(|| tree_sitter_php::LANGUAGE_PHP_ONLY.into());

/// Pure-PHP files are parsed with the lighter `php_only` grammar; anything
/// containing non-PHP text outside `<?php ?>` blocks needs the mixed-mode
/// `php` grammar or every literal line counts as an error.
fn language_for(source: &[u8]) -> &'static Language {
    let is_pure = source.starts_with(b"<?php") || source.starts_with(b"<?=");
    if is_pure {
        &PHP_ONLY_LANGUAGE
    } else {
        &PHP_LANGUAGE
    }
}

/// A raw `extends` / `implements` clause observation.
///
/// `kind` is `Inherits` for an `extends` clause (class extends class,
/// interface extends interface) and `Implements` for an `implements`
/// clause. Each written target yields its own observation.
#[derive(Debug, Clone)]
pub struct RawHeritage {
    pub kind: EdgeKind,
    /// Type reference as written (e.g. `Base`, `Vendor\Pkg\Iface`).
    pub target: String,
    /// Namespace-qualified name of the declaring type.
    pub declarant: String,
    pub span: Span,
}

/// A raw trait-`use` observation inside a class body.
#[derive(Debug, Clone)]
pub struct RawTraitUse {
    /// Trait reference as written.
    pub trait_name: String,
    /// Namespace-qualified name of the enclosing class/trait/enum.
    pub class_name: String,
    pub span: Span,
}

/// Per-file PHP extraction output.
#[derive(Debug, Clone)]
pub struct RawFile {
    pub defs: Vec<RawDef>,
    pub imports: Vec<RawImport>,
    pub calls: Vec<RawCall>,
    pub heritage: Vec<RawHeritage>,
    pub trait_uses: Vec<RawTraitUse>,
    /// Indices into `defs` of methods declared `static`. A constant
    /// receiver (`Cls::m()`) may only reach these.
    pub static_methods: Vec<usize>,
    pub parse_errors: usize,
    /// One entry per callable def (function, method).
    pub measures: Vec<RawMeasure>,
    /// Line classes of the whole file.
    pub file_lines: LineCounts,
    /// Every line's class, for clone occurrence counts.
    pub line_classes: Vec<LineClass>,
    /// The normalised token stream clone detection reads.
    pub tokens: Vec<Token>,
}

/// How PHP leaves become the normalised token stream: comments dropped,
/// every `name` `$id` (a `$var` is the `$` token and `$id`), strings,
/// heredocs, numbers, booleans, and inline HTML `$lit`; keywords,
/// `null`, and punctuation stay as written.
pub const PHP_TOKENS: TokenRules = TokenRules {
    comments: &["comment"],
    identifiers: &["name"],
    literals: &[
        "string",
        "encapsed_string",
        "heredoc",
        "nowdoc",
        "integer",
        "float",
        "boolean",
        "text",
    ],
};

/// Extract structural observations from a single PHP source file.
pub fn extract_file(rel_path: &str, source: &[u8]) -> Result<RawFile> {
    let _ = rel_path; // matches the other adapters' signature
    let mut parser = Parser::new();
    parser
        .set_language(language_for(source))
        .context("set tree-sitter-php language")?;
    let tree = parser
        .parse(source, None)
        .context("tree-sitter parse returned None")?;

    let root = tree.root_node();
    let mut walker = Walker::new(source);
    let mut ctx = Ctx::default();
    walker.visit_children(root, &mut ctx);
    walker.resolve_property_receivers();

    let classes = line_classes(source, &comment_ranges(root, PHP_TOKENS.comments));
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
    let tokens = token_stream(root, source, &PHP_TOKENS);

    Ok(RawFile {
        defs: walker.defs,
        imports: walker.imports,
        calls: walker.calls,
        heritage: walker.heritage,
        trait_uses: walker.trait_uses,
        static_methods: walker.static_methods,
        parse_errors: walker.errors,
        measures,
        file_lines,
        line_classes: classes,
        tokens,
    })
}

/// Mutable traversal context: the lexical scope chain (root-first), the
/// namespace qualification prefix, and the current lexical parent def.
#[derive(Default)]
struct Ctx {
    /// Names of enclosing defs, root-first: qualified for types, leaf for
    /// functions/methods. The active namespace lives in `ns_prefix`, not
    /// here, so type/method scopes never repeat the namespace segment.
    chain: Vec<String>,
    /// `\`-joined active namespace (`namespace App\Model;` → `App\Model`).
    ns_prefix: Option<String>,
    /// Full qualified names of enclosing type defs (innermost last); used
    /// to name heritage/trait-use declarants.
    type_stack: Vec<String>,
    /// Index into `defs` of the lexical parent node (`None` = file).
    parent: Option<usize>,
    /// Receiver-regime input: `Some(true)` inside an instance method body,
    /// `Some(false)` inside a static method, `None` outside any method.
    /// `$this->m()` is instance-verifiable only inside `Some(true)`.
    in_instance_method: Option<bool>,
    /// Class imports in force: lowercase alias to fully qualified name
    /// (`use App\Model\User as U;` → `("u", "App\Model\User")`). PHP
    /// resolves the first segment of a written class name against this
    /// table before the active namespace.
    uses: Vec<(String, String)>,
    /// Parameter scopes of the enclosing callables, innermost last.
    locals: Vec<LocalScope>,
}

/// The parameters of one callable body, with the declared class of each
/// (when its type is one plain or nullable in-source class name), and
/// the variable names the body assigns, which lose their declared type.
#[derive(Default)]
struct LocalScope {
    params: HashMap<String, Option<String>>,
    reassigned: HashSet<String>,
    /// An arrow function captures the enclosing scope by value, so a
    /// lookup that misses here continues outward; a function, method, or
    /// closure sees only its own parameters and stops here.
    transparent: bool,
}

/// Sentinel in `LocalScope::reassigned`: the body writes locals by name
/// at runtime (`$$name`, `extract(...)`), so no parameter keeps its type.
const ANY_LOCAL: &str = "*";

/// The one class a declared type names, qualified: `Foo`, `?Foo`,
/// `\Ext\Q`. Union and intersection types, scalars, `mixed`, `object`,
/// and the relative names `self`, `static`, and `parent` name no single
/// in-source class and yield `None`.
fn declared_class(ctx: &Ctx, source: &[u8], type_node: Node) -> Option<String> {
    let named = match type_node.kind() {
        "named_type" => type_node,
        "optional_type" => {
            let mut cursor = type_node.walk();
            type_node
                .named_children(&mut cursor)
                .find(|n| n.kind() == "named_type")?
        }
        _ => return None,
    };
    let written = text(source, named);
    let leaf = written.rsplit('\\').next().unwrap_or(&written);
    if matches!(
        leaf.to_ascii_lowercase().as_str(),
        "self" | "static" | "parent"
    ) {
        return None;
    }
    Some(qualify_class_reference(ctx, &written))
}

/// Variable names (without `$`) the subtree under `body` assigns, plus
/// [`ANY_LOCAL`] when it writes locals by name at runtime. Over-approximate
/// by design: a name written anywhere in the body, including inside a
/// nested closure, loses its declared type for the whole body.
fn assigned_names(body: Node, source: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "assignment_expression"
            | "augmented_assignment_expression"
            | "reference_assignment_expression" => {
                if let Some(left) = node.child_by_field_name("left") {
                    collect_variable_names(left, source, &mut names);
                }
            }
            "update_expression"
            | "global_declaration"
            | "static_variable_declaration"
            | "unset_statement" => collect_variable_names(node, source, &mut names),
            "foreach_statement" => {
                // The first named child is the iterated expression; every
                // variable named after `as` is written on each iteration.
                let body_start = node.child_by_field_name("body").map(|b| b.start_byte());
                let mut cursor = node.walk();
                for (i, child) in node.named_children(&mut cursor).enumerate() {
                    if i == 0 || Some(child.start_byte()) == body_start {
                        continue;
                    }
                    collect_variable_names(child, source, &mut names);
                }
            }
            "catch_clause" => {
                if let Some(name) = node.child_by_field_name("name") {
                    collect_variable_names(name, source, &mut names);
                }
            }
            "dynamic_variable_name" => {
                names.insert(ANY_LOCAL.to_string());
            }
            "function_call_expression" => {
                let callee = node.child_by_field_name("function").map(|f| {
                    text(source, f)
                        .trim_start_matches('\\')
                        .to_ascii_lowercase()
                });
                if callee.as_deref() == Some("extract") {
                    names.insert(ANY_LOCAL.to_string());
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    names
}

/// Every `$name` under `node`, recorded without the sigil.
fn collect_variable_names(node: Node, source: &[u8], names: &mut HashSet<String>) {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "variable_name" {
            names.insert(text(source, n).trim_start_matches('$').to_string());
            continue;
        }
        let mut cursor = n.walk();
        stack.extend(n.children(&mut cursor));
    }
}

/// The declared class of local `name` at this point of the walk, when the
/// enclosing callable declares it as a parameter and never assigns it.
fn declared_local(ctx: &Ctx, name: &str) -> Option<String> {
    for scope in ctx.locals.iter().rev() {
        if scope.reassigned.contains(name) || scope.reassigned.contains(ANY_LOCAL) {
            return None;
        }
        if let Some(declared) = scope.params.get(name) {
            return declared.clone();
        }
        if !scope.transparent {
            return None;
        }
    }
    None
}

struct Walker<'a> {
    source: &'a [u8],
    defs: Vec<RawDef>,
    imports: Vec<RawImport>,
    calls: Vec<RawCall>,
    heritage: Vec<RawHeritage>,
    trait_uses: Vec<RawTraitUse>,
    static_methods: Vec<usize>,
    /// Declared class of each non-static property this file declares,
    /// keyed by (qualified declaring type, property name); `None` when the
    /// type is absent or names no single class.
    properties: HashMap<(String, String), Option<String>>,
    /// Member calls on `$this->prop`, as (call index, qualified enclosing
    /// type, property name); resolved against `properties` once the whole
    /// file is read, since a declaration may follow its uses.
    property_receivers: Vec<(usize, String, String)>,
    /// `(def index, cyclomatic complexity)` per callable, in def order.
    callables: Vec<(usize, u32, Form)>,
    errors: usize,
}

/// The receiver/expression operand of a call expression: the first named
/// child that is not the `name` field (tree-sitter-php does not name the
/// receiver slot, so position is the reliable discriminator).
fn first_named_non_name_child(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    let name_start = node.child_by_field_name("name").map(|n| n.start_byte());
    node.named_children(&mut cursor)
        .find(|child| Some(child.start_byte()) != name_start && child.kind() != "arguments")
}

/// Start byte of the first name token under `node`, in source order: the
/// point where an import path begins.
fn first_name_start(node: Node) -> Option<usize> {
    let mut stack = vec![node];
    let mut earliest: Option<usize> = None;
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "name" | "qualified_name" | "namespace_name") {
            let start = n.start_byte();
            earliest = Some(earliest.map_or(start, |e| e.min(start)));
            continue;
        }
        let mut cursor = n.walk();
        stack.extend(n.children(&mut cursor));
    }
    earliest
}

fn text(source: &[u8], node: Node) -> String {
    node.utf8_text(source).unwrap_or_default().to_string()
}

/// Every adapter numbers lines from 1 (`Span::from_ts` adds one to the
/// zero-based tree-sitter row); until P2 this one did not, so every PHP
/// span was one line early.
fn span(node: Node) -> Span {
    Span::from_ts(
        node.start_byte(),
        node.end_byte(),
        (node.start_position().row, node.start_position().column),
        (node.end_position().row, node.end_position().column),
    )
}

/// Fully qualify a freshly read declared name: nested under the innermost
/// enclosing def when any, otherwise under the active namespace. The
/// namespace is never in `chain`, so qualification cannot repeat its
/// prefix.
fn qualify_name(ctx: &Ctx, name: &str) -> String {
    ctx.chain.last().map_or_else(
        || {
            ctx.ns_prefix
                .as_ref()
                .map_or_else(|| name.to_string(), |ns| format!("{ns}::{name}"))
        },
        |enclosing| format!("{enclosing}::{name}"),
    )
}

/// Resolve a class name as written at a use site to its fully qualified
/// form (no leading backslash), by PHP's compile-time rules: a leading `\`
/// is already fully qualified; `namespace\X` is relative to the active
/// namespace; otherwise the first segment is looked up in the file's class
/// imports, and a name no import covers belongs to the active namespace.
/// Class names are case-insensitive, so aliases match ignoring case.
fn qualify_class_reference(ctx: &Ctx, written: &str) -> String {
    if let Some(fully_qualified) = written.strip_prefix('\\') {
        return fully_qualified.to_string();
    }
    let in_namespace = |name: &str| {
        ctx.ns_prefix
            .as_ref()
            .map_or_else(|| name.to_string(), |ns| format!("{ns}\\{name}"))
    };
    if let Some(relative) = written.strip_prefix("namespace\\") {
        return in_namespace(relative);
    }
    let (first, rest) = written
        .split_once('\\')
        .map_or((written, None), |(first, rest)| (first, Some(rest)));
    let first_lower = first.to_ascii_lowercase();
    match ctx.uses.iter().find(|(alias, _)| *alias == first_lower) {
        Some((_, imported)) => {
            rest.map_or_else(|| imported.clone(), |rest| format!("{imported}\\{rest}"))
        }
        None => in_namespace(written),
    }
}

/// Register one class import (`Path` or `Path as Alias`) in the table.
fn register_class_import(ctx: &mut Ctx, import: &str) {
    let (fully_qualified, alias) = import
        .split_once(" as ")
        .map_or((import, None), |(path, alias)| (path, Some(alias)));
    let fully_qualified = fully_qualified.trim_start_matches('\\');
    let alias = alias.unwrap_or_else(|| {
        fully_qualified
            .rsplit('\\')
            .next()
            .unwrap_or(fully_qualified)
    });
    ctx.uses
        .push((alias.to_ascii_lowercase(), fully_qualified.to_string()));
}

impl<'a> Walker<'a> {
    fn new(source: &'a [u8]) -> Self {
        Self {
            source,
            defs: Vec::new(),
            imports: Vec::new(),
            calls: Vec::new(),
            heritage: Vec::new(),
            trait_uses: Vec::new(),
            static_methods: Vec::new(),
            properties: HashMap::new(),
            property_receivers: Vec::new(),
            callables: Vec::new(),
            errors: 0,
        }
    }

    /// Upgrade each `$this->prop->m()` call to a declared receiver when
    /// the enclosing type declares `prop` with one class.
    fn resolve_property_receivers(&mut self) {
        for (index, class, property) in self.property_receivers.drain(..) {
            if let Some(Some(declared)) = self.properties.get(&(class, property)) {
                self.calls[index].receiver = ReceiverShape::Declared(declared.clone());
            }
        }
    }

    /// Record the parameters of a callable as the innermost local scope.
    /// A promoted constructor parameter also declares a property.
    fn enter_callable(&mut self, node: Node, ctx: &mut Ctx, transparent: bool) {
        let mut scope = LocalScope {
            transparent,
            ..LocalScope::default()
        };
        if let Some(parameters) = node.child_by_field_name("parameters") {
            let mut cursor = parameters.walk();
            for parameter in parameters.named_children(&mut cursor) {
                let Some(name) = parameter.child_by_field_name("name") else {
                    continue;
                };
                let name = self.text(name).trim_start_matches('$').to_string();
                let declared = match parameter.kind() {
                    "simple_parameter" | "property_promotion_parameter" => parameter
                        .child_by_field_name("type")
                        .and_then(|t| declared_class(ctx, self.source, t)),
                    // A variadic parameter is an array of its type.
                    _ => None,
                };
                if parameter.kind() == "property_promotion_parameter" {
                    if let Some(class) = ctx.type_stack.last() {
                        self.properties
                            .insert((class.clone(), name.clone()), declared.clone());
                    }
                }
                scope.params.insert(name, declared);
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            scope.reassigned = assigned_names(body, self.source);
        }
        ctx.locals.push(scope);
    }

    /// Record the declared class of each property in a declaration
    /// (`private ?Foo $a, $b;`). Static properties are reached through
    /// `self::$p`, never `$this->p`, and are left out.
    fn record_property_declaration(&mut self, node: Node, ctx: &Ctx) {
        let Some(class) = ctx.type_stack.last().cloned() else {
            return;
        };
        let mut cursor = node.walk();
        if node
            .children(&mut cursor)
            .any(|c| c.kind() == "static_modifier")
        {
            return;
        }
        let declared = node
            .child_by_field_name("type")
            .and_then(|t| declared_class(ctx, self.source, t));
        let mut cursor = node.walk();
        for element in node.children(&mut cursor) {
            if element.kind() != "property_element" {
                continue;
            }
            if let Some(name) = element.child_by_field_name("name") {
                let name = self.text(name).trim_start_matches('$').to_string();
                self.properties
                    .insert((class.clone(), name), declared.clone());
            }
        }
    }

    fn text(&self, node: Node) -> String {
        text(self.source, node)
    }

    fn push_def(&mut self, kind: NodeKind, name: String, span_of: Node, ctx: &Ctx) -> usize {
        // Scope = enclosing def names plus the active namespace prefix, so
        // type/function symbol keys read `App\Model::User::method`. The
        // caller pushes the def's own chain entry after this call.
        let mut scope = ctx.chain.clone();
        if scope.is_empty() {
            if let Some(ns) = &ctx.ns_prefix {
                scope = vec![ns.clone()];
            }
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

    fn record_call(
        &mut self,
        node: Node,
        name_node: Node,
        raw_name: String,
        receiver: ReceiverShape,
        ctx: &Ctx,
    ) {
        // Scope = enclosing type chain (qualified); the policy model reads
        // the innermost type as the `self` receiver class.
        let scope = ctx.type_stack.clone();
        self.calls.push(RawCall {
            raw_name,
            path_root: None,
            span: span(node),
            name_start_byte: name_node.start_byte(),
            receiver,
            scope,
        });
    }

    fn record_heritage(&mut self, clause: Node, kind: EdgeKind, ctx: &Ctx) {
        let Some(declarant) = ctx.type_stack.last().cloned() else {
            return;
        };
        let mut cursor = clause.walk();
        for child in clause.children(&mut cursor) {
            if matches!(child.kind(), "name" | "qualified_name") {
                self.heritage.push(RawHeritage {
                    kind,
                    target: qualify_class_reference(ctx, &self.text(child)),
                    declarant: declarant.clone(),
                    span: span(child),
                });
            }
        }
    }

    fn visit_children(&mut self, node: Node, ctx: &mut Ctx) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.visit(child, ctx);
        }
    }

    #[allow(clippy::too_many_lines)] // grammar-node dispatch reads as one table
    fn visit(&mut self, node: Node, ctx: &mut Ctx) {
        if node.has_error() && node.kind() == "ERROR" {
            self.errors += 1;
        }
        match node.kind() {
            "namespace_definition" => self.visit_namespace_definition(node, ctx),
            "namespace_use_declaration" => self.visit_namespace_use(node, ctx),
            "class_declaration" => {
                self.visit_type(node, ctx, NodeKind::Type(TypeKind::Class));
            }
            "interface_declaration" => {
                self.visit_type(node, ctx, NodeKind::Type(TypeKind::Interface));
            }
            "trait_declaration" => self.visit_type(node, ctx, NodeKind::Trait),
            "enum_declaration" => self.visit_type(node, ctx, NodeKind::Type(TypeKind::Enum)),
            "function_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let leaf = self.text(name_node);
                    let idx = self.push_def(NodeKind::Function, leaf.clone(), node, ctx);
                    self.callables.push((idx, cyclomatic(node), form(node)));
                    ctx.chain.push(leaf);
                    let saved_parent = ctx.parent.replace(idx);
                    self.enter_callable(node, ctx, false);
                    self.visit_children(node, ctx);
                    ctx.locals.pop();
                    ctx.parent = saved_parent;
                    ctx.chain.pop();
                } else {
                    self.visit_children(node, ctx);
                }
            }
            "anonymous_function" | "arrow_function" => {
                // A closure sees its own parameters only; an arrow
                // function also sees the enclosing scope, captured by
                // value.
                self.enter_callable(node, ctx, node.kind() == "arrow_function");
                self.visit_children(node, ctx);
                ctx.locals.pop();
            }
            "property_declaration" => {
                self.record_property_declaration(node, ctx);
                self.visit_children(node, ctx);
            }
            "method_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let leaf = self.text(name_node);
                    let idx = self.push_def(NodeKind::Method, leaf.clone(), node, ctx);
                    self.callables.push((idx, cyclomatic(node), form(node)));
                    ctx.chain.push(leaf);
                    let saved_parent = ctx.parent.replace(idx);
                    // `$this` regime: instance methods only. A `static`
                    // modifier anywhere in the declaration header makes the
                    // body a singleton context.
                    let is_static = node
                        .children_by_field_name("modifier", &mut node.walk())
                        .any(|m| m.kind() == "static_modifier")
                        || {
                            let mut c = node.walk();
                            node.children(&mut c)
                                .any(|ch| ch.kind() == "static_modifier")
                        };
                    if is_static {
                        self.static_methods.push(idx);
                    }
                    let saved_im = ctx.in_instance_method.replace(!is_static);
                    self.enter_callable(node, ctx, false);
                    self.visit_children(node, ctx);
                    ctx.locals.pop();
                    ctx.in_instance_method = saved_im;
                    ctx.parent = saved_parent;
                    ctx.chain.pop();
                } else {
                    self.visit_children(node, ctx);
                }
            }
            "const_declaration" => {
                // Constants are values, not structure; the schema has no
                // constant node kind (consistent with the Rust and Ruby
                // adapters). Descend only to cover nested expressions.
                self.visit_children(node, ctx);
            }
            "use_declaration" => {
                if let Some(class_name) = ctx.type_stack.last().cloned() {
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        if matches!(child.kind(), "name" | "qualified_name") {
                            self.trait_uses.push(RawTraitUse {
                                trait_name: qualify_class_reference(ctx, &self.text(child)),
                                class_name: class_name.clone(),
                                span: span(child),
                            });
                        }
                    }
                }
                // Adaptation clauses can contain member references but no
                // calls; still descend for completeness of ERROR counting.
                self.visit_children(node, ctx);
            }
            "member_call_expression" | "nullsafe_member_call_expression" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let raw = self.text(node);
                    let receiver = self.member_receiver(node, ctx, self.calls.len());
                    self.record_call(node, name_node, raw, receiver, ctx);
                }
                self.visit_children(node, ctx);
            }
            "scoped_call_expression" => {
                // `Cls::m()` — constant receiver when the scope is a name;
                // `parent::m()` resolves to the verified superclass.
                if let Some(name_node) = node.child_by_field_name("name") {
                    let raw = self.text(node);
                    let receiver = self.scoped_receiver(node, ctx);
                    self.record_call(node, name_node, raw, receiver, ctx);
                }
                self.visit_children(node, ctx);
            }
            "function_call_expression" => {
                if let Some(func) = node.child_by_field_name("function") {
                    let raw = self.text(func);
                    let name_node = if func.kind() == "qualified_name" {
                        func.child_by_field_name("name").unwrap_or(func)
                    } else {
                        func
                    };
                    self.record_call(node, name_node, raw, ReceiverShape::Other, ctx);
                }
                self.visit_children(node, ctx);
            }
            "object_creation_expression" => {
                // `new Foo(...)` records the class name; `new $x(...)` is an
                // unnameable dynamic-construction gap (no call recorded).
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if matches!(child.kind(), "qualified_name" | "name") {
                        self.record_call(node, child, self.text(node), ReceiverShape::Other, ctx);
                    }
                }
                self.visit_children(node, ctx);
            }
            "include_expression"
            | "include_once_expression"
            | "require_expression"
            | "require_once_expression" => {
                // Record the include path verbatim as an import placeholder,
                // exactly like Ruby `require`.
                if let Some(arg) = node.named_child(0) {
                    self.imports.push(RawImport {
                        path: self.text(arg),
                        span: span(node),
                    });
                }
                self.visit_children(node, ctx);
            }
            _ => self.visit_children(node, ctx),
        }
    }

    /// Receiver shape of an instance/member call. `$this` inside an
    /// instance method is the implicit self; a typed, never-assigned
    /// parameter is a declared receiver; `$this->prop` is a declared
    /// receiver once the file's property declarations are known
    /// (`call_index` names the call for that later pass); every other
    /// object receiver is the guess regime (`Other`).
    fn member_receiver(&mut self, node: Node, ctx: &Ctx, call_index: usize) -> ReceiverShape {
        let Some(object) = node.child_by_field_name("object") else {
            return ReceiverShape::Other;
        };
        match object.kind() {
            "variable_name" => {
                let name = self.text(object);
                if name == "$this" {
                    if ctx.in_instance_method == Some(true) {
                        ReceiverShape::ImplicitSelf
                    } else {
                        ReceiverShape::Other
                    }
                } else {
                    declared_local(ctx, name.trim_start_matches('$'))
                        .map_or(ReceiverShape::Other, ReceiverShape::Declared)
                }
            }
            "member_access_expression" => {
                let on_this = object
                    .child_by_field_name("object")
                    .is_some_and(|o| o.kind() == "variable_name" && self.text(o) == "$this");
                let property = object
                    .child_by_field_name("name")
                    .filter(|n| n.kind() == "name")
                    .map(|n| self.text(n));
                if let (true, Some(true), Some(class), Some(property)) = (
                    on_this,
                    ctx.in_instance_method,
                    ctx.type_stack.last(),
                    property,
                ) {
                    self.property_receivers
                        .push((call_index, class.clone(), property));
                }
                ReceiverShape::Other
            }
            _ => ReceiverShape::Other,
        }
    }

    /// Receiver shape of a static call (`Cls::m()`, `parent::m()`,
    /// `static::m()`). The scope node is `qualified_name` for named classes
    /// and `relative_scope` for `self`/`parent`/`static` keywords.
    fn scoped_receiver(&self, node: Node, ctx: &Ctx) -> ReceiverShape {
        let scope = first_named_non_name_child(node);
        match scope.map(|n| (n.kind(), self.text(n))) {
            Some(("relative_scope", text)) => match text.as_str() {
                "self" | "static" if ctx.in_instance_method == Some(true) => {
                    ReceiverShape::ImplicitSelf
                }
                "parent" => ReceiverShape::Parent,
                // Static method body: singleton regime — no verifiable
                // receiver class beyond the class itself; admit only via
                // constant-style chain checks, which `Other` does not do.
                #[allow(clippy::match_same_arms)] // distinct meaning from catch-all
                "self" | "static" => ReceiverShape::Other,
                _ => ReceiverShape::Other,
            },
            Some(("name" | "qualified_name", text)) => {
                ReceiverShape::Constant(qualify_class_reference(ctx, &text))
            }
            _ => ReceiverShape::Other,
        }
    }

    fn visit_namespace_definition(&mut self, node: Node, ctx: &mut Ctx) {
        let name = node
            .child_by_field_name("name")
            .map(|n| self.text(n))
            .unwrap_or_default();
        if let Some(body) = node.child_by_field_name("body") {
            // Braced form: namespace def becomes a Module node; its body
            // contents nest lexically under it. `ns_prefix` records the
            // qualifier; the Module itself is the lexical parent of its
            // body defs, but is NOT in `chain` (qualification via
            // `qualify_name`).
            let idx = self.push_def(NodeKind::Module, name.clone(), node, ctx);
            let saved_ns = ctx.ns_prefix.replace(name);
            let saved_parent = ctx.parent.replace(idx);
            let saved_uses = std::mem::take(&mut ctx.uses);
            self.visit_children(body, ctx);
            ctx.uses = saved_uses;
            ctx.parent = saved_parent;
            ctx.ns_prefix = saved_ns;
        } else {
            // Unbraced form: the namespace qualifies all following
            // definitions in the file. Recorded as a Module node for
            // identity; following defs are file-level siblings owned by the
            // file. `ns_prefix` scopes everything that follows.
            let _ = self.push_def(NodeKind::Module, name.clone(), node, ctx);
            ctx.ns_prefix = Some(name);
            ctx.uses.clear();
        }
    }

    fn visit_namespace_use(&mut self, node: Node, ctx: &mut Ctx) {
        // `use function f;` and `use const C;` import callables and
        // constants, which never name a class. The grammar keeps neither
        // keyword as a node, so read the text between `use` and the first
        // clause.
        let imports_classes = {
            let after_use = node
                .child(0)
                .map_or_else(|| node.start_byte(), |n| n.end_byte());
            let first_name = first_name_start(node).unwrap_or_else(|| node.end_byte());
            let keyword = std::str::from_utf8(&self.source[after_use..first_name.max(after_use)])
                .unwrap_or_default()
                .trim();
            !matches!(keyword, "function" | "const")
        };
        // Group form: `use Foo\{Bar, Baz as Qux};`
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "namespace_use_clause" => {
                    let path = self.clause_path("", child);
                    if imports_classes {
                        register_class_import(ctx, &path);
                    }
                    self.imports.push(RawImport {
                        path,
                        span: span(child),
                    });
                }
                "namespace_use_group" => {
                    // The group's prefix is the `namespace_name` sibling
                    // preceding it inside this `use` declaration
                    // (`use Vendor\Pkg\{...}`): the grammar does not expose
                    // it as a `prefix` field on the group node.
                    let prefix = node
                        .children(&mut node.walk())
                        .take_while(|n| n.kind() != "namespace_use_group")
                        .find(|n| n.kind() == "namespace_name")
                        .map(|n| self.text(n))
                        .unwrap_or_default();
                    let mut stack = vec![child];
                    while let Some(n) = stack.pop() {
                        if n.kind() == "namespace_use_clause" {
                            let path = self.clause_path(&prefix, n);
                            if imports_classes {
                                register_class_import(ctx, &path);
                            }
                            self.imports.push(RawImport {
                                path,
                                span: span(n),
                            });
                        } else {
                            let mut c = n.walk();
                            stack.extend(n.children(&mut c));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Path text of one `namespace_use_clause`, alias recorded verbatim as
    /// `Path as Alias`, prefixed for group members.
    fn clause_path(&self, prefix: &str, clause: Node) -> String {
        let name = clause
            .child_by_field_name("name")
            .or_else(|| {
                let mut c = clause.walk();
                clause
                    .children(&mut c)
                    .find(|n| matches!(n.kind(), "name" | "qualified_name"))
            })
            .map(|n| self.text(n))
            .unwrap_or_default();
        let alias = clause.child_by_field_name("alias").map(|n| self.text(n));
        let full = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}\\{name}")
        };
        match alias {
            Some(a) => format!("{full} as {a}"),
            None => full,
        }
    }

    fn visit_type(&mut self, node: Node, ctx: &mut Ctx, kind: NodeKind) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.visit_children(node, ctx);
            return;
        };
        let leaf = self.text(name_node);
        let qualified = qualify_name(ctx, &leaf);

        // Convention: `chain` holds the name pushed for each enclosing def —
        // the qualified name for types/namespaces, the leaf for
        // functions/methods. A def's `scope` (chain at push time, before its
        // own entry) plus its leaf `name` yields the qualified symbol key
        // exactly once: `App\Model::User` → scope [`App\Model`], name `User`.
        let idx = self.push_def(kind, leaf, node, ctx);
        ctx.chain.push(qualified.clone());
        ctx.type_stack.push(qualified);

        // Heritage clauses (siblings of the body, inside this decl).
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "base_clause" => self.record_heritage(child, EdgeKind::Inherits, ctx),
                "class_interface_clause" => {
                    self.record_heritage(child, EdgeKind::Implements, ctx);
                }
                _ => {}
            }
        }

        let saved_parent = ctx.parent.replace(idx);
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_children(body, ctx);
        }
        ctx.parent = saved_parent;
        ctx.type_stack.pop();
        ctx.chain.pop();
    }
}

// ───────────────────────── measures ─────────────────────────

/// Cyclomatic complexity of one callable: 1 plus its decision points.
///
/// Decision points (policy `cc-sloc-clones-form/php/1`, plan section 4): `if`,
/// `elseif` (and an `else if`, which the grammar reads as an `if` inside
/// the `else`), the ternary `?:` (including `?:` short form), `??`,
/// `while`, `do`, `for`, `foreach`, each `match` arm (the default arm
/// included: it is a branch the value can take), each `case`, each
/// `catch`; each `&&`, `||`, `and`, `or`, `xor` operator. Closures and
/// arrow functions are part of the callable that encloses them, so their
/// branches count here; nested named functions, methods, and type
/// declarations are callables or containers of their own and are skipped.
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
            "if_statement"
            | "else_if_clause"
            | "conditional_expression"
            | "while_statement"
            | "do_statement"
            | "for_statement"
            | "foreach_statement"
            | "match_conditional_expression"
            | "match_default_expression"
            | "case_statement"
            | "catch_clause" => 1,
            "binary_expression" => operator_count(child, &["&&", "||", "and", "or", "xor", "??"]),
            _ => 0,
        };
        count += decision_points(child);
    }
    count
}

/// The form measures of one callable under `cc-sloc-clones-form/php/1`,
/// over the same body [`cyclomatic`] reads: nested named definitions are
/// left to their own measures; closures and arrow functions belong to the
/// callable that encloses them. PHP's `$this->x` is a member access like
/// any other, so `ivars` is absent.
pub fn form(callable: Node) -> Form {
    Form {
        params: params(callable),
        nesting: nesting(callable),
        chain: chain(callable),
        ivars: None,
    }
}

/// Parameters the definition declares: each `simple_parameter`,
/// `variadic_parameter`, and `property_promotion_parameter` under the
/// `parameters` field. PHP has no receiver parameter.
fn params(callable: Node) -> u32 {
    let Some(parameters) = callable.child_by_field_name("parameters") else {
        return 0;
    };
    let mut cursor = parameters.walk();
    let count = parameters
        .named_children(&mut cursor)
        .filter(|child| {
            matches!(
                child.kind(),
                "simple_parameter" | "variadic_parameter" | "property_promotion_parameter"
            )
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The deepest stack of control structures any node of the body sits
/// under: `if` (its `elseif` and `else` clauses are fields of the same
/// node, so their bodies sit at the `if`'s depth), `while`, `do`, `for`,
/// `foreach`, `switch` (cases add nothing), `match` (arms add nothing),
/// and closure or arrow-function bodies. `try` is not a level. A straight
/// body is 0.
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
        let level = u32::from(matches!(
            child.kind(),
            "if_statement"
                | "while_statement"
                | "do_statement"
                | "for_statement"
                | "foreach_statement"
                | "switch_statement"
                | "match_expression"
                | "anonymous_function"
                | "arrow_function"
        ));
        deepest = deepest.max(depth_below(child, depth + level));
    }
    deepest
}

/// The longest dispatch spine of any expression in the body ([`spine`]);
/// a body without a member or static access is 0.
fn chain(callable: Node) -> u32 {
    let mut longest = 0;
    let mut cursor = callable.walk();
    for child in callable.children(&mut cursor) {
        if is_named_definition(child.kind()) {
            continue;
        }
        if is_hop(child.kind()) {
            longest = longest.max(spine(child));
        }
        longest = longest.max(chain(child));
    }
    longest
}

/// The node kinds that are one hop of a spine: `->`, `?->`, and `::`
/// dispatches to a member, method, static method, or static property.
const fn is_hop(kind: &str) -> bool {
    matches!(
        kind.as_bytes(),
        b"member_call_expression"
            | b"member_access_expression"
            | b"nullsafe_member_call_expression"
            | b"nullsafe_member_access_expression"
            | b"scoped_call_expression"
            | b"scoped_property_access_expression"
    )
}

/// Hops along one expression's receiver spine: each `->`, `?->`, or `::`
/// dispatch is one; a plain call, a subscript, and parentheses pass the
/// spine through without counting; a variable, a bare call, or a class
/// constant ends it at 0. `$this->repo->find($id)->items()` is 3,
/// `Str::of($s)->upper()` is 2, `$items[0]->name` is 1.
fn spine(node: Node) -> u32 {
    match node.kind() {
        "member_call_expression"
        | "member_access_expression"
        | "nullsafe_member_call_expression"
        | "nullsafe_member_access_expression" => {
            1 + node.child_by_field_name("object").map_or(0, spine)
        }
        "scoped_call_expression" | "scoped_property_access_expression" => {
            1 + node.child_by_field_name("scope").map_or(0, spine)
        }
        "function_call_expression" => node.child_by_field_name("function").map_or(0, spine),
        "subscript_expression" | "parenthesized_expression" => node.named_child(0).map_or(0, spine),
        _ => 0,
    }
}

const fn is_named_definition(kind: &str) -> bool {
    matches!(
        kind.as_bytes(),
        b"function_definition"
            | b"method_declaration"
            | b"class_declaration"
            | b"interface_declaration"
            | b"trait_declaration"
            | b"enum_declaration"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> RawFile {
        extract_file("f.php", source.as_bytes()).expect("php source parses")
    }

    fn constant_receivers(source: &str) -> Vec<String> {
        extract(source)
            .calls
            .into_iter()
            .filter_map(|call| match call.receiver {
                ReceiverShape::Constant(name) => Some(name),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_written_class_name_is_qualified_by_php_rules() {
        let source = "<?php\nnamespace App\\Model;\nuse Vendor\\Pkg\\{Alpha, Beta as B};\nuse Other\\Thing as T;\nuse function sprintf as fmt;\nfunction f() {\n  Local::a();\n  \\Fully\\Qualified::b();\n  B::c();\n  T\\Sub::d();\n  alpha::e();\n  fmt::h();\n}\n";
        assert_eq!(
            constant_receivers(source),
            [
                "App\\Model\\Local",
                "Fully\\Qualified",
                "Vendor\\Pkg\\Beta",
                "Other\\Thing\\Sub",
                "Vendor\\Pkg\\Alpha",
                "App\\Model\\fmt",
            ]
        );
    }

    fn declared_receivers(source: &str) -> Vec<(String, String)> {
        extract(source)
            .calls
            .into_iter()
            .filter_map(|call| match call.receiver {
                ReceiverShape::Declared(class) => Some((call.raw_name, class)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_typed_parameter_the_body_never_assigns_is_a_declared_receiver() {
        let source = "<?php\nnamespace App;\nuse Lib\\Renderer;\nfunction f(Foo $a, ?Renderer $r, \\Ext\\Q $q, Foo|Bar $u, mixed $m, self $s, Foo ...$many, $untyped) {\n  $a->one();\n  $r?->two();\n  $q->three();\n  $u->four();\n  $m->five();\n  $s->six();\n  $many->seven();\n  $untyped->eight();\n}\n";
        assert_eq!(
            declared_receivers(source),
            [
                ("$a->one()".to_string(), "App\\Foo".to_string()),
                ("$r?->two()".to_string(), "Lib\\Renderer".to_string()),
                ("$q->three()".to_string(), "Ext\\Q".to_string()),
            ]
        );
    }

    #[test]
    fn a_parameter_the_body_assigns_loses_its_declared_type() {
        let cases = [
            "$a = new Bar();",
            "$a .= 'x';",
            "$a =& $other;",
            "[$a, $b] = pair();",
            "list($b, $a) = pair();",
            "foreach ($items as $a) {}",
            "foreach ($items as $k => $a) {}",
            "try {} catch (\\Exception $a) {}",
            "global $a;",
            "static $a;",
            "unset($a);",
            "$a++;",
            "$fn = function () use (&$a) { $a = null; };",
            "$$name = 1;",
            "extract($row);",
        ];
        for case in cases {
            let source = format!(
                "<?php\nfunction f(Foo $a, Foo $keep) {{\n  {case}\n  $a->m();\n  $keep->m();\n}}\n"
            );
            let declared = declared_receivers(&source);
            assert!(
                declared.iter().all(|(raw, _)| raw != "$a->m()"),
                "{case}: an assigned parameter must not be a declared receiver"
            );
            let keeps = declared.iter().any(|(raw, _)| raw == "$keep->m()");
            let poisons_all = case.contains("$$") || case.contains("extract");
            assert_eq!(
                keeps, !poisons_all,
                "{case}: an unassigned parameter keeps its type unless locals are written by name"
            );
        }
    }

    #[test]
    fn closures_see_their_own_parameters_and_arrow_functions_see_the_enclosing_scope() {
        let source = "<?php\nfunction f(Foo $a) {\n  $c = function (Bar $b) use ($a) { $b->one(); $a->two(); };\n  $d = fn(Baz $z) => $z->three() . $a->four();\n  $a->five();\n}\n";
        assert_eq!(
            declared_receivers(source),
            [
                ("$b->one()".to_string(), "Bar".to_string()),
                ("$z->three()".to_string(), "Baz".to_string()),
                ("$a->four()".to_string(), "Foo".to_string()),
                ("$a->five()".to_string(), "Foo".to_string()),
            ]
        );
    }

    #[test]
    fn a_typed_property_of_the_enclosing_class_is_a_declared_receiver_through_this() {
        let source = "<?php\nnamespace App;\nclass S {\n  public function early() { $this->late->one(); $this->promoted->two(); $this->plain->three(); $this->union->four(); $this->stat->five(); $this->inherited->six(); }\n  public static function inStatic() { $this->late->seven(); }\n  public function __construct(private Foo $promoted, Foo $notPromoted) { $notPromoted->eight(); }\n  private ?Bar $late;\n  private $plain;\n  private Foo|Bar $union;\n  private static Foo $stat;\n}\n";
        assert_eq!(
            declared_receivers(source),
            [
                ("$this->late->one()".to_string(), "App\\Bar".to_string()),
                ("$this->promoted->two()".to_string(), "App\\Foo".to_string()),
                ("$notPromoted->eight()".to_string(), "App\\Foo".to_string()),
            ]
        );
    }

    #[test]
    fn a_property_declared_by_a_trait_is_declared_inside_the_trait_only() {
        let source = "<?php\ntrait T {\n  private Foo $x;\n  public function inTrait() { $this->x->one(); }\n}\nclass C {\n  use T;\n  public function inClass() { $this->x->two(); }\n}\n";
        assert_eq!(
            declared_receivers(source),
            [("$this->x->one()".to_string(), "Foo".to_string())]
        );
    }

    #[test]
    fn static_methods_are_marked_by_index() {
        let source = "<?php\nclass A {\n  public static function s() {}\n  public function i() {}\n  final public static function t() {}\n}\n";
        let file = extract(source);
        let names: Vec<&str> = file
            .static_methods
            .iter()
            .map(|&i| file.defs[i].name.as_str())
            .collect();
        assert_eq!(names, ["s", "t"]);
    }

    #[test]
    fn a_name_outside_any_namespace_is_global() {
        let source = "<?php\nuse Lib\\Tool;\nfunction f() { Plain::a(); Tool::b(); }\n";
        assert_eq!(constant_receivers(source), ["Plain", "Lib\\Tool"]);
    }

    #[test]
    fn heritage_and_trait_targets_are_qualified_from_the_imports() {
        let source = "<?php\nnamespace A;\nuse B\\Base;\nclass C extends Base implements Iface, \\Ext\\Marker {\n  use Tr;\n}\n";
        let file = extract(source);
        let targets: Vec<&str> = file.heritage.iter().map(|h| h.target.as_str()).collect();
        assert_eq!(targets, ["B\\Base", "A\\Iface", "Ext\\Marker"]);
        let traits: Vec<&str> = file
            .trait_uses
            .iter()
            .map(|t| t.trait_name.as_str())
            .collect();
        assert_eq!(traits, ["A\\Tr"]);
    }

    #[test]
    fn each_namespace_block_has_its_own_imports() {
        let source = "<?php\nnamespace X {\n  use Q\\Z;\n  class C extends Z {}\n}\nnamespace Y {\n  class D extends Z {}\n}\n";
        let file = extract(source);
        let targets: Vec<&str> = file.heritage.iter().map(|h| h.target.as_str()).collect();
        assert_eq!(targets, ["Q\\Z", "Y\\Z"]);

        let source = "<?php\nnamespace X;\nuse Q\\Z;\nclass C extends Z {}\nnamespace Y;\nclass D extends Z {}\n";
        let file = extract(source);
        let targets: Vec<&str> = file.heritage.iter().map(|h| h.target.as_str()).collect();
        assert_eq!(targets, ["Q\\Z", "Y\\Z"]);
    }
}
