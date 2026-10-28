//! Tree-sitter C++ structure extraction.
//!
//! Owns parsing only. A function or method node is emitted when the source
//! has a body. A declaration without a body is only counted, so it cannot
//! later be mistaken for a callable target. Syntax mode records call
//! expressions and never emits a `calls` edge.
//!
//! Overload identity is the normalized declarator: parameter-type spelling
//! (nested function-pointer signatures and array bounds included),
//! template-parameter kinds, and cv/ref qualifiers. Parameter names and
//! default expressions are not part of the key. The same key under two
//! preprocessing branches stays two nodes; nothing pairs them by order.
//!
//! An out-of-line definition `A::f` is a method only when `A` names a type
//! and a function only when it names a namespace, looked up in the
//! [`ScopeIndex`] of every C++ file in the snapshot. When `A` is neither or
//! both, the definition is not emitted and is counted instead; the calls in
//! its body are still observed.
//!
//! Every fact describes the source as spelled (policy
//! [`crate::schema::structure_policy`]): the preprocessor is not run and no
//! macro is expanded, so `N::f` names `N` even where a macro, from any
//! header or the command line, would rename it. Macro-sensitive meaning is
//! the compilation profile's job. Within that contract, a definition inside
//! an `#if`/`#ifdef`/`#else` region never decides what another name means,
//! because only unconditional code is compiled in every configuration, and
//! an inheritance edge is withheld when a directive in the same file sits
//! between the base definition and its use.
//!
//! Names are spelled from the token stream, so whitespace inside a type
//! (`Vec< int >`) does not change a key; a literal is one token, spelled
//! exactly.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use tree_sitter::{Language, Node, Parser};

use crate::rust_syntax::{RawCall, RawDef, RawImport, ReceiverShape};
use crate::schema::{NodeKind, Span, TypeKind};

static CPP_LANGUAGE: LazyLock<Language> = LazyLock::new(|| tree_sitter_cpp::LANGUAGE.into());

/// A base class that names exactly one type body in this file.
#[derive(Debug, Clone)]
pub struct RawInheritLink {
    pub child: usize,
    pub base: usize,
    pub span: Span,
}

/// A body-less declaration of a function or type.
pub const OBSERVED_DECLARATION: &str = "declaration";
/// A base class this adapter does not resolve to a definition.
pub const OBSERVED_UNRESOLVED_BASE: &str = "unresolved_base";
/// An out-of-line definition whose qualifier is not one known type or
/// namespace.
pub const OBSERVED_UNRESOLVED_QUALIFIER: &str = "unresolved_qualifier";

/// Per-file C++ extraction output.
#[derive(Debug, Clone)]
pub struct RawFile {
    pub defs: Vec<RawDef>,
    pub imports: Vec<RawImport>,
    pub calls: Vec<RawCall>,
    pub inherits: Vec<RawInheritLink>,
    /// Constructs seen and not emitted, counted by kind.
    pub observations: BTreeMap<&'static str, usize>,
    pub parse_errors: usize,
    /// Indices into `defs` of definitions inside `#if`/`#ifdef`/`#else`
    /// regions, which may not be compiled.
    pub conditional_defs: BTreeSet<usize>,
    pub bodies: Vec<BodyAnchor>,
    pub owners: Vec<OwnerAnchor>,
    pub call_anchors: Vec<CallAnchor>,
}

/// Exact declarator witnesses; these do not establish compiled owners.
#[derive(Debug, Clone)]
pub struct BodyAnchor {
    pub span: Span,
    pub name_span: Span,
    pub qualifiers: Vec<Span>,
    pub global: bool,
    pub lexical_scope: Vec<String>,
    pub normalized_name: String,
    pub templated: bool,
    pub conditional: bool,
    pub parent: Option<usize>,
}

/// Exact source call shape; it does not resolve a target or caller.
#[derive(Debug, Clone)]
pub struct CallAnchor {
    pub span: Span,
    pub function_span: Span,
    pub name_span: Span,
    pub function_kind: String,
    pub name_kind: String,
    pub templated: bool,
    pub inside_lambda: bool,
    pub caller_span: Option<Span>,
    pub unevaluated: bool,
}

/// A named namespace or type body, linked to its exact spelled definition.
#[derive(Debug, Clone)]
pub struct OwnerAnchor {
    pub definition: usize,
    pub name_span: Span,
    pub templated: bool,
}

/// The namespace and type paths defined across a snapshot's C++ files.
///
/// Anonymous namespaces are distinct per file, so a path through one is
/// keyed by its file.
#[derive(Debug, Default)]
pub struct ScopeIndex {
    namespaces: BTreeSet<Vec<String>>,
    types: BTreeSet<Vec<String>>,
}

#[derive(Clone, Copy)]
enum Owner {
    Namespace,
    Type,
}

impl ScopeIndex {
    /// Index every namespace and type body in `files` outside preprocessor
    /// conditionals. A definition that may not be compiled cannot decide
    /// what a qualifier names.
    pub fn build(files: &[(String, Vec<u8>)]) -> Result<Self> {
        let mut index = Self::default();
        let empty = Self::default();
        for (rel_path, bytes) in files {
            let raw = extract_file(rel_path, bytes, &empty)
                .with_context(|| format!("indexing scopes of {rel_path}"))?;
            for (position, def) in raw.defs.into_iter().enumerate() {
                if raw.conditional_defs.contains(&position) {
                    continue;
                }
                let name = def.name.split('<').next().unwrap_or(&def.name);
                let mut path = def.scope.clone();
                path.push(name.to_string());
                let key = index_key(rel_path, path);
                match def.kind {
                    NodeKind::Module => index.namespaces.insert(key),
                    NodeKind::Type(_) => index.types.insert(key),
                    _ => false,
                };
            }
        }
        Ok(index)
    }

    /// Resolve `qualifiers` as written in `enclosing` (a namespace path).
    /// The first qualifier is looked up from the innermost namespace
    /// outward; the full path must then be exactly one of a namespace or a
    /// type.
    fn resolve(
        &self,
        rel_path: &str,
        enclosing: &[String],
        global: bool,
        qualifiers: &[String],
    ) -> Option<(Vec<String>, Owner)> {
        let first = qualifiers.first()?;
        let innermost = if global { 0 } else { enclosing.len() };
        for depth in (0..=innermost).rev() {
            let mut probe = enclosing[..depth].to_vec();
            probe.push(first.clone());
            let probe = index_key(rel_path, probe);
            if !self.namespaces.contains(&probe) && !self.types.contains(&probe) {
                continue;
            }
            let mut path = enclosing[..depth].to_vec();
            path.extend(qualifiers.iter().cloned());
            let key = index_key(rel_path, path.clone());
            return match (self.namespaces.contains(&key), self.types.contains(&key)) {
                (true, false) => Some((path, Owner::Namespace)),
                (false, true) => Some((path, Owner::Type)),
                _ => None,
            };
        }
        None
    }
}

