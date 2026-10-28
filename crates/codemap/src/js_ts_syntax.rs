//! Source-only JS/TS structure and exact parser witnesses for later admission.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use tree_sitter::{Language, Node, Parser, Tree};

use crate::rust_syntax::{RawCall, RawDef, RawImport, ReceiverShape};
use crate::schema::{NodeKind, Span, TypeKind};

static JAVASCRIPT: LazyLock<Language> = LazyLock::new(|| tree_sitter_javascript::LANGUAGE.into());
static TYPESCRIPT: LazyLock<Language> =
    LazyLock::new(|| tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into());
static TSX: LazyLock<Language> = LazyLock::new(|| tree_sitter_typescript::LANGUAGE_TSX.into());

#[derive(Debug, Clone)]
pub struct BodyAnchor {
    pub definition: usize,
    pub callable: Span,
    pub name: Option<Span>,
    pub body: Span,
    pub compiler_callable: Span,
}

#[derive(Debug, Clone)]
pub struct CallAnchor {
    pub span: Span,
    pub callee: Span,
    pub callee_name: Span,
    pub caller_body: Option<Span>,
    pub constructor: bool,
    pub tagged: bool,
}

#[derive(Debug, Clone)]
pub struct RawFile {
    pub defs: Vec<RawDef>,
    pub imports: Vec<RawImport>,
    pub calls: Vec<RawCall>,
    pub bodies: Vec<BodyAnchor>,
    pub call_anchors: Vec<CallAnchor>,
    pub jsx_names: Vec<Span>,
    pub observations: BTreeMap<&'static str, usize>,
    pub parse_errors: usize,
    pub(crate) tree: Tree,
}

pub fn extract_file(rel_path: &str, source: &[u8]) -> Result<RawFile> {
    std::str::from_utf8(source).context("JS/TS source must be valid UTF-8")?;
    let extension = rel_path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let language = match extension.as_str() {
        "ts" | "mts" | "cts" => &TYPESCRIPT,
        "tsx" => &TSX,
        _ => &JAVASCRIPT,
    };
    let mut parser = Parser::new();
    parser.set_language(language).context("set JS/TS grammar")?;
    let tree = parser.parse(source, None).context("parse JS/TS source")?;
    let mut walker = Walker {
        source,
        output: RawFile {
            defs: Vec::new(),
            imports: Vec::new(),
            calls: Vec::new(),
            bodies: Vec::new(),
            call_anchors: Vec::new(),
            jsx_names: Vec::new(),
            observations: BTreeMap::new(),
            parse_errors: 0,
            tree: tree.clone(),
        },
    };
    walker.visit(tree.root_node(), &ContextState::default());
    Ok(walker.output)
}

#[derive(Default, Clone)]
struct ContextState {
    parent: Option<usize>,
    scope: Vec<String>,
    caller_body: Option<Span>,
    declaration_only: bool,
}

struct Walker<'a> {
    source: &'a [u8],
    output: RawFile,
}

fn span(node: Node) -> Span {
    Span::from_ts(
        node.start_byte(),
        node.end_byte(),
        (node.start_position().row, node.start_position().column),
        (node.end_position().row, node.end_position().column),
    )
}

/// Tree-sitter may attach trailing extra comments after a closing token.
/// The executable witness ends at the final parsed non-extra token.
pub(crate) fn executable_span(node: Node) -> Span {
    let mut last = node;
    loop {
        let next = (0..last.child_count())
            .rev()
            .filter_map(|index| last.child(index))
            .find(|child| !child.is_extra() && !child.is_missing());
        let Some(next) = next else {
            break;
        };
        last = next;
    }
    Span::from_ts(
        node.start_byte(),
        last.end_byte(),
        (node.start_position().row, node.start_position().column),
        (last.end_position().row, last.end_position().column),
    )
}

fn callable(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "arrow_function"
            | "method_definition"
    )
}

