//! Prism Ruby syntax extraction (adapter R1).
//!
//! Mirrors [`crate::rust_syntax`]: owns parsing and language-specific
//! extraction only, never guesses targets, never resolves cross-file
//! references — that is [`crate::resolve`]. Produces raw, source-addressed
//! observations; [`crate::extract`] assigns deterministic IDs and
//! [`crate::resolve`] upgrades resolvable observations into edges.
//!
//! Prism (the `CRuby` reference parser, via the official `ruby-prism` Rust
//! bindings) tracks local-variable scope at parse time, which tree-sitter
//! cannot: a bare identifier that is not a local parses as a `CallNode`.
//! This closes the bare-call omission of the tree-sitter-based adapter —
//! `save`, `params`, `render`, route helpers — at the observation layer.
//!
//! Emitted directly from syntax (no resolution needed):
//! - **definition nodes**: `module` → Module, `class` → Type(Class),
//!   `method`/`singleton_method` → Method (`def self.name` recorded as
//!   `self.name`);
//! - **lexical `contains`** edges (file/module/class → nested item);
//! - **`imports`** edges (file → `ImportRef` placeholder) from
//!   `require`/`require_relative`/`load`/`autoload` with a string literal
//!   argument, preserving the literal verbatim.
//!
//! Collected as observations for later resolution:
//! - **superclasses** (`class A < B`) → [`RawInherit`], upgraded to an
//!   `inherits` edge only when both sides resolve to unique in-repository
//!   definitions;
//! - **mixins** (`include`/`extend`/`prepend Mod`) → [`RawMixin`], upgraded
//!   to an `implements` edge under the same discipline;
//! - **call expressions** → [`RawCall`] observations. Syntax-only mode
//!   never emits canonical `calls` edges for Ruby.
//!
//! Documented omissions (observations, never guessed facts):
//! - operator calls (`+`, `==`, `[]=`, …) parse as `CallNode`s in Prism but
//!   are deliberately not recorded;
//! - attribute writes (`foo.bar = 1`) are not recorded as call sites;
//! - `class << expr` singleton classes contribute no definition node; their
//!   methods attach to the enclosing scope;
//! - constant assignments (`FOO = ...`) are not definitions in this phase.

use anyhow::Result;
use ruby_prism::{
    ArgumentsNode, CallNode, ClassNode, ConstantId, DefNode, Location, ModuleNode, Node,
    SingletonClassNode, Visit, visit_call_node, visit_class_node, visit_def_node,
    visit_module_node, visit_program_node, visit_singleton_class_node,
};

use crate::measures::{Form, LineClass, LineCounts, RawMeasure, count_lines, line_classes};
use crate::rust_syntax::{RawCall, RawDef, RawImport, ReceiverShape};
use crate::schema::{NodeKind, Span, TypeKind};

/// Mixin flavor of a [`RawMixin`] observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixinKind {
    Include,
    Extend,
    Prepend,
}

impl MixinKind {
    pub const fn report_key(self) -> &'static str {
        match self {
            Self::Include => "include",
            Self::Extend => "extend",
            Self::Prepend => "prepend",
        }
    }
}

/// A raw mixin observation (`include`/`extend`/`prepend Mod`).
#[derive(Debug, Clone)]
pub struct RawMixin {
    pub mixin: MixinKind,
    /// Module reference as written (`Foo` or `Foo::Bar`).
    pub target: String,
    /// Enclosing class/module for implicit or explicit-self mixin calls.
    /// Call blocks and other receivers remain unknown. Resolution requires `Some`.
    pub receiver_name: Option<String>,
    /// Fully qualified lexical entries at the mixin call, innermost last.
    pub scope: Vec<String>,
    pub span: Span,
}

/// A raw superclass observation (`class A < B`).
#[derive(Debug, Clone)]
pub struct RawInherit {
    /// Superclass reference as written.
    pub superclass: String,
    /// Fully qualified identity of the class declaring the superclass.
    pub class_name: String,
    /// Fully qualified enclosing lexical entries before the class body opens.
    pub scope: Vec<String>,
    pub span: Span,
}

/// Per-file Ruby extraction output.
#[derive(Debug, Clone)]
pub struct RawFile {
    pub defs: Vec<RawDef>,
    pub imports: Vec<RawImport>,
    pub mixins: Vec<RawMixin>,
    pub inherits: Vec<RawInherit>,
    pub calls: Vec<RawCall>,
    pub parse_errors: usize,
    pub measures: Vec<RawMeasure>,
    pub file_lines: LineCounts,
    pub line_classes: Vec<LineClass>,
    /// Known or possible constructor changes; None means an unknown receiver.
    pub constructor_overrides: Vec<(Option<String>, Vec<String>)>,
    pub uncertain_instance_methods: Vec<String>,
    /// Lexical method definition indices whose runtime owner depends on a call block.
    pub unverified_method_owners: Vec<usize>,
}

/// Extract structural observations from a single Ruby source file.
///
/// The `Result` mirrors [`crate::rust_syntax::extract_file`] so the two
/// adapters stay interchangeable at the call site; Prism parsing itself is
/// total (error-tolerant), so the error path is currently unreachable.
#[allow(clippy::unnecessary_wraps)]
pub fn extract_file(_rel_path: &str, source: &[u8]) -> Result<RawFile> {
    let result = ruby_prism::parse(source);
    let parse_errors = result.errors().count();

    let comments: Vec<_> = result
        .comments()
        .map(|comment| {
            let loc = comment.location();
            (loc.start_offset(), loc.end_offset())
        })
        .collect();
    let classes = line_classes(source, &comments);
    let mut walker = Walker::new(source, &classes);
    let root = result.node();
    walker.visit(&root);

    Ok(RawFile {
        defs: walker.defs,
        imports: walker.imports,
        mixins: walker.mixins,
        inherits: walker.inherits,
        calls: walker.calls,
        parse_errors,
        measures: walker.measures,
        file_lines: count_lines(&classes, 1, u32::MAX),
        constructor_overrides: walker.constructor_overrides,
        uncertain_instance_methods: walker.uncertain_instance_methods,
        unverified_method_owners: walker.unverified_method_owners,
        line_classes: classes,
    })
}