fn index_key(rel_path: &str, mut path: Vec<String>) -> Vec<String> {
    if path.iter().any(|part| part.starts_with("anonymous@")) {
        path.insert(0, format!("file:{rel_path}"));
    }
    path
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Namespace,
    Type,
}

struct Frame {
    name: String,
    kind: FrameKind,
    def_index: usize,
}

/// An unqualified base of a non-template type declared directly in a
/// namespace. Only this shape can be resolved within one file.
struct PendingBase {
    child: usize,
    name: String,
    span: Span,
}

struct Walker<'a> {
    rel_path: &'a str,
    /// Start bytes of every preprocessor directive, in source order.
    directives: Vec<usize>,
    /// Depth of `#if`/`#ifdef`/`#else` regions around the current node.
    conditional_depth: usize,
    /// Definitions emitted inside a conditional region.
    conditional_defs: BTreeSet<usize>,
    /// Depth of bodies whose owner was not emitted. Nothing inside one is
    /// emitted either; its calls are still observed.
    omitted_owner: usize,
    source: &'a [u8],
    index: &'a ScopeIndex,
    defs: Vec<RawDef>,
    bodies: Vec<BodyAnchor>,
    owners: Vec<OwnerAnchor>,
    call_anchors: Vec<CallAnchor>,
    imports: Vec<RawImport>,
    calls: Vec<RawCall>,
    observations: BTreeMap<&'static str, usize>,
    pending_bases: Vec<PendingBase>,
    errors: usize,
}

/// Extract structural observations from one C++ source file. `index`
/// holds the snapshot's namespace and type paths for out-of-line
/// definitions.
pub fn extract_file(rel_path: &str, source: &[u8], index: &ScopeIndex) -> Result<RawFile> {
    let mut parser = Parser::new();
    parser
        .set_language(&CPP_LANGUAGE)
        .context("set tree-sitter-cpp language")?;
    let tree = parser
        .parse(source, None)
        .context("tree-sitter parse returned None")?;
    let mut walker = Walker {
        rel_path,
        directives: Vec::new(),
        conditional_depth: 0,
        conditional_defs: BTreeSet::new(),
        omitted_owner: 0,
        source,
        index,
        defs: Vec::new(),
        bodies: Vec::new(),
        owners: Vec::new(),
        call_anchors: Vec::new(),
        imports: Vec::new(),
        calls: Vec::new(),
        observations: BTreeMap::new(),
        pending_bases: Vec::new(),
        errors: 0,
    };
    let mut frames = Vec::new();
    walker.visit(tree.root_node(), &mut frames, &mut None);
    let inherits = walker.resolve_bases();
    Ok(RawFile {
        defs: walker.defs,
        imports: walker.imports,
        calls: walker.calls,
        inherits,
        observations: walker.observations,
        parse_errors: walker.errors,
        conditional_defs: walker.conditional_defs,
        bodies: walker.bodies,
        owners: walker.owners,
        call_anchors: walker.call_anchors,
    })
}

impl<'a> Walker<'a> {
    fn visit(&mut self, node: Node<'a>, frames: &mut Vec<Frame>, template: &mut Option<String>) {
        if node.kind() == "ERROR" {
            self.errors += 1;
        }
        if node.kind().starts_with("preproc_") {
            self.directives.push(node.start_byte());
        }
        match node.kind() {
            "preproc_if" | "preproc_ifdef" | "preproc_elif" | "preproc_elifdef"
            | "preproc_else" => {
                self.conditional_depth += 1;
                self.visit_children(node, frames, template);
                self.conditional_depth -= 1;
            }
            "namespace_definition" => self.visit_namespace(node, frames, template),
            "class_specifier" => self.visit_type(node, TypeKind::Class, frames, template),
            "struct_specifier" => self.visit_type(node, TypeKind::Struct, frames, template),
            "union_specifier" => self.visit_type(node, TypeKind::Union, frames, template),
            "enum_specifier" => self.visit_type(node, TypeKind::Enum, frames, template),
            "function_definition" => self.visit_function(node, frames, template),
            "template_declaration" => self.visit_template(node, frames, template),
            "lambda_expression" => self.visit_lambda(node, frames, template),
            "preproc_include" => self.visit_include(node),
            "call_expression" => {
                self.record_call(node);
                self.visit_children(node, frames, template);
            }
            "declaration" | "field_declaration" => {
                if self.note_declaration(node) {
                    return;
                }
                self.visit_children(node, frames, template);
            }
            _ => self.visit_children(node, frames, template),
        }
    }