fn expression_owner(mut node: Node) -> Option<Node> {
    while let Some(parent) = node.parent() {
        if matches!(
            parent.kind(),
            "parenthesized_expression"
                | "as_expression"
                | "satisfies_expression"
                | "type_assertion"
                | "non_null_expression"
        ) {
            node = parent;
        } else {
            return Some(parent);
        }
    }
    None
}

pub(crate) fn compiler_span(node: Node) -> Span {
    let mut range = executable_span(node);
    if matches!(
        node.kind(),
        "function_declaration" | "generator_function_declaration" | "function_signature"
    ) {
        if let Some(parent) = node
            .parent()
            .filter(|parent| parent.kind() == "export_statement")
        {
            range.start_byte = parent.start_byte();
            range.start_line = parent.start_position().row as u32 + 1;
            range.start_col = parent.start_position().column as u32;
        }
    }
    range
}

fn default_export(node: Node) -> bool {
    expression_owner(node).is_some_and(|parent| {
        let mut cursor = parent.walk();
        parent.kind() == "export_statement"
            && parent
                .children(&mut cursor)
                .any(|child| child.kind() == "default")
    })
}

impl Walker<'_> {
    fn text(&self, node: Node) -> String {
        node.utf8_text(self.source)
            .expect("validated UTF-8")
            .to_owned()
    }

    fn observe(&mut self, kind: &'static str) {
        *self.output.observations.entry(kind).or_default() += 1;
    }

    fn definition(
        &mut self,
        node: Node,
        kind: NodeKind,
        name: String,
        context: &ContextState,
    ) -> ContextState {
        let index = self.output.defs.len();
        self.output.defs.push(RawDef {
            kind,
            name: name.clone(),
            scope: context.scope.clone(),
            span: span(node),
            parent: context.parent,
        });
        let mut nested = context.clone();
        nested.parent = Some(index);
        nested.scope.push(name);
        nested
    }

    fn children(&mut self, node: Node, context: &ContextState) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(child, context);
        }
    }

    fn visit_callable(&mut self, node: Node, context: &ContextState) {
        let Some(body) = node.child_by_field_name("body") else {
            self.observe("declaration_only");
            self.children(node, context);
            return;
        };
        if context.declaration_only {
            self.observe("declaration_only");
            return;
        }
        let binding = expression_owner(node)
            .filter(|parent| parent.kind() == "variable_declarator")
            .and_then(|parent| parent.child_by_field_name("name"))
            .filter(|name| name.kind() == "identifier");
        let own_name = node.child_by_field_name("name");
        let name_node = binding.or(own_name);
        let name = name_node.map_or_else(
            || {
                if default_export(node) {
                    "default".to_owned()
                } else {
                    format!("anonymous@{}", node.start_byte())
                }
            },
            |name| self.text(name),
        );
        let kind = if node.kind() == "method_definition" {
            NodeKind::Method
        } else {
            NodeKind::Function
        };
        let mut nested = self.definition(node, kind, name, context);
        self.output.bodies.push(BodyAnchor {
            definition: nested.parent.expect("new definition"),
            callable: executable_span(node),
            name: name_node.map(span),
            body: executable_span(body),
            compiler_callable: compiler_span(node),
        });
        nested.caller_body = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let mut child_context = nested.clone();
            if child.id() == body.id() {
                child_context.caller_body = Some(executable_span(body));
            }
            self.visit(child, &child_context);
        }
    }

    fn visit_type(&mut self, node: Node, context: &ContextState) {
        let name = node.child_by_field_name("name").map_or_else(
            || format!("anonymous@{}", node.start_byte()),
            |name| self.text(name),
        );
        let kind = match node.kind() {
            "interface_declaration" => NodeKind::Type(TypeKind::Interface),
            "type_alias_declaration" => NodeKind::Type(TypeKind::TypeAlias),
            "enum_declaration" => NodeKind::Type(TypeKind::Enum),
            "internal_module" | "module" => NodeKind::Module,
            _ => NodeKind::Type(TypeKind::Class),
        };
        let mut nested = self.definition(node, kind, name, context);
        nested.caller_body = None;
        if matches!(
            node.kind(),
            "interface_declaration" | "type_alias_declaration"
        ) {
            nested.declaration_only = true;
            self.observe("type_only");
        }
        self.children(node, &nested);
    }

    fn visit(&mut self, node: Node, context: &ContextState) {
        if node.is_error() || node.is_missing() {
            self.output.parse_errors += 1;
        }
        if callable(node.kind()) {
            self.visit_callable(node, context);
            return;
        }
        match node.kind() {
            "ambient_declaration" => {
                let mut nested = context.clone();
                nested.declaration_only = true;
                nested.caller_body = None;
                self.observe("declaration_only");
                self.children(node, &nested);
                return;
            }
            "function_signature"
            | "method_signature"
            | "abstract_method_signature"
            | "call_signature"
            | "construct_signature" => self.observe("declaration_only"),
            "class_declaration"
            | "abstract_class_declaration"
            | "class"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration"
            | "internal_module"
            | "module" => {
                self.visit_type(node, context);
                return;
            }
            "public_field_definition" | "class_static_block" => {
                let mut nested = context.clone();
                nested.caller_body = None;
                self.observe("class_initializer");
                self.children(node, &nested);
                return;
            }
            "import_statement" | "export_statement"
                if node.child_by_field_name("source").is_some() =>
            {
                self.output.imports.push(RawImport {
                    path: self.text(node),
                    span: span(node),
                });
            }
            "import_statement" => self.observe("unsupported_import"),
            "call_expression" | "new_expression" => {
                let constructor = node.kind() == "new_expression";
                if let Some(callee) = node.child_by_field_name(if constructor {
                    "constructor"
                } else {
                    "function"
                }) {
                    self.output.calls.push(RawCall {
                        raw_name: self.text(callee),
                        path_root: None,
                        span: span(node),
                        name_start_byte: callee.start_byte(),
                        receiver: ReceiverShape::Other,
                        scope: context.scope.clone(),
                    });
                    self.output.call_anchors.push(CallAnchor {
                        span: span(node),
                        callee: span(callee),
                        callee_name: span(callee.child_by_field_name("property").unwrap_or(callee)),
                        caller_body: context.caller_body,
                        constructor,
                        tagged: node
                            .child_by_field_name("arguments")
                            .is_some_and(|arguments| arguments.kind() == "template_string"),
                    });
                    if constructor {
                        self.observe("constructor");
                    }
                    if context.caller_body.is_none() {
                        self.observe("call_without_body_caller");
                    }
                    if self.text(callee) == "require" {
                        self.observe("unverified_require");
                    }
                    if callee.kind() == "import" {
                        self.observe("dynamic_import");
                    }
                }
            }
            "jsx_element" => self.observe("jsx_rendering"),
            "jsx_opening_element" | "jsx_self_closing_element" => {
                if node.kind() == "jsx_self_closing_element" {
                    self.observe("jsx_rendering");
                }
                if let Some(name) = node.child_by_field_name("name") {
                    self.output.jsx_names.push(span(name));
                }
            }
            _ => {}
        }
        self.children(node, context);
    }
}

#[cfg(test)]
mod semantic_span_tests {
    use super::*;
    #[test]
    fn executable_witness_ends_at_parsed_token_before_trailing_comments() {
        for path in ["source.js", "source.ts", "source.jsx", "source.tsx"] {
            let source = b"export function caller() { return 1; } // trailing comment\n";
            let raw = extract_file(path, source).unwrap();
            assert_eq!(raw.parse_errors, 0);
            let body = &raw.bodies[0];
            assert_eq!(
                &source[body.body.start_byte..body.body.end_byte],
                b"{ return 1; }"
            );
            assert_eq!(
                &source[body.compiler_callable.start_byte..body.compiler_callable.end_byte],
                b"export function caller() { return 1; }"
            );
        }
    }
}