struct Walker<'a> {
    source: &'a [u8],
    line_classes: &'a [LineClass],
    measures: Vec<RawMeasure>,
    defs: Vec<RawDef>,
    imports: Vec<RawImport>,
    mixins: Vec<RawMixin>,
    inherits: Vec<RawInherit>,
    calls: Vec<RawCall>,
    /// Fully qualified lexical class/module entries, innermost last.
    /// Methods do not push; the final entry owns definitions and mixins.
    scope_stack: Vec<String>,
    /// Lexical parent definition index parallel to `scope_stack`.
    parent_stack: Vec<usize>,
    /// Enclosing `def` chain: `true` for singleton defs (`def self.x`).
    /// Empty outside any method (class/module bodies, top level).
    def_stack: Vec<bool>,
    singleton_receivers: Vec<Option<String>>,
    constructor_overrides: Vec<(Option<String>, Vec<String>)>,
    uncertain_instance_methods: Vec<String>,
    unverified_method_owners: Vec<usize>,
    unmodeled_block_depth: usize,
    unverified_self_context: bool,
    rebinding_block_start: Option<usize>,
}

impl<'a> Walker<'a> {
    const fn new(source: &'a [u8], line_classes: &'a [LineClass]) -> Self {
        Self {
            source,
            line_classes,
            measures: Vec::new(),
            defs: Vec::new(),
            imports: Vec::new(),
            mixins: Vec::new(),
            inherits: Vec::new(),
            calls: Vec::new(),
            scope_stack: Vec::new(),
            parent_stack: Vec::new(),
            def_stack: Vec::new(),
            singleton_receivers: Vec::new(),
            constructor_overrides: Vec::new(),
            uncertain_instance_methods: Vec::new(),
            unverified_method_owners: Vec::new(),
            unmodeled_block_depth: 0,
            unverified_self_context: false,
            rebinding_block_start: None,
        }
    }

    // ── definitions ──

    fn push_definition(&mut self, kind: NodeKind, name: &str, loc: &Location<'_>) -> usize {
        let idx = self.defs.len();
        let parent = self.parent_stack.last().copied();
        self.defs.push(RawDef {
            kind,
            name: name.strip_prefix("::").unwrap_or(name).to_string(),
            scope: if name.starts_with("::") {
                Vec::new()
            } else {
                self.scope_stack
                    .last()
                    .map(|owner| owner.split("::").map(str::to_string).collect())
                    .unwrap_or_default()
            },
            span: self.span(loc),
            parent,
        });
        idx
    }

    fn visit_class_like(
        &mut self,
        kind: NodeKind,
        constant_path: &Node<'_>,
        body_loc: &Location<'_>,
        superclass: Option<Node<'_>>,
        recurse: impl FnOnce(&mut Self),
    ) {
        let name = collapse_whitespace(self.node_text(constant_path));
        let loc_start = self.scope_stack.len();

        let idx = self.push_definition(kind, &name, body_loc);
        let definition = &self.defs[idx];
        let qualified_name = definition
            .scope
            .iter()
            .chain(std::iter::once(&definition.name))
            .cloned()
            .collect::<Vec<_>>()
            .join("::");

        if let Some(super_node) = superclass {
            self.inherits.push(RawInherit {
                superclass: collapse_whitespace(self.node_text(&super_node)),
                class_name: qualified_name.clone(),
                scope: self.scope_stack.clone(),
                span: self.span(&super_node.location()),
            });
        }

        self.scope_stack.push(qualified_name);
        self.parent_stack.push(idx);
        let enclosing_blocks = std::mem::replace(&mut self.unmodeled_block_depth, 0);
        let enclosing_self = std::mem::replace(&mut self.unverified_self_context, false);
        recurse(self);
        self.unmodeled_block_depth = enclosing_blocks;
        self.unverified_self_context = enclosing_self;
        // Scope must unwind exactly to the pre-entry depth.
        self.scope_stack.truncate(loc_start);
        self.parent_stack
            .truncate(loc_start.min(self.parent_stack.len()));
    }

    // ── calls ──