    fn visit_children(
        &mut self,
        node: Node<'a>,
        frames: &mut Vec<Frame>,
        template: &mut Option<String>,
    ) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(child, frames, template);
        }
    }

    fn visit_namespace(
        &mut self,
        node: Node<'a>,
        frames: &mut Vec<Frame>,
        template: &mut Option<String>,
    ) {
        let names = node.child_by_field_name("name").map_or_else(
            || vec![format!("anonymous@{}", node.start_byte())],
            |name| self.namespace_names(name),
        );
        let name_nodes = node
            .child_by_field_name("name")
            .map(namespace_identifiers)
            .unwrap_or_default();
        let mut added = 0;
        for name in names {
            let index = self.emit(
                NodeKind::Module,
                name.clone(),
                scope_names(frames),
                span_of(node),
                frames.last().map(|frame| frame.def_index),
            );
            if let Some(name) = name_nodes.get(added) {
                self.owners.push(OwnerAnchor {
                    definition: index,
                    name_span: span_of(*name),
                    templated: false,
                });
            }
            frames.push(Frame {
                name,
                kind: FrameKind::Namespace,
                def_index: index,
            });
            added += 1;
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body, frames, template);
        }
        for _ in 0..added {
            frames.pop();
        }
    }

    fn visit_type(
        &mut self,
        node: Node<'a>,
        type_kind: TypeKind,
        frames: &mut Vec<Frame>,
        template: &mut Option<String>,
    ) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.visit_children(node, frames, template);
            return;
        };
        if node.child_by_field_name("body").is_none() {
            self.observe(OBSERVED_DECLARATION);
            return;
        }
        if self.omitted_owner > 0 {
            self.visit_children(node, frames, template);
            return;
        }
        let name = self.spelling(name_node);
        let shape = template.take();
        // A template parameter or an enclosing class can hide a base name,
        // so only a non-template type directly in a namespace resolves it.
        let resolvable = shape.is_none()
            && name_node.kind() == "type_identifier"
            && frames
                .iter()
                .all(|frame| frame.kind == FrameKind::Namespace);
        let shaped = shape.map_or_else(|| name.clone(), |shape| format!("{name}{shape}"));
        let index = self.emit(
            NodeKind::Type(type_kind),
            shaped,
            scope_names(frames),
            span_of(node),
            frames.last().map(|frame| frame.def_index),
        );
        self.owners.push(OwnerAnchor {
            definition: index,
            name_span: span_of(name_node),
            templated: has_template_parent(node) || name_node.kind() == "template_type",
        });
        self.record_bases(node, index, resolvable);
        frames.push(Frame {
            name,
            kind: FrameKind::Type,
            def_index: index,
        });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body, frames, template);
        }
        frames.pop();
    }

    fn visit_template(
        &mut self,
        node: Node<'a>,
        frames: &mut Vec<Frame>,
        template: &mut Option<String>,
    ) {
        let shape = node
            .child_by_field_name("parameters")
            .map(|parameters| self.template_shape(parameters));
        let saved = template.clone();
        *template = shape;
        self.visit_children(node, frames, template);
        *template = saved;
    }

    fn visit_function(
        &mut self,
        node: Node<'a>,
        frames: &mut Vec<Frame>,
        template: &mut Option<String>,
    ) {
        let Some(declarator) = node.child_by_field_name("declarator") else {
            self.visit_children(node, frames, template);
            return;
        };
        let Some(function) = find_kind(declarator, "function_declarator") else {
            self.visit_children(node, frames, template);
            return;
        };
        let Some(name_node) = function.child_by_field_name("declarator") else {
            return;
        };
        if self.omitted_owner > 0 {
            self.visit_omitted_body(node, frames);
            return;
        }
        let (global, qualifiers, name) = self.qualified_parts(name_node);
        self.observe_body(node, function, name_node, frames, template.as_deref());
        let in_type = frames.iter().any(|frame| frame.kind == FrameKind::Type);
        let (kind, scope) = if qualifiers.is_empty() && !global {
            let kind = if in_type {
                NodeKind::Method
            } else {
                NodeKind::Function
            };
            (kind, scope_names(frames))
        } else {
            let resolved = if in_type {
                None
            } else {
                self.index
                    .resolve(self.rel_path, &namespace_scope(frames), global, &qualifiers)
            };
            match resolved {
                Some((path, Owner::Type)) => (NodeKind::Method, path),
                Some((path, Owner::Namespace)) => (NodeKind::Function, path),
                None => {
                    self.observe(OBSERVED_UNRESOLVED_QUALIFIER);
                    self.visit_omitted_body(node, frames);
                    return;
                }
            }
        };
        let template = template.take().unwrap_or_default();
        let full_name = self.callable_name(&name, function, &template);
        let index = self.emit(
            kind,
            full_name,
            scope,
            span_of(node),
            frames.last().map(|frame| frame.def_index),
        );
        let saved = frames.len();
        frames.push(Frame {
            name,
            kind: FrameKind::Type,
            def_index: index,
        });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body, frames, &mut None);
        }
        frames.truncate(saved);
    }

    fn callable_name(&self, name: &str, function: Node<'a>, template: &str) -> String {
        let params = function.child_by_field_name("parameters").map_or_else(
            || "()".to_string(),
            |parameters| self.parameter_list(parameters),
        );
        format!("{name}{template}{params}{}", self.function_suffix(function))
    }

    fn observe_body(
        &mut self,
        node: Node<'a>,
        function: Node<'a>,
        name_node: Node<'a>,
        frames: &[Frame],
        template: Option<&str>,
    ) {
        let (global, qualifiers, identifier) = qualified_nodes(name_node);
        self.bodies.push(BodyAnchor {
            span: span_of(node),
            name_span: span_of(identifier),
            qualifiers: qualifiers.iter().map(|node| span_of(*node)).collect(),
            global,
            lexical_scope: scope_names(frames),
            normalized_name: self.callable_name(
                &self.spelling(identifier),
                function,
                template.unwrap_or_default(),
            ),
            templated: template.is_some()
                || has_template_parent(node)
                || qualifiers.iter().any(|node| node.kind() == "template_type")
                || identifier.kind() == "template_function",
            conditional: self.conditional_depth > 0,
            parent: frames.last().map(|frame| frame.def_index),
        });
    }

    /// Observe the calls in a body whose owner is not emitted.
    fn visit_omitted_body(&mut self, node: Node<'a>, frames: &mut Vec<Frame>) {
        if let Some(body) = node.child_by_field_name("body") {
            self.omitted_owner += 1;
            self.visit(body, frames, &mut None);
            self.omitted_owner -= 1;
        }
    }

    fn visit_lambda(
        &mut self,
        node: Node<'a>,
        frames: &mut Vec<Frame>,
        template: &mut Option<String>,
    ) {
        if self.omitted_owner > 0 {
            self.visit_children(node, frames, template);
            return;
        }
        let index = self.emit(
            NodeKind::Function,
            format!("lambda@{}", node.start_byte()),
            scope_names(frames),
            span_of(node),
            frames.last().map(|frame| frame.def_index),
        );
        let saved = frames.len();
        frames.push(Frame {
            name: format!("lambda@{}", node.start_byte()),
            kind: FrameKind::Type,
            def_index: index,
        });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body, frames, template);
        }
        frames.truncate(saved);
    }

    fn visit_include(&mut self, node: Node<'a>) {
        let Some(path) = node.child_by_field_name("path") else {
            return;
        };
        let raw = self.text(path);
        let path = if path.kind() == "string_literal" {
            raw.trim_matches('"').to_string()
        } else {
            raw
        };
        self.imports.push(RawImport {
            path,
            span: span_of(node),
        });
    }

    fn note_declaration(&mut self, node: Node<'a>) -> bool {
        if find_kind(node, "function_declarator").is_none()
            || find_kind(node, "function_definition").is_some()
        {
            return false;
        }
        self.observe(OBSERVED_DECLARATION);
        true
    }

    fn observe(&mut self, kind: &'static str) {
        *self.observations.entry(kind).or_default() += 1;
    }

    fn record_call(&mut self, node: Node<'a>) {
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        let target = callee_token(function);
        {
            let anchor_target = source_callee_name(function);
            let mut ancestor = node.parent();
            let mut inside_lambda = false;
            let mut caller_span = None;
            let mut unevaluated = matches!(self.text(function).as_str(), "typeid" | "noexcept");
            while let Some(parent) = ancestor {
                if parent.kind() == "call_expression"
                    && parent
                        .child_by_field_name("function")
                        .is_some_and(|callee| {
                            matches!(self.text(callee).as_str(), "typeid" | "noexcept")
                        })
                {
                    unevaluated = true;
                }
                if parent.kind() == "lambda_expression" {
                    inside_lambda = true;
                }
                if matches!(
                    parent.kind(),
                    "sizeof_expression"
                        | "alignof_expression"
                        | "decltype"
                        | "noexcept"
                        | "requires_expression"
                ) {
                    unevaluated = true;
                }
                if matches!(
                    parent.kind(),
                    "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier"
                ) {
                    break;
                }
                if parent.kind() == "function_definition" {
                    caller_span = Some(span_of(parent));
                    break;
                }
                ancestor = parent.parent();
            }
            self.call_anchors.push(CallAnchor {
                span: span_of(node),
                function_span: span_of(function),
                name_span: span_of(anchor_target),
                function_kind: function.kind().to_string(),
                name_kind: anchor_target.kind().to_string(),
                templated: has_template_parent(node)
                    || find_kind(function, "template_function").is_some()
                    || find_kind(function, "template_method").is_some()
                    || find_kind(function, "template_type").is_some(),
                inside_lambda,
                caller_span,
                unevaluated,
            });
        }
        self.calls.push(RawCall {
            raw_name: self.spelling(target),
            path_root: None,
            span: span_of(node),
            name_start_byte: target.start_byte(),
            receiver: ReceiverShape::Other,
            scope: Vec::new(),
        });
    }

    fn record_bases(&mut self, node: Node<'a>, child: usize, resolvable: bool) {
        let mut cursor = node.walk();
        for clause in node.named_children(&mut cursor) {
            if clause.kind() != "base_class_clause" {
                continue;
            }
            let mut bases = clause.walk();
            for base in clause.named_children(&mut bases) {
                if base.kind() == "access_specifier" {
                    continue;
                }
                // A qualified or template base needs lookup this adapter
                // does not do.
                if resolvable && base.kind() == "type_identifier" {
                    self.pending_bases.push(PendingBase {
                        child,
                        name: self.text(base),
                        span: span_of(base),
                    });
                } else {
                    self.observe(OBSERVED_UNRESOLVED_BASE);
                }
            }
        }
    }

    /// Link each pending base to the one type of that name defined earlier
    /// in the child's own namespace. Unqualified lookup from a namespace
    /// finds that definition before any other, provided the preprocessor
    /// cannot intervene: neither side sits in a conditional region and no
    /// directive (a `#define`, an `#include` that may define a macro) lies
    /// between the definition and the base name. Anything else stays
    /// counted.
    fn resolve_bases(&mut self) -> Vec<RawInheritLink> {
        let mut links = Vec::new();
        let pending = std::mem::take(&mut self.pending_bases);
        for base in pending {
            let child = &self.defs[base.child];
            let matches: Vec<usize> = self
                .defs
                .iter()
                .enumerate()
                .filter(|(_, def)| {
                    matches!(def.kind, NodeKind::Type(_))
                        && def.name == base.name
                        && def.scope == child.scope
                        && def.span.end_byte <= child.span.start_byte
                })
                .map(|(index, _)| index)
                .collect();
            let preprocessor_free = |index: usize| {
                let start = self.defs[index].span.start_byte;
                !self.conditional_defs.contains(&index)
                    && !self.conditional_defs.contains(&base.child)
                    && !self
                        .directives
                        .iter()
                        .any(|&directive| directive > start && directive < base.span.start_byte)
            };
            match matches.as_slice() {
                [only] if preprocessor_free(*only) => links.push(RawInheritLink {
                    child: base.child,
                    base: *only,
                    span: base.span,
                }),
                _ => self.observe(OBSERVED_UNRESOLVED_BASE),
            }
        }
        links
    }

    fn emit(
        &mut self,
        kind: NodeKind,
        name: String,
        scope: Vec<String>,
        span: Span,
        parent: Option<usize>,
    ) -> usize {
        let index = self.defs.len();
        if self.conditional_depth > 0 {
            self.conditional_defs.insert(index);
        }
        self.defs.push(RawDef {
            kind,
            name,
            scope,
            span,
            parent,
        });
        index
    }

    fn namespace_names(&self, node: Node<'a>) -> Vec<String> {
        let mut names = namespace_identifiers(node)
            .into_iter()
            .map(|name| self.text(name))
            .collect::<Vec<_>>();
        if names.is_empty() {
            names.push(format!("anonymous@{}", node.start_byte()));
        }
        names
    }

    /// A declarator name as `(global, qualifiers, name)`, read from the
    /// tree so template arguments containing `::` stay whole. A template
    /// qualifier contributes its template name.
    fn qualified_parts(&self, node: Node<'a>) -> (bool, Vec<String>, String) {
        let (global, scopes, name) = qualified_nodes(node);
        let qualifiers = scopes
            .into_iter()
            .map(|scope| {
                let named = if scope.kind() == "template_type" {
                    scope.child_by_field_name("name").unwrap_or(scope)
                } else {
                    scope
                };
                self.spelling(named)
            })
            .collect();
        (global, qualifiers, self.spelling(name))
    }

    fn template_shape(&self, parameters: Node<'a>) -> String {
        let mut kinds = Vec::new();
        let mut cursor = parameters.walk();
        for child in parameters.named_children(&mut cursor) {
            let kind = match child.kind() {
                "type_parameter_declaration" | "optional_type_parameter_declaration" => self
                    .text(child)
                    .split_whitespace()
                    .next()
                    .unwrap_or("typename")
                    .to_string(),
                "variadic_type_parameter_declaration" => "typename...".to_string(),
                "parameter_declaration" | "optional_parameter_declaration" => {
                    self.parameter_type(child)
                }
                "variadic_parameter_declaration" => "...".to_string(),
                _ => continue,
            };
            kinds.push(kind);
        }
        format!("<{}>", kinds.join(","))
    }

    fn parameter_list(&self, parameters: Node<'a>) -> String {
        let mut types = Vec::new();
        let mut cursor = parameters.walk();
        for child in parameters.named_children(&mut cursor) {
            match child.kind() {
                "parameter_declaration" | "optional_parameter_declaration" => {
                    types.push(self.parameter_type(child));
                }
                "variadic_parameter_declaration" => types.push("...".to_string()),
                _ => {}
            }
        }
        format!("({})", types.join(", "))
    }

    fn parameter_type(&self, node: Node<'a>) -> String {
        let mut parts = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "type_qualifier" {
                parts.push(self.text(child));
            }
        }
        if let Some(ty) = node.child_by_field_name("type") {
            parts.push(self.spelling(ty));
        }
        if let Some(declarator) = node.child_by_field_name("declarator") {
            let suffix = self.declarator_suffix(declarator);
            if !suffix.is_empty() {
                parts.push(suffix);
            }
        }
        parts.join(" ")
    }

    /// The type-bearing shape of a parameter declarator, without its name:
    /// pointers, references, array bounds, and nested function signatures.
    fn declarator_suffix(&self, node: Node<'a>) -> String {
        let inner =
            inner_declarator(node).map_or_else(String::new, |child| self.declarator_suffix(child));
        match node.kind() {
            "pointer_declarator" | "abstract_pointer_declarator" => {
                format!("*{}{inner}", qualifier_text(self.source, node))
            }
            "reference_declarator" | "abstract_reference_declarator" => {
                let sigil = if self.text(node).trim_start().starts_with("&&") {
                    "&&"
                } else {
                    "&"
                };
                format!("{sigil}{inner}")
            }
            "array_declarator" | "abstract_array_declarator" => {
                let size = node
                    .child_by_field_name("size")
                    .map(|size| self.spelling(size))
                    .unwrap_or_default();
                format!("{inner}[{size}]")
            }
            "parenthesized_declarator" | "abstract_parenthesized_declarator" => {
                if inner.is_empty() {
                    inner
                } else {
                    format!("({inner})")
                }
            }
            "function_declarator" | "abstract_function_declarator" => {
                let params = node.child_by_field_name("parameters").map_or_else(
                    || "()".to_string(),
                    |parameters| self.parameter_list(parameters),
                );
                let mut suffix = self.function_suffix(node);
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "noexcept" {
                        suffix.push(' ');
                        suffix.push_str(&self.spelling(child));
                    }
                }
                format!("{inner}{params}{suffix}")
            }
            _ => String::new(),
        }
    }

    fn function_suffix(&self, declarator: Node<'a>) -> String {
        let mut suffix = String::new();
        let mut cursor = declarator.walk();
        for child in declarator.named_children(&mut cursor) {
            if matches!(child.kind(), "type_qualifier" | "ref_qualifier") {
                suffix.push(' ');
                suffix.push_str(&self.spelling(child));
            }
        }
        suffix
    }

    /// The node's tokens joined with a space only where the two would
    /// otherwise lex as something else: two words (`unsigned int`), a word
    /// and a literal (`u8 "x"`), or two punctuators that form a longer one
    /// (`- -` is not `--`). Layout never reaches a key.
    fn spelling(&self, node: Node<'a>) -> String {
        let mut tokens = Vec::new();
        collect_tokens(node, &mut tokens);
        let mut spelled = String::new();
        for token in tokens {
            let text = token.utf8_text(self.source).unwrap_or_default();
            match (spelled.chars().last(), text.chars().next()) {
                (Some(last), Some(first)) if tokens_fuse(last, first) => spelled.push(' '),
                _ => {}
            }
            spelled.push_str(text);
        }
        spelled
    }

    fn text(&self, node: Node<'a>) -> String {
        node.utf8_text(self.source).unwrap_or_default().to_string()
    }
}