    fn visit_call(&mut self, node: &CallNode<'_>) {
        // Operator calls and attribute writes are not call-site facts.
        let call_name = node.name();
        if node.is_attribute_write() || !is_identifier_name(&call_name) {
            return;
        }
        let name = constant_str(&call_name);
        self.observe_constructor_change(node, &name);
        match name.as_str() {
            "require" | "require_relative" | "load" | "autoload" => {
                if let Some(arguments) = node.arguments() {
                    if let Some((path, loc)) = first_string_argument(&arguments) {
                        self.imports.push(RawImport {
                            path,
                            span: self.span(&loc),
                        });
                    }
                }
            }
            "include" | "extend" | "prepend" => {
                let mixin = match name.as_str() {
                    "include" => MixinKind::Include,
                    "extend" => MixinKind::Extend,
                    _ => MixinKind::Prepend,
                };
                let receiver_name = match node.receiver() {
                    None | Some(Node::SelfNode { .. }) if self.unmodeled_block_depth == 0 => {
                        self.scope_stack.last().cloned()
                    }
                    _ => None,
                };
                if node.arguments().is_none_or(|args| {
                    args.arguments().iter().any(|arg| {
                        !matches!(
                            arg,
                            Node::ConstantReadNode { .. } | Node::ConstantPathNode { .. }
                        )
                    })
                }) {
                    let receiver = self.mutation_receiver(node.receiver());
                    self.constructor_overrides
                        .push((receiver, self.scope_stack.clone()));
                }
                if let Some(arguments) = node.arguments() {
                    for arg in &arguments.arguments() {
                        if matches!(
                            arg,
                            Node::ConstantReadNode { .. } | Node::ConstantPathNode { .. }
                        ) {
                            let loc = arg.location();
                            self.mixins.push(RawMixin {
                                mixin,
                                target: collapse_whitespace(self.node_text(&arg)),
                                receiver_name: receiver_name.clone(),
                                scope: self.scope_stack.clone(),
                                span: self.span(&loc),
                            });
                        }
                    }
                }
            }
            _ => {
                let message_loc = node.message_loc();
                let name_start = message_loc
                    .as_ref()
                    .map_or_else(|| node.location().start_offset(), Location::start_offset);
                self.calls.push(RawCall {
                    raw_name: name,
                    path_root: None,
                    span: self.span(&node.location()),
                    name_start_byte: name_start,
                    receiver: self.receiver_shape(node),
                    scope: self.scope_stack.clone(),
                });
            }
        }
    }

    // ── helpers ──

    fn observe_constructor_change(&mut self, node: &CallNode<'_>, name: &str) {
        let arguments: Vec<_> = node
            .arguments()
            .map(|args| args.arguments().iter().collect())
            .unwrap_or_default();
        let mut forwarded = name.to_owned();
        let mut index = 0;
        while matches!(forwarded.as_str(), "send" | "public_send" | "__send__") {
            let Some(method) = arguments.get(index).and_then(literal_method_name) else {
                return;
            };
            forwarded = method;
            index += 1;
        }
        let method = arguments.get(index).and_then(literal_method_name);
        let constructor_method = method.as_deref().is_none_or(|method| {
            matches!(
                method,
                "new" | "included" | "prepended" | "extended" | "inherited"
            )
        });
        let singleton_definition = forwarded == "define_singleton_method";
        let instance_self = self.def_stack.last() == Some(&false)
            && self.singleton_receivers.is_empty()
            && matches!(node.receiver(), None | Some(Node::SelfNode { .. }));
        if (singleton_definition && (constructor_method || instance_self))
            || matches!(
                forwarded.as_str(),
                "define_method"
                    | "alias_method"
                    | "undef_method"
                    | "remove_method"
                    | "class_eval"
                    | "module_eval"
                    | "instance_eval"
                    | "eval"
            )
        {
            self.constructor_overrides.push((
                self.mutation_receiver(node.receiver()),
                self.scope_stack.clone(),
            ));
        } else if singleton_definition
            && !matches!(
                node.receiver(),
                None | Some(
                    Node::SelfNode { .. }
                        | Node::ConstantReadNode { .. }
                        | Node::ConstantPathNode { .. }
                )
            )
            && let Some(method) = method
        {
            self.uncertain_instance_methods.push(method);
        }
    }

    fn mutation_receiver(&self, receiver: Option<Node<'_>>) -> Option<String> {
        match receiver {
            None | Some(Node::SelfNode { .. }) => self
                .singleton_receivers
                .last()
                .cloned()
                .unwrap_or_else(|| self.scope_stack.last().map(|name| format!("::{name}"))),
            Some(receiver @ (Node::ConstantReadNode { .. } | Node::ConstantPathNode { .. })) => {
                Some(collapse_whitespace(self.node_text(&receiver)))
            }
            _ => None,
        }
    }

    fn node_text(&self, node: &Node<'_>) -> &str {
        let loc = node.location();
        let start = loc.start_offset();
        let end = loc.end_offset().min(self.source.len());
        std::str::from_utf8(&self.source[start..end]).unwrap_or("")
    }

    fn span(&self, loc: &Location<'_>) -> Span {
        let start = loc.start_offset();
        let end = loc.end_offset();
        let (start_line, start_col) = line_col(self.source, start);
        let (end_line, end_col) = line_col(self.source, end);
        Span::from_ts(start, end, (start_line, start_col), (end_line, end_col))
    }
}

impl<'pr> Visit<'pr> for Walker<'_> {
    fn visit_class_node(&mut self, node: &ClassNode<'pr>) {
        let path = node.constant_path();
        let superclass = node.superclass();
        let body_loc = node.location();
        self.visit_class_like(
            NodeKind::Type(TypeKind::Class),
            &path,
            &body_loc,
            superclass,
            |w| {
                visit_class_node(w, node);
            },
        );
    }

    fn visit_module_node(&mut self, node: &ModuleNode<'pr>) {
        let path = node.constant_path();
        let body_loc = node.location();
        self.visit_class_like(NodeKind::Module, &path, &body_loc, None, |w| {
            visit_module_node(w, node);
        });
    }

    fn visit_singleton_class_node(&mut self, node: &SingletonClassNode<'pr>) {
        // No definition node: methods inside attach to the enclosing scope
        // (documented omission). The receiver stack marks calls inside as
        // singleton-context, where implicit self is not instance-verifiable.
        let receiver = match node.expression() {
            Node::SelfNode { .. } => self.scope_stack.last().map(|name| format!("::{name}")),
            receiver @ (Node::ConstantReadNode { .. } | Node::ConstantPathNode { .. }) => {
                Some(collapse_whitespace(self.node_text(&receiver)))
            }
            _ => None,
        };
        self.singleton_receivers.push(receiver);
        visit_singleton_class_node(self, node);
        self.singleton_receivers.pop();
    }

    fn visit_def_node(&mut self, node: &DefNode<'pr>) {
        let def_name = node.name();
        let base = constant_str(&def_name);
        let instance_singleton = self.def_stack.last() == Some(&false)
            && matches!(node.receiver(), Some(Node::SelfNode { .. }));
        if instance_singleton
            || !self.singleton_receivers.is_empty()
            || matches!(
                base.as_str(),
                "new" | "included" | "prepended" | "extended" | "inherited"
            )
        {
            let receiver = match node.receiver() {
                Some(Node::SelfNode { .. }) => {
                    Some(self.scope_stack.last().map(|name| format!("::{name}")))
                }
                Some(
                    receiver @ (Node::ConstantReadNode { .. } | Node::ConstantPathNode { .. }),
                ) => Some(Some(collapse_whitespace(self.node_text(&receiver)))),
                Some(_) => Some(None),
                None => self.singleton_receivers.last().cloned(),
            };
            if let Some(receiver) = receiver {
                self.constructor_overrides
                    .push((receiver, self.scope_stack.clone()));
            }
        }
        let is_singleton = node
            .receiver()
            .is_some_and(|receiver| matches!(receiver, Node::SelfNode { .. }));
        let name = if is_singleton {
            format!("self.{base}")
        } else {
            base
        };
        // The definition spans the whole `def` body (like Rust `fn`s) so
        // semantic resolution can locate call sites inside it.
        let loc = node.location();
        let def = self.push_definition(NodeKind::Method, &name, &loc);
        if self.unmodeled_block_depth > 0 {
            self.unverified_method_owners.push(def);
        }
        let span = self.defs[def].span;
        self.measures.push(RawMeasure {
            def,
            cc: cyclomatic(node),
            lines: count_lines(self.line_classes, span.start_line, span.end_line),
            form: Some(form(node)),
        });
        // Methods do not extend the scope chain; nested definitions keep the
        // enclosing class/module as their lexical parent.
        self.def_stack.push(is_singleton);
        let enclosing_self = self.unverified_self_context;
        self.unverified_self_context |= self.unmodeled_block_depth > 0;
        visit_def_node(self, node);
        self.unverified_self_context = enclosing_self;
        self.def_stack.pop();
    }

    fn visit_call_node(&mut self, node: &CallNode<'pr>) {
        self.visit_call(node);
        // Always continue traversal: arguments and receivers of routed or
        // skipped calls can contain real call sites.
        let enclosing_rebinding = self.rebinding_block_start;
        self.rebinding_block_start = if matches!(
            node.name().as_slice(),
            b"instance_eval"
                | b"instance_exec"
                | b"class_eval"
                | b"class_exec"
                | b"module_eval"
                | b"module_exec"
        ) {
            node.block().map(|block| block.location().start_offset())
        } else {
            None
        };
        visit_call_node(self, node);
        self.rebinding_block_start = enclosing_rebinding;
    }

    fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
        self.unmodeled_block_depth += 1;
        let enclosing_self = self.unverified_self_context;
        self.unverified_self_context |=
            self.rebinding_block_start == Some(node.location().start_offset());
        ruby_prism::visit_block_node(self, node);
        self.unverified_self_context = enclosing_self;
        self.unmodeled_block_depth -= 1;
    }

    fn visit_program_node(&mut self, node: &ruby_prism::ProgramNode<'pr>) {
        visit_program_node(self, node);
    }
}

impl Walker<'_> {
    /// Receiver shape policy input for the R2 semantic edge policy.
    ///
    /// Implicit self is instance-verifiable only inside an instance method
    /// body. In class/module bodies, singleton defs, and `class << self`
    /// blocks, `self` is the singleton — codemap's static model does not
    /// verify that regime, so those calls are `Other` (observations).
    /// Ordinary yielding blocks retain the legacy R2 enclosing-self regime;
    /// this does not prove an arbitrary block consumer preserves self.
    /// Explicit eval/exec blocks and block-owned method bodies are unverified.
    fn receiver_shape(&self, node: &CallNode<'_>) -> ReceiverShape {
        match node.receiver() {
            None | Some(Node::SelfNode { .. }) => {
                let in_instance_def = self.def_stack.last() == Some(&false);
                if in_instance_def
                    && self.singleton_receivers.is_empty()
                    && !self.unverified_self_context
                {
                    ReceiverShape::ImplicitSelf
                } else {
                    ReceiverShape::Other
                }
            }
            Some(receiver @ (Node::ConstantReadNode { .. } | Node::ConstantPathNode { .. })) => {
                ReceiverShape::Constant(collapse_whitespace(self.node_text(&receiver)))
            }
            Some(receiver @ Node::CallNode { .. }) => {
                let receiver = receiver.as_call_node().unwrap();
                if receiver.name().as_slice() != b"new" {
                    return ReceiverShape::Other;
                }
                match receiver.receiver() {
                    Some(
                        class @ (Node::ConstantReadNode { .. } | Node::ConstantPathNode { .. }),
                    ) => ReceiverShape::Constructed {
                        class_name: collapse_whitespace(self.node_text(&class)),
                        constructor_start: receiver.message_loc().map_or_else(
                            || receiver.location().start_offset(),
                            |loc| loc.start_offset(),
                        ),
                    },
                    _ => ReceiverShape::Other,
                }
            }
            _ => ReceiverShape::Other,
        }
    }
}