/// Literal kinds that are one token: their inner whitespace is content.
const LITERAL_KINDS: &[&str] = &[
    "string_literal",
    "raw_string_literal",
    "char_literal",
    "user_defined_literal",
];

fn has_template_parent(mut node: Node<'_>) -> bool {
    while let Some(parent) = node.parent() {
        if parent.kind() == "template_declaration" {
            return true;
        }
        node = parent;
    }
    false
}

fn namespace_identifiers(node: Node<'_>) -> Vec<Node<'_>> {
    if node.kind() == "namespace_identifier" {
        return vec![node];
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() == "namespace_identifier")
        .collect()
}

fn qualified_nodes(node: Node<'_>) -> (bool, Vec<Node<'_>>, Node<'_>) {
    let mut qualifiers = Vec::new();
    let mut global = false;
    let mut current = node;
    while current.kind() == "qualified_identifier" {
        match current.child_by_field_name("scope") {
            Some(scope) => qualifiers.push(scope),
            None if qualifiers.is_empty() => global = true,
            None => {}
        }
        let Some(name) = current.child_by_field_name("name") else {
            break;
        };
        current = name;
    }
    (global, qualifiers, current)
}

fn collect_tokens<'a>(node: Node<'a>, tokens: &mut Vec<Node<'a>>) {
    if node.kind() == "comment" {
        return;
    }
    if node.child_count() == 0 || LITERAL_KINDS.contains(&node.kind()) {
        tokens.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_tokens(child, tokens);
    }
}

const fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii()
}

/// Adjacent character pairs inside a C++ punctuator, digraph, or comment
/// opener. Two tokens meeting at one of these pairs would lex differently
/// without a space.
const PUNCTUATOR_PAIRS: &[&str] = &[
    "::", "->", ">*", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "=>", "&&", "||", "+=", "-=",
    "*=", "/=", "%=", "&=", "|=", "^=", ".*", "..", "##", "<:", ":>", "<%", "%>", "%:", ":%", "//",
    "/*",
];

fn tokens_fuse(last: char, first: char) -> bool {
    if is_word_char(last) {
        return is_word_char(first) || first == '"' || first == '\'';
    }
    let mut pair = String::with_capacity(2);
    pair.push(last);
    pair.push(first);
    PUNCTUATOR_PAIRS.contains(&pair.as_str())
}

fn scope_names(frames: &[Frame]) -> Vec<String> {
    frames.iter().map(|frame| frame.name.clone()).collect()
}

fn namespace_scope(frames: &[Frame]) -> Vec<String> {
    frames
        .iter()
        .filter(|frame| frame.kind == FrameKind::Namespace)
        .map(|frame| frame.name.clone())
        .collect()
}

/// The declarator a pointer, reference, array, parenthesized, or function
/// declarator wraps. Some wrappers name it as a field and some do not.
fn inner_declarator(node: Node<'_>) -> Option<Node<'_>> {
    if let Some(inner) = node.child_by_field_name("declarator") {
        return Some(inner);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind().ends_with("declarator"))
}

fn qualifier_text(source: &[u8], node: Node<'_>) -> String {
    let mut cursor = node.walk();
    let mut text = String::new();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "type_qualifier" {
            text.push(' ');
            text.push_str(child.utf8_text(source).unwrap_or_default());
        }
    }
    text
}

fn find_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    if node.kind() == kind {
        return Some(node);
    }
    if matches!(
        node.kind(),
        "class_specifier"
            | "struct_specifier"
            | "union_specifier"
            | "enum_specifier"
            | "function_definition"
            | "lambda_expression"
    ) {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = find_kind(child, kind) {
            return Some(found);
        }
    }
    None
}

fn source_callee_name(node: Node<'_>) -> Node<'_> {
    let name = match node.kind() {
        "template_function" | "template_method" | "qualified_identifier" => {
            node.child_by_field_name("name")
        }
        "field_expression" => node.child_by_field_name("field"),
        _ => None,
    };
    name.map_or(node, source_callee_name)
}

fn callee_token(node: Node<'_>) -> Node<'_> {
    match node.kind() {
        "field_expression" => node.child_by_field_name("field").unwrap_or(node),
        "qualified_identifier" | "template_function" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor).last().unwrap_or(node)
        }
        _ => node,
    }
}