fn literal_method_name(node: &Node<'_>) -> Option<String> {
    match node {
        Node::SymbolNode { .. } => {
            Some(String::from_utf8_lossy(node.as_symbol_node().unwrap().unescaped()).into_owned())
        }
        Node::StringNode { .. } => {
            Some(String::from_utf8_lossy(node.as_string_node().unwrap().unescaped()).into_owned())
        }
        _ => None,
    }
}

/// Ruby policy `cc-sloc-clones-form/ruby/1`: base one, conditional/loop nodes,
/// when/in arms, rescue clauses, boolean operators and safe navigation.
/// Prism represents ternary/modifier/elsif forms with the same node kinds.
/// Blocks and lambdas count in their enclosing method; nested named
/// definitions have their own measures and do not inflate the parent CC.
pub fn cyclomatic(method: &DefNode<'_>) -> u32 {
    struct Decisions(u32);
    // Prism traversal can call typed visitors directly (notably rescue),
    // bypassing visit_branch_node_enter. Count in the typed visitors.
    impl<'pr> Visit<'pr> for Decisions {
        fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
            self.0 += 1;
            ruby_prism::visit_if_node(self, node);
        }

        fn visit_unless_node(&mut self, node: &ruby_prism::UnlessNode<'pr>) {
            self.0 += 1;
            ruby_prism::visit_unless_node(self, node);
        }

        fn visit_while_node(&mut self, node: &ruby_prism::WhileNode<'pr>) {
            self.0 += 1;
            ruby_prism::visit_while_node(self, node);
        }

        fn visit_until_node(&mut self, node: &ruby_prism::UntilNode<'pr>) {
            self.0 += 1;
            ruby_prism::visit_until_node(self, node);
        }

        fn visit_for_node(&mut self, node: &ruby_prism::ForNode<'pr>) {
            self.0 += 1;
            ruby_prism::visit_for_node(self, node);
        }

        fn visit_when_node(&mut self, node: &ruby_prism::WhenNode<'pr>) {
            self.0 += 1;
            ruby_prism::visit_when_node(self, node);
        }

        fn visit_in_node(&mut self, node: &ruby_prism::InNode<'pr>) {
            self.0 += 1;
            ruby_prism::visit_in_node(self, node);
        }

        fn visit_rescue_node(&mut self, node: &ruby_prism::RescueNode<'pr>) {
            self.0 += 1;
            ruby_prism::visit_rescue_node(self, node);
        }

        fn visit_rescue_modifier_node(&mut self, node: &ruby_prism::RescueModifierNode<'pr>) {
            self.0 += 1;
            ruby_prism::visit_rescue_modifier_node(self, node);
        }

        fn visit_and_node(&mut self, node: &ruby_prism::AndNode<'pr>) {
            self.0 += 1;
            ruby_prism::visit_and_node(self, node);
        }

        fn visit_or_node(&mut self, node: &ruby_prism::OrNode<'pr>) {
            self.0 += 1;
            ruby_prism::visit_or_node(self, node);
        }

        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            self.0 += u32::from(node.is_safe_navigation());
            ruby_prism::visit_call_node(self, node);
        }

        fn visit_call_and_write_node(&mut self, node: &ruby_prism::CallAndWriteNode<'pr>) {
            self.0 += u32::from(node.is_safe_navigation());
            ruby_prism::visit_call_and_write_node(self, node);
        }

        fn visit_call_operator_write_node(
            &mut self,
            node: &ruby_prism::CallOperatorWriteNode<'pr>,
        ) {
            self.0 += u32::from(node.is_safe_navigation());
            ruby_prism::visit_call_operator_write_node(self, node);
        }

        fn visit_call_or_write_node(&mut self, node: &ruby_prism::CallOrWriteNode<'pr>) {
            self.0 += u32::from(node.is_safe_navigation());
            ruby_prism::visit_call_or_write_node(self, node);
        }

        fn visit_call_target_node(&mut self, node: &ruby_prism::CallTargetNode<'pr>) {
            self.0 += u32::from(node.is_safe_navigation());
            ruby_prism::visit_call_target_node(self, node);
        }

        fn visit_index_and_write_node(&mut self, node: &ruby_prism::IndexAndWriteNode<'pr>) {
            self.0 += u32::from(node.is_safe_navigation());
            ruby_prism::visit_index_and_write_node(self, node);
        }

        fn visit_index_operator_write_node(
            &mut self,
            node: &ruby_prism::IndexOperatorWriteNode<'pr>,
        ) {
            self.0 += u32::from(node.is_safe_navigation());
            ruby_prism::visit_index_operator_write_node(self, node);
        }

        fn visit_index_or_write_node(&mut self, node: &ruby_prism::IndexOrWriteNode<'pr>) {
            self.0 += u32::from(node.is_safe_navigation());
            ruby_prism::visit_index_or_write_node(self, node);
        }

        fn visit_index_target_node(&mut self, node: &ruby_prism::IndexTargetNode<'pr>) {
            self.0 += u32::from(node.is_safe_navigation());
            ruby_prism::visit_index_target_node(self, node);
        }

        fn visit_def_node(&mut self, _node: &DefNode<'pr>) {}
    }
    let mut decisions = Decisions(1);
    visit_def_node(&mut decisions, method);
    decisions.0
}

/// The four form measures of one `def` under `cc-sloc-clones-form/ruby/1`.
/// Each is counted over the same body `cyclomatic` reads: nested named
/// definitions are left to their own measures, blocks and lambdas belong
/// to the method that encloses them.
pub fn form(method: &DefNode<'_>) -> Form {
    Form {
        params: params(method),
        nesting: nesting(method),
        chain: chain(method),
        ivars: Some(ivars(method)),
    }
}