fn span_of(node: Node<'_>) -> Span {
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

    /// Extract one file against an index of that file alone.
    fn extract(rel_path: &str, source: &[u8]) -> RawFile {
        let files = [(rel_path.to_string(), source.to_vec())];
        let index = ScopeIndex::build(&files).expect("index");
        extract_file(rel_path, source, &index).expect("extract")
    }

    fn names(source: &str) -> Vec<String> {
        let file = extract("sample.cpp", source.as_bytes());
        assert_eq!(file.parse_errors, 0, "parse errors in {source}");
        file.defs
            .iter()
            .map(|def| {
                let scope = if def.scope.is_empty() {
                    String::new()
                } else {
                    format!("{}::", def.scope.join("::"))
                };
                format!("{scope}{}", def.name)
            })
            .collect()
    }

    fn anchored_text<'a>(source: &'a str, span: &Span) -> &'a str {
        &source[span.start_byte..span.end_byte]
    }

    #[test]
    fn omitted_guarded_body_retains_exact_declarator_and_lexical_anchors() {
        let header = "#ifndef GUARD\n#define GUARD\nnamespace api { struct Widget { int run(int value) const; }; }\n#endif\n";
        let source =
            "namespace api {\nstd::string Widget::run(int value) const { return {}; }\n}\n";
        let files = [
            ("guarded.hpp".to_string(), header.as_bytes().to_vec()),
            ("unit.cpp".to_string(), source.as_bytes().to_vec()),
        ];
        let index = ScopeIndex::build(&files).expect("spelled index");
        let raw = extract_file("unit.cpp", source.as_bytes(), &index).expect("extract");
        assert_eq!(raw.parse_errors, 0);
        assert_eq!(raw.observations[OBSERVED_UNRESOLVED_QUALIFIER], 1);
        assert!(!raw.defs.iter().any(|def| def.kind == NodeKind::Method));
        assert_eq!(raw.bodies.len(), 1);
        let body = &raw.bodies[0];
        assert_eq!(anchored_text(source, &body.name_span), "run");
        assert_eq!(body.qualifiers.len(), 1);
        assert_eq!(anchored_text(source, &body.qualifiers[0]), "Widget");
        assert_eq!(body.lexical_scope, ["api"]);
        assert_eq!(body.normalized_name, "run(int) const");
        assert_eq!(body.span.start_line, 2);
        assert_eq!(
            anchored_text(source, &body.span),
            source.lines().nth(1).unwrap()
        );
        assert!(!body.global && !body.templated && !body.conditional);
    }

    #[test]
    fn declarator_anchors_preserve_global_nested_template_and_conditional_shapes() {
        let source = "int ::api::Widget::run() { return 1; }\nint outer::inner::pick(int value) { return value; }\ntemplate <typename T> T box<T>::get() { return {}; }\n#if ENABLE\nint hidden::run() { return 2; }\n#endif\nint declared();\n";
        let raw = extract("unit.cpp", source.as_bytes());
        assert_eq!(raw.parse_errors, 0);
        assert_eq!(raw.bodies.len(), 4, "prototypes are not body witnesses");
        let scopes = raw
            .bodies
            .iter()
            .map(|body| {
                body.qualifiers
                    .iter()
                    .map(|span| anchored_text(source, span))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            scopes,
            [
                vec!["api", "Widget"],
                vec!["outer", "inner"],
                vec!["box<T>"],
                vec!["hidden"]
            ]
        );
        assert!(raw.bodies[0].global);
        assert!(!raw.bodies[1].global);
        assert!(raw.bodies[2].templated);
        assert_eq!(raw.bodies[2].normalized_name, "get<typename>()");
        assert!(raw.bodies[3].conditional);
        assert_eq!(raw.observations[OBSERVED_DECLARATION], 1);
    }

    #[test]
    fn body_witnesses_share_existing_overload_normalization() {
        let source = "namespace api {\nint pick(int value) { return value; }\nint pick(int renamed, int scale) { return renamed * scale; }\nstruct Item { int get() const & { return 1; } };\n}\n";
        let raw = extract("unit.cpp", source.as_bytes());
        assert_eq!(raw.parse_errors, 0);
        assert_eq!(raw.bodies.len(), 3);
        for body in &raw.bodies {
            let matches = raw
                .defs
                .iter()
                .filter(|def| {
                    def.span == body.span
                        && matches!(def.kind, NodeKind::Function | NodeKind::Method)
                })
                .collect::<Vec<_>>();
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].name, body.normalized_name);
            assert_eq!(matches[0].scope, body.lexical_scope);
        }
        assert_ne!(raw.bodies[0].normalized_name, raw.bodies[1].normalized_name);
    }

    #[test]
    fn inline_body_inside_a_template_class_retains_its_unsupported_context() {
        let source = "template <typename T> struct Box { int get() { return 1; } };";
        let raw = extract("unit.hpp", source.as_bytes());
        assert_eq!(raw.parse_errors, 0);
        assert_eq!(raw.bodies.len(), 1);
        assert!(raw.bodies[0].templated);
        assert_eq!(anchored_text(source, &raw.bodies[0].name_span), "get");
        assert_eq!(raw.bodies[0].lexical_scope, ["Box"]);
    }

    #[test]
    fn owner_anchors_exclude_aliases_forwards_and_names_inside_a_body() {
        let source = "namespace api {\nstruct Widget;\nstruct Widget { int member; };\nusing Alias = Widget;\n}\nnamespace Shortcut = api;\nnamespace outer::inner { struct Item {}; }\n";
        let raw = extract("unit.hpp", source.as_bytes());
        assert_eq!(raw.parse_errors, 0);
        let names = raw
            .owners
            .iter()
            .map(|owner| {
                let def = &raw.defs[owner.definition];
                let name = anchored_text(source, &owner.name_span);
                assert_eq!(name, def.name);
                (name, def.scope.clone())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                ("api", vec![]),
                ("Widget", vec!["api".to_string()]),
                ("outer", vec![]),
                ("inner", vec!["outer".to_string()]),
                ("Item", vec!["outer".to_string(), "inner".to_string()])
            ]
        );
        assert_eq!(raw.owners[1].name_span.start_line, 3);
    }

    #[test]
    fn overloads_keep_distinct_keys_when_reordered_or_edited_above() {
        let first = names(
            "namespace ops {\nint pick(int value) { return value; }\nint pick(int value, int scale) { return value * scale; }\n}\n",
        );
        let second = names(
            "namespace ops {\n// café\nint pick(int value, int scale) { return value * scale; }\nint pick(int value) { return value; }\n}\n",
        );
        assert!(first.contains(&"ops::pick(int)".to_string()));
        assert!(first.contains(&"ops::pick(int, int)".to_string()));
        let mut first_picks: Vec<_> = first
            .into_iter()
            .filter(|name| name.contains("pick("))
            .collect();
        let mut second_picks: Vec<_> = second
            .into_iter()
            .filter(|name| name.contains("pick("))
            .collect();
        first_picks.sort();
        second_picks.sort();
        assert_eq!(first_picks, second_picks);
    }

    #[test]
    fn methods_templates_headers_and_declarations_stay_distinct() {
        let file = extract("sample.hpp", b"namespace ops {\nstruct Base { virtual int id() const; int concrete() const { return 1; } };\nstruct Child : Base { int id() const override { return 2; } };\nint Child::other() const & { return 3; }\ntemplate <typename T>\nT identity(T value) { return value; }\nint declared(int value);\ninline int header_pick() { return 1; }\n}\n");
        let keys: Vec<_> = file
            .defs
            .iter()
            .map(|def| format!("{}::{}", def.scope.join("::"), def.name))
            .collect();
        assert!(keys.iter().any(|key| key == "ops::Base::concrete() const"));
        assert!(keys.iter().any(|key| key == "ops::Child::id() const"));
        assert!(keys.iter().any(|key| key.contains("other() const &")));
        assert!(keys.iter().any(|key| key.contains("identity<typename>(T)")));
        assert!(keys.iter().any(|key| key.contains("header_pick()")));
        assert!(!keys.iter().any(|key| key.contains("declared")));
        assert!(keys.iter().any(|key| key == "ops::Child::other() const &"));
        // `virtual int id() const;` and `int declared(int);`
        assert_eq!(file.observations.get(OBSERVED_DECLARATION), Some(&2));
        assert_eq!(file.inherits.len(), 1);
        assert!(file.imports.is_empty());
    }

    #[test]
    fn includes_are_literal_and_calls_are_observations() {
        let file = extract(
            "caller.cpp",
            b"#include \"ops.hpp\"\n#include <vector>\nint run() {\n  return pick(1);\n}\n",
        );
        let paths: Vec<_> = file
            .imports
            .iter()
            .map(|import| import.path.as_str())
            .collect();
        assert_eq!(paths, ["ops.hpp", "<vector>"]);
        assert_eq!(file.calls.len(), 1);
        assert_eq!(file.calls[0].raw_name, "pick");
    }

    #[test]
    fn a_malformed_file_is_reported_and_a_lambda_is_local() {
        let broken = extract("broken.cpp", b"int {{{");
        assert!(broken.parse_errors > 0);
        let file = extract(
            "lambda.cpp",
            b"int run() {\n  auto value = [] { return 1; };\n  return value();\n}\n",
        );
        assert!(file.defs.iter().any(|def| def.name.starts_with("lambda@")));
        let lambda = file
            .defs
            .iter()
            .find(|def| def.name.starts_with("lambda@"))
            .expect("lambda");
        assert!(lambda.parent.is_some());
    }

    #[test]
    fn unicode_before_a_definition_does_not_change_its_key() {
        let prefixed = "int pick(int value) { return value; }\n";
        let marked = format!("// café\n{prefixed}");
        let plain = names(prefixed);
        let shifted = names(&marked);
        assert_eq!(plain, shifted);
        let file = extract("utf.cpp", marked.as_bytes());
        let def = file
            .defs
            .iter()
            .find(|def| def.name == "pick(int)")
            .expect("pick");
        let expected = marked.find("int pick").expect("source offset");
        assert_eq!(def.span.start_byte, expected);
    }

    fn keys(file: &RawFile) -> Vec<String> {
        file.defs
            .iter()
            .map(|def| {
                let mut path = def.scope.clone();
                path.push(def.name.clone());
                path.join("::")
            })
            .collect()
    }

    #[test]
    fn only_an_unqualified_base_earlier_in_the_same_namespace_is_linked() {
        let linked = extract(
            "ok.cpp",
            b"namespace N {\nstruct Base {};\nstruct Child : Base {};\n}\n",
        );
        assert_eq!(linked.inherits.len(), 1);
        assert_eq!(keys(&linked)[linked.inherits[0].base], "N::Base");

        for source in [
            // `A::Base` is not `B::Base`.
            "namespace B { struct Base {}; }\nnamespace N { struct Child : A::Base {}; }\n",
            // The template parameter hides `::Base`.
            "struct Base {};\ntemplate <class Base> struct Child : Base {};\n",
            // At `Child`, `Base` is `::Base`, not the later `N::Base`.
            "struct Base {};\nnamespace N {\nstruct Child : Base {};\nstruct Base {};\n}\n",
            // A nested class sees its enclosing class's members first.
            "struct Base {};\nstruct Outer {\nstruct Child : Base {};\n};\n",
        ] {
            let file = extract("base.cpp", source.as_bytes());
            assert!(file.inherits.is_empty(), "{source}");
            assert_eq!(
                file.observations.get(OBSERVED_UNRESOLVED_BASE),
                Some(&1),
                "{source}"
            );
        }
    }

    #[test]
    fn nested_function_signatures_and_array_bounds_separate_overloads() {
        let file = extract(
            "overloads.cpp",
            b"int select(int (*f)(int)) { return f(1); }\n\
              int select(int (*f)(double)) { return f(1.0); }\n\
              int select(int (*)(int) noexcept) { return 0; }\n\
              int size(int (&a)[3]) { return 3; }\n\
              int size(int (&a)[4]) { return 4; }\n\
              int size(int *a[4]) { return 0; }\n",
        );
        let keys = keys(&file);
        for expected in [
            "select(int (*)(int))",
            "select(int (*)(double))",
            "select(int (*)(int) noexcept)",
            "size(int (&)[3])",
            "size(int (&)[4])",
            "size(int *[4])",
        ] {
            assert!(
                keys.iter().any(|key| key == expected),
                "{expected} in {keys:?}"
            );
        }
        let distinct: BTreeSet<_> = keys.iter().collect();
        assert_eq!(distinct.len(), keys.len(), "{keys:?}");
    }

    #[test]
    fn an_out_of_line_definition_takes_its_kind_and_scope_from_the_qualifier() {
        let files = [
            (
                "include/widget.hpp".to_string(),
                b"struct Widget { int draw() const; };\n".to_vec(),
            ),
            (
                "src/widget.cpp".to_string(),
                b"int Widget::draw() const { return 1; }\n\
                  struct util {};\n\
                  namespace other { namespace util { int helper(); } }\n\
                  int other::util::helper() { return 2; }\n\
                  int Ghost::run() { return 3; }\n"
                    .to_vec(),
            ),
        ];
        let index = ScopeIndex::build(&files).expect("index");
        let file = extract_file("src/widget.cpp", &files[1].1, &index).expect("extract");
        let callables: Vec<_> = file
            .defs
            .iter()
            .zip(keys(&file))
            .filter(|(def, _)| matches!(def.kind, NodeKind::Function | NodeKind::Method))
            .map(|(def, key)| (def.kind, key))
            .collect();
        assert_eq!(
            callables,
            [
                (NodeKind::Method, "Widget::draw() const".to_string()),
                (NodeKind::Function, "other::util::helper()".to_string()),
            ]
        );
        assert_eq!(
            file.observations.get(OBSERVED_UNRESOLVED_QUALIFIER),
            Some(&1),
            "Ghost is neither a type nor a namespace"
        );
    }

    #[test]
    fn a_directive_or_conditional_between_base_and_use_withholds_the_edge() {
        for source in [
            // After the define, `Base` in the base clause means `Other`.
            "struct Base {};\n#define Base Other\nstruct Other {};\nstruct Child : Base {};\n",
            // An included header may define `Base` as a macro.
            "struct Base {};\n#include \"rename.hpp\"\nstruct Child : Base {};\n",
            // The only definition may not be compiled at all.
            "#if 0\nstruct Base {};\n#endif\nstruct Child : Base {};\n",
            // Nor the child.
            "struct Base {};\n#ifdef FEATURE\nstruct Child : Base {};\n#endif\n",
        ] {
            let file = extract("pp.cpp", source.as_bytes());
            assert!(file.inherits.is_empty(), "{source}");
            assert_eq!(
                file.observations.get(OBSERVED_UNRESOLVED_BASE),
                Some(&1),
                "{source}"
            );
        }
        let before = extract(
            "pp.cpp",
            b"#include \"common.hpp\"\n#define EXPORT\nstruct Base {};\nstruct Child : Base {};\n",
        );
        assert_eq!(before.inherits.len(), 1, "directives before both are fine");
    }

    #[test]
    fn whitespace_inside_a_type_does_not_change_a_key() {
        let tight =
            names("int f(Vec<Map<int, char>> v) { return 0; }\nint operator()() { return 0; }\n");
        let loose = names(
            "int f( Vec< Map< int ,char > > v ) { return 0; }\nint operator ( ) ( ) { return 0; }\n",
        );
        assert_eq!(tight, loose);
        assert!(
            tight.contains(&"f(Vec<Map<int,char> >)".to_string()),
            "{tight:?}"
        );
        let words = names("int g(unsigned  long /* width */ int x) { return 0; }\n");
        assert_eq!(words, ["g(unsigned long int)"]);
    }

    #[test]
    fn tokens_that_would_lex_differently_keep_their_boundary() {
        let keys = names(
            "int i = 0;\n\
             int f(decltype(- -i) v) { return 0; }\n\
             int f(decltype(--i) v) { return 1; }\n\
             int f(decltype(i - -1) v) { return 2; }\n\
             int f(decltype(i--) v) { return 3; }\n",
        );
        let distinct: BTreeSet<_> = keys.iter().collect();
        assert_eq!(distinct.len(), keys.len(), "{keys:?}");
        assert!(keys.contains(&"f(decltype(- -i))".to_string()), "{keys:?}");
        assert!(keys.contains(&"f(decltype(--i))".to_string()), "{keys:?}");
    }

    #[test]
    fn a_literal_is_spelled_exactly() {
        let keys = names(
            "int f(decltype(\"a \") v) { return 0; }\n\
             int f(decltype(\"a\") v) { return 1; }\n\
             int f(decltype(' ') v) { return 2; }\n",
        );
        assert_eq!(
            keys,
            [
                "f(decltype(\"a \"))",
                "f(decltype(\"a\"))",
                "f(decltype(' '))"
            ]
        );
    }

    /// Kind of each emitted callable, keyed by its spelled path.
    fn callables(file: &RawFile) -> Vec<(NodeKind, String)> {
        file.defs
            .iter()
            .zip(keys(file))
            .filter(|(def, _)| matches!(def.kind, NodeKind::Function | NodeKind::Method))
            .map(|(def, key)| (def.kind, key))
            .collect()
    }

    #[test]
    fn an_owner_is_resolved_as_spelled_and_macros_are_not_expanded() {
        // The spelled-source contract: `N` is the namespace written here.
        // Clang, after `#define N Widget` in `rename.inc`, compiles
        // `Widget::f`; syntax mode does not see that, by design.
        let files = [
            (
                "src/unit.cpp".to_string(),
                b"#include \"rename.inc\"\nnamespace N { int f(); }\nstruct Widget { int f(); };\nint N::f() { return 1; }\n"
                    .to_vec(),
            ),
        ];
        let index = ScopeIndex::build(&files).expect("index");
        let file = extract_file("src/unit.cpp", &files[0].1, &index).expect("extract");
        assert_eq!(
            callables(&file),
            [(NodeKind::Function, "N::f()".to_string())]
        );

        // A macro in an unrelated translation unit changes nothing.
        let files = [
            ("src/a.cpp".to_string(), b"#define N other\n".to_vec()),
            (
                "src/b.cpp".to_string(),
                b"namespace N { int f(); }\nint N::f() { return 1; }\n".to_vec(),
            ),
        ];
        let index = ScopeIndex::build(&files).expect("index");
        let file = extract_file("src/b.cpp", &files[1].1, &index).expect("extract");
        assert_eq!(
            callables(&file),
            [(NodeKind::Function, "N::f()".to_string())]
        );
        assert!(
            !file
                .observations
                .contains_key(OBSERVED_UNRESOLVED_QUALIFIER),
            "{:?}",
            file.observations
        );
    }

    #[test]
    fn a_type_in_a_conditional_cannot_decide_an_out_of_line_owner() {
        let file = extract(
            "cond.cpp",
            b"#if 0\nstruct N { int f(); };\n#endif\nnamespace N { int f(); }\nint N::f() { return 1; }\n",
        );
        let callables: Vec<_> = file
            .defs
            .iter()
            .zip(keys(&file))
            .filter(|(def, _)| matches!(def.kind, NodeKind::Function | NodeKind::Method))
            .map(|(def, key)| (def.kind, key))
            .collect();
        assert_eq!(callables, [(NodeKind::Function, "N::f()".to_string())]);
    }

    #[test]
    fn an_unowned_body_still_reports_its_calls_and_emits_nothing_inside() {
        let file = extract(
            "ghost.cpp",
            b"int Ghost::run() {\n  auto inner = [] { return target(); };\n  struct Local { int m() { return other(); } };\n  return inner();\n}\n",
        );
        assert!(file.defs.is_empty(), "{:?}", keys(&file));
        let calls: Vec<_> = file
            .calls
            .iter()
            .map(|call| call.raw_name.as_str())
            .collect();
        assert_eq!(calls, ["target", "other", "inner"]);
        assert_eq!(
            file.observations.get(OBSERVED_UNRESOLVED_QUALIFIER),
            Some(&1)
        );
    }
}