/// Parameters the `def` declares: required, optional, and post-rest
/// positionals and keywords one each; a rest (`*rest` or a forwarding
/// `...`), a keyword rest, and a block one each; `**nil` declares none.
fn params(method: &DefNode<'_>) -> u32 {
    let Some(parameters) = method.parameters() else {
        return 0;
    };
    let listed = parameters.requireds().iter().count()
        + parameters.optionals().iter().count()
        + parameters.posts().iter().count()
        + parameters.keywords().iter().count();
    let keyword_rest = parameters
        .keyword_rest()
        .is_some_and(|node| node.as_no_keywords_parameter_node().is_none());
    let count = listed
        + usize::from(parameters.rest().is_some())
        + usize::from(keyword_rest)
        + usize::from(parameters.block().is_some());
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The deepest stack of control structures any node of the body sits
/// under: `if`/`unless` (an `elsif` reached through `subsequent` stays at
/// its parent's depth; modifier and ternary forms are the same node and
/// count one level), `while`/`until`/`for`, `case`/`case-in` (arms add
/// nothing), and block or lambda bodies, Ruby's loops. `begin`/`rescue`
/// is not a control structure here. A straight body is 0.
fn nesting(method: &DefNode<'_>) -> u32 {
    struct Nesting {
        depth: u32,
        deepest: u32,
    }
    impl Nesting {
        fn enter(&mut self, inside: impl FnOnce(&mut Self)) {
            self.depth += 1;
            self.deepest = self.deepest.max(self.depth);
            inside(self);
            self.depth -= 1;
        }
    }
    impl<'pr> Visit<'pr> for Nesting {
        fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
            self.enter(|v| {
                v.visit(&node.predicate());
                if let Some(statements) = node.statements() {
                    ruby_prism::visit_statements_node(v, &statements);
                }
            });
            if let Some(subsequent) = node.subsequent() {
                match (subsequent.as_if_node(), subsequent.as_else_node()) {
                    (Some(elsif), _) => self.visit_if_node(&elsif),
                    (None, Some(else_clause)) => {
                        self.enter(|v| ruby_prism::visit_else_node(v, &else_clause));
                    }
                    (None, None) => self.visit(&subsequent),
                }
            }
        }

        fn visit_unless_node(&mut self, node: &ruby_prism::UnlessNode<'pr>) {
            self.enter(|v| ruby_prism::visit_unless_node(v, node));
        }

        fn visit_while_node(&mut self, node: &ruby_prism::WhileNode<'pr>) {
            self.enter(|v| ruby_prism::visit_while_node(v, node));
        }

        fn visit_until_node(&mut self, node: &ruby_prism::UntilNode<'pr>) {
            self.enter(|v| ruby_prism::visit_until_node(v, node));
        }

        fn visit_for_node(&mut self, node: &ruby_prism::ForNode<'pr>) {
            self.enter(|v| ruby_prism::visit_for_node(v, node));
        }

        fn visit_case_node(&mut self, node: &ruby_prism::CaseNode<'pr>) {
            self.enter(|v| ruby_prism::visit_case_node(v, node));
        }

        fn visit_case_match_node(&mut self, node: &ruby_prism::CaseMatchNode<'pr>) {
            self.enter(|v| ruby_prism::visit_case_match_node(v, node));
        }

        fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
            self.enter(|v| ruby_prism::visit_block_node(v, node));
        }

        fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
            self.enter(|v| ruby_prism::visit_lambda_node(v, node));
        }

        fn visit_def_node(&mut self, _node: &DefNode<'pr>) {}
    }
    let mut nesting = Nesting {
        depth: 0,
        deepest: 0,
    };
    visit_def_node(&mut nesting, method);
    nesting.deepest
}

/// Dispatches written with a call operator (`.`, `&.`, `::`) along one
/// call's receiver spine: `a.b.c.d` is 3, `Foo.new.bar` is 2, `a + b` and
/// `a[0]` are 0 (operator and index calls have no call operator), a bare
/// `foo` is 0.
fn spine(call: &CallNode<'_>) -> u32 {
    if call.call_operator_loc().is_none() {
        return 0;
    }
    let below = call
        .receiver()
        .and_then(|receiver| receiver.as_call_node())
        .map_or(0, |receiver| spine(&receiver));
    1 + below
}

/// The longest receiver spine of any call in the body ([`spine`]); a body
/// with no dotted call is 0.
fn chain(method: &DefNode<'_>) -> u32 {
    struct Chain(u32);
    impl<'pr> Visit<'pr> for Chain {
        fn visit_call_node(&mut self, node: &CallNode<'pr>) {
            self.0 = self.0.max(spine(node));
            visit_call_node(self, node);
        }

        fn visit_def_node(&mut self, _node: &DefNode<'pr>) {}
    }
    let mut chain = Chain(0);
    visit_def_node(&mut chain, method);
    chain.0
}

/// Distinct instance variables the body writes, in any write form
/// (`@a =`, `@a ||=`, `@a &&=`, `@a += 1`, a multiple-assignment target);
/// reads do not count.
fn ivars(method: &DefNode<'_>) -> u32 {
    struct Ivars(std::collections::BTreeSet<String>);
    impl<'pr> Visit<'pr> for Ivars {
        fn visit_instance_variable_write_node(
            &mut self,
            node: &ruby_prism::InstanceVariableWriteNode<'pr>,
        ) {
            self.0.insert(constant_str(&node.name()));
            ruby_prism::visit_instance_variable_write_node(self, node);
        }

        fn visit_instance_variable_or_write_node(
            &mut self,
            node: &ruby_prism::InstanceVariableOrWriteNode<'pr>,
        ) {
            self.0.insert(constant_str(&node.name()));
            ruby_prism::visit_instance_variable_or_write_node(self, node);
        }

        fn visit_instance_variable_and_write_node(
            &mut self,
            node: &ruby_prism::InstanceVariableAndWriteNode<'pr>,
        ) {
            self.0.insert(constant_str(&node.name()));
            ruby_prism::visit_instance_variable_and_write_node(self, node);
        }

        fn visit_instance_variable_operator_write_node(
            &mut self,
            node: &ruby_prism::InstanceVariableOperatorWriteNode<'pr>,
        ) {
            self.0.insert(constant_str(&node.name()));
            ruby_prism::visit_instance_variable_operator_write_node(self, node);
        }

        fn visit_instance_variable_target_node(
            &mut self,
            node: &ruby_prism::InstanceVariableTargetNode<'pr>,
        ) {
            self.0.insert(constant_str(&node.name()));
        }

        fn visit_def_node(&mut self, _node: &DefNode<'pr>) {}
    }
    let mut ivars = Ivars(std::collections::BTreeSet::new());
    visit_def_node(&mut ivars, method);
    u32::try_from(ivars.0.len()).unwrap_or(u32::MAX)
}

fn constant_str(id: &ConstantId<'_>) -> String {
    String::from_utf8_lossy(id.as_slice()).into_owned()
}

/// Prism reports operator methods (`+`, `==`, `[]=`, `-@`, …) and setters
/// as call names; only identifier-shaped names (`save`, `find`, `valid?`,
/// `changed!`) are call-site facts.
fn is_identifier_name(id: &ConstantId<'_>) -> bool {
    let bytes = id.as_slice();
    let Some(&first) = bytes.first() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    if bytes.len() == 1 {
        return true;
    }
    let last = bytes[bytes.len() - 1];
    let middle_ok = bytes[1..bytes.len() - 1]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_');
    let last_ok = last.is_ascii_alphanumeric() || last == b'_' || last == b'?' || last == b'!';
    middle_ok && last_ok
}

fn first_string_argument<'n>(arguments: &ArgumentsNode<'n>) -> Option<(String, Location<'n>)> {
    for arg in &arguments.arguments() {
        if let Some(string) = arg.as_string_node() {
            let content_loc = string.content_loc();
            let path = std::str::from_utf8(content_loc.as_slice())
                .unwrap_or("")
                .to_string();
            return Some((path, string.location()));
        }
    }
    None
}

/// 0-based line and byte column for a byte offset (tree-sitter point
/// convention, which `Span::from_ts` converts to 1-based lines).
fn line_col(source: &[u8], offset: usize) -> (usize, usize) {
    let mut line = 0usize;
    let mut last_nl = 0usize;
    for (i, &b) in source.iter().take(offset).enumerate() {
        if b == b'\n' {
            line += 1;
            last_nl = i + 1;
        }
    }
    (line, offset - last_nl)
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(src: &str) -> RawFile {
        extract_file("app/test.rb", src.as_bytes()).unwrap()
    }

    #[test]
    fn extracts_module_class_and_methods() {
        let f = extract(
            "module Repo\n  class User\n    def save\n    end\n\n    def self.find\n    end\n  end\nend\n",
        );
        let kinds: Vec<_> = f.defs.iter().map(|d| (d.kind, d.name.as_str())).collect();
        assert!(kinds.contains(&(NodeKind::Module, "Repo")));
        assert!(kinds.contains(&(NodeKind::Type(TypeKind::Class), "User")));
        assert!(kinds.contains(&(NodeKind::Method, "save")));
        assert!(kinds.contains(&(NodeKind::Method, "self.find")));

        let user = f.defs.iter().find(|d| d.name == "User").unwrap();
        assert_eq!(user.scope, vec!["Repo".to_string()]);
        let save = f.defs.iter().find(|d| d.name == "save").unwrap();
        assert_eq!(save.scope, vec!["Repo".to_string(), "User".to_string()]);
    }

    #[test]
    fn call_blocks_preserve_lexical_defs_without_proving_owners_or_self() {
        let raw = extract(include_str!(
            "../tests/fixtures/ruby_block_ownership/ownership.rb"
        ));
        assert_eq!(raw.parse_errors, 0);
        let unverified: Vec<_> = raw
            .unverified_method_owners
            .iter()
            .map(|index| raw.defs[*index].name.as_str())
            .collect();
        assert_eq!(
            unverified,
            [
                "routed",
                "misplaced",
                "unverified",
                "self.unverified_singleton"
            ]
        );
        assert!(
            raw.calls
                .iter()
                .filter(|call| call.raw_name == "lexical_helper")
                .all(|call| call.receiver == ReceiverShape::Other)
        );
        let driver_helpers: Vec<_> = raw
            .calls
            .iter()
            .filter(|call| call.raw_name == "helper" && call.scope == ["Driver"])
            .map(|call| &call.receiver)
            .collect();
        assert_eq!(
            driver_helpers,
            [
                &ReceiverShape::Other,
                &ReceiverShape::Other,
                &ReceiverShape::ImplicitSelf,
                &ReceiverShape::ImplicitSelf,
                &ReceiverShape::ImplicitSelf,
                &ReceiverShape::Other,
                &ReceiverShape::Other,
                &ReceiverShape::Other,
                &ReceiverShape::Other,
                &ReceiverShape::Other
            ]
        );
        assert!(raw.calls.iter().any(|call| call.raw_name == "helper"
            && call.scope == ["Yielding", "Yielding::Explicit"]
            && call.receiver == ReceiverShape::ImplicitSelf));
        assert!(raw.calls.iter().any(|call| call.raw_name == "value"
            && call.receiver == ReceiverShape::Constant("Utility".into())));
        for index in raw.unverified_method_owners {
            assert!(raw.measures.iter().any(|measure| measure.def == index));
        }
    }

    #[test]
    fn block_mixins_keep_unknown_receivers_until_explicit_class_scope() {
        let raw = extract(
            "module Lexical\n  Other.class_eval do\n    include Feature\n    self.prepend Feature\n    extend Feature\n    class Explicit\n      include Feature\n    end\n  end\nend\n",
        );
        let receivers: Vec<_> = raw
            .mixins
            .iter()
            .map(|mixin| mixin.receiver_name.as_deref())
            .collect();
        assert_eq!(receivers, [None, None, None, Some("Lexical::Explicit")]);
    }

    #[test]
    fn extracts_superclass_as_inherit_observation() {
        let f = extract("class Admin < User\nend\n");
        assert_eq!(f.inherits.len(), 1);
        assert_eq!(f.inherits[0].superclass, "User");
        assert_eq!(f.inherits[0].class_name, "Admin");
    }

    #[test]
    fn extracts_mixins_with_receiver() {
        let f = extract(
            "class Admin\n  include Auditable\n  extend Finder\n  prepend Versioned\nend\n",
        );
        assert_eq!(f.mixins.len(), 3);
        let include = &f.mixins[0];
        assert_eq!(include.mixin, MixinKind::Include);
        assert_eq!(include.target, "Auditable");
        assert_eq!(include.receiver_name.as_deref(), Some("Admin"));
        assert_eq!(f.mixins[1].mixin, MixinKind::Extend);
        assert_eq!(f.mixins[2].mixin, MixinKind::Prepend);
    }

    #[test]
    fn extracts_require_imports_verbatim() {
        let f = extract("require \"json\"\nrequire_relative \"repo/user\"\n");
        assert_eq!(f.imports.len(), 2);
        assert_eq!(f.imports[0].path, "json");
        assert_eq!(f.imports[1].path, "repo/user");
    }

    #[test]
    fn collects_call_sites_as_observations() {
        let f = extract("def run\n  record.save\n  publish(event)\nend\n");
        let names: Vec<_> = f.calls.iter().map(|c| c.raw_name.as_str()).collect();
        assert!(names.contains(&"save"));
        assert!(names.contains(&"publish"));
        // Prism tracks locals: `record` and `event` are not locals here, so
        // they are method calls too — receiver-side calls included.
        assert!(names.contains(&"record"));
        assert!(names.contains(&"event"));
    }

    #[test]
    fn collects_bare_calls_tree_sitter_missed() {
        // `params` and `save` are not locals, so Prism parses them as
        // CallNodes; local variable reads stay out of the observations.
        let f = extract("def index\n  user = build_user\n  params\n  save\n  user\nend\n");
        let names: Vec<_> = f.calls.iter().map(|c| c.raw_name.as_str()).collect();
        assert!(names.contains(&"params"));
        assert!(names.contains(&"save"));
        assert!(names.contains(&"build_user"));
        assert!(!names.contains(&"user"), "local reads are not calls");
    }

    #[test]
    fn excludes_operator_calls_and_attribute_writes() {
        let f = extract("def f(a)\n  a + 1\n  a == b\n  self.name = \"x\"\n  a[0]\nend\n");
        let names: Vec<_> = f.calls.iter().map(|c| c.raw_name.as_str()).collect();
        assert!(!names.contains(&"+"));
        assert!(!names.contains(&"=="));
        assert!(!names.contains(&"[]"));
        assert!(!names.contains(&"name="));
        // `a` is a declared local -> not a call; `b` is not -> bare call.
        assert!(!names.contains(&"a"));
        assert!(names.contains(&"b"));
    }

    #[test]
    fn require_and_include_are_not_call_observations() {
        let f = extract("require \"json\"\nclass A\n  include B\nend\n");
        assert!(f.calls.is_empty(), "calls: {:?}", f.calls);
    }

    #[test]
    fn reports_parse_errors_on_malformed_input() {
        let f = extract("class Broken\n  def oops\nend\n");
        assert!(
            f.parse_errors > 0,
            "expected parse errors, got {}",
            f.parse_errors
        );
    }

    #[test]
    fn nested_named_methods_have_independent_complexity() {
        let raw =
            extract("def outer\n  def inner(x)\n    x ? 1 : 0\n  end\n  true if ready\nend\n");
        assert_eq!(raw.parse_errors, 0);
        let counts: Vec<_> = raw
            .measures
            .iter()
            .map(|m| (raw.defs[m.def].name.as_str(), m.cc))
            .collect();
        assert_eq!(counts, [("outer", 2), ("inner", 2)]);
    }

    #[test]
    fn safe_navigation_on_assignment_nodes_counts_once() {
        let raw = extract(
            "def update(value)\n  value&.name = 1\n  value&.name += 1\n  value&.name ||= 1\n  value&.name &&= 1\nend\n",
        );
        assert_eq!(raw.parse_errors, 0);
        assert_eq!(
            raw.measures[0].cc, 5,
            "base plus four safe navigations; assignment operators are not boolean expression nodes"
        );
    }
    #[test]
    fn executable_singleton_class_body_counts_in_the_enclosing_method() {
        let raw = extract(
            "def setup(value)\n  class << value\n    include Feature if enabled?\n    def inner(x)\n      x ? 1 : 0\n    end\n  end\nend\n",
        );
        assert_eq!(raw.parse_errors, 0);
        let counts: Vec<_> = raw
            .measures
            .iter()
            .map(|m| (raw.defs[m.def].name.as_str(), m.cc))
            .collect();
        assert_eq!(counts, [("setup", 2), ("inner", 2)]);
    }
}
