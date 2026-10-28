//! Independent source witnesses for compiler-resolved immutable direct bindings.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use tree_sitter::Node;

use crate::js_ts_semantic::CompilerEvidence;
use crate::js_ts_syntax::{BodyAnchor, RawFile, compiler_span, executable_span};
use crate::schema::Span;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Location {
    path: String,
    start: usize,
    end: usize,
    start_byte: usize,
    end_byte: usize,
    line: usize,
    kind: String,
}

impl Location {
    fn matches(&self, source: &[u8], span: Span) -> bool {
        if self.start_byte != span.start_byte || self.end_byte != span.end_byte {
            return false;
        }
        let Ok(text) = std::str::from_utf8(source) else {
            return false;
        };
        let Some(before) = text.get(..self.start_byte) else {
            return false;
        };
        let Some(through) = text.get(..self.end_byte) else {
            return false;
        };
        self.start_byte == span.start_byte
            && self.end_byte == span.end_byte
            && self.start <= self.end
            && before.encode_utf16().count() == self.start
            && through.encode_utf16().count() == self.end
    }
}

#[derive(Deserialize)]
struct Declaration {
    #[serde(flatten)]
    location: Location,
    callable: Option<Location>,
    body: Option<Location>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Admission {
    reason: String,
    target: Option<Location>,
    target_body: Option<Location>,
    caller: Option<Location>,
    signature: Option<Location>,
    mutations: Option<Vec<Location>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Site {
    admission: Admission,
    location: Location,
    callee: String,
    callee_location: Location,
    callee_name: Location,
    callee_any: bool,
    caller: Option<Location>,
    symbol: Option<String>,
    declarations: Vec<Declaration>,
    signature: Option<Location>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Alias {
    declaration: Location,
    targets: Vec<Location>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Diagnostic {
    code: u32,
    path: Option<String>,
    start: Option<usize>,
    length: Option<usize>,
}

impl Diagnostic {
    fn overlaps(&self, location: &Location) -> bool {
        self.path.as_deref() == Some(location.path.as_str())
            && self.start.is_some_and(|start| {
                start < location.end && location.start < start + self.length.unwrap_or(1).max(1)
            })
    }
}

fn checked_diagnostics(evidence: &CompilerEvidence) -> Result<Vec<Diagnostic>> {
    let program = &evidence.program_diagnostics;
    ensure!(
        program.options == 0 && program.global == 0 && program.syntactic == 0,
        "invalid whole-program compiler diagnostics"
    );
    let diagnostics: Vec<Diagnostic> = evidence
        .diagnostics
        .iter()
        .map(|value| serde_json::from_value(value.clone()))
        .collect::<std::result::Result<_, _>>()
        .context("malformed compiler diagnostic evidence")?;
    for diagnostic in &diagnostics {
        ensure!(
            ![2307, 2688, 2792, 6053, 6305].contains(&diagnostic.code)
                && diagnostic
                    .path
                    .as_ref()
                    .is_some_and(|path| !path.is_empty())
                && diagnostic.start.is_some_and(|start| start
                    .checked_add(diagnostic.length.unwrap_or(1).max(1))
                    .is_some()),
            "blocking or invalid compiler diagnostic evidence"
        );
    }
    Ok(diagnostics)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct BindingKey {
    path: String,
    index: usize,
}

#[derive(Debug, Clone, Copy)]
enum Role {
    Body(usize),
    Import,
    Other,
}

struct Binding {
    name: String,
    scope: usize,
    declaration: Span,
    role: Role,
    default_export: bool,
}

struct Scope {
    span: Span,
    parent: Option<usize>,
    function: bool,
}

struct Signature {
    span: Span,
    name: String,
    scope: usize,
}

struct FilePolicy {
    scopes: Vec<Scope>,
    bindings: Vec<Binding>,
    signatures: Vec<Signature>,
    writes: BTreeSet<usize>,
    dynamic: bool,
    escaped_bindings: bool,
    module: bool,
}

impl FilePolicy {
    fn new(raw: &RawFile, source: &[u8], path: &str) -> Self {
        let root = raw.tree.root_node();
        let mut policy = Self {
            scopes: vec![Scope {
                span: node_span(root),
                parent: None,
                function: true,
            }],
            bindings: Vec::new(),
            signatures: Vec::new(),
            writes: BTreeSet::new(),
            dynamic: false,
            escaped_bindings: false,
            module: [".cjs", ".cts", ".mjs", ".mts"]
                .iter()
                .any(|extension| path.ends_with(extension)),
        };
        policy.scan(root, 0, raw, source);
        policy.scan_writes(root, source);
        policy
    }

    fn add_scope(&mut self, node: Node, parent: usize, function: bool) -> usize {
        let index = self.scopes.len();
        self.scopes.push(Scope {
            span: node_span(node),
            parent: Some(parent),
            function,
        });
        index
    }

    fn add_binding(
        &mut self,
        name: Node,
        declaration: Span,
        scope: usize,
        role: Role,
        source: &[u8],
        default_export: bool,
    ) {
        self.bindings.push(Binding {
            name: node_text(name, source).to_owned(),
            declaration,
            scope,
            role,
            default_export,
        });
    }

    fn scan(&mut self, node: Node, scope: usize, raw: &RawFile, source: &[u8]) {
        let kind = node.kind();
        if kind == "identifier" && node_text(node, source).contains('\\') {
            self.escaped_bindings = true;
        }
        if kind == "with_statement"
            || (kind == "call_expression"
                && node
                    .child_by_field_name("function")
                    .is_some_and(|callee| is_eval(callee, source)))
        {
            self.dynamic = true;
        }
        if scope == 0 && matches!(kind, "import_statement" | "export_statement") {
            self.module = true;
        }
        let function = matches!(
            kind,
            "function_declaration"
                | "generator_function_declaration"
                | "function_expression"
                | "generator_function"
                | "arrow_function"
                | "method_definition"
                | "function_signature"
        );
        self.declare_function(node, scope, raw, source);
        self.declare_variable(node, scope, raw, source);
        self.declare_import_or_type(node, scope, source);
        let nested_scope = if function
            || matches!(
                kind,
                "statement_block"
                    | "for_statement"
                    | "for_in_statement"
                    | "class_body"
                    | "switch_body"
            ) {
            self.add_scope(node, scope, function)
        } else {
            scope
        };
        if function {
            self.declare_parameters(node, nested_scope, source);
        }
        for child in named_children(node) {
            self.scan(child, nested_scope, raw, source);
        }
    }

    fn declare_function(&mut self, node: Node, scope: usize, raw: &RawFile, source: &[u8]) {
        let kind = node.kind();
        if matches!(
            kind,
            "function_declaration" | "generator_function_declaration"
        ) {
            let body_index = raw
                .bodies
                .iter()
                .position(|body| body.callable == node_span(node));
            let role = if namespace_owner(node) {
                Role::Other
            } else {
                body_index.map_or(Role::Other, Role::Body)
            };
            if let Some(name) = node.child_by_field_name("name") {
                self.add_binding(
                    name,
                    compiler_span(node),
                    scope,
                    role,
                    source,
                    is_default(node),
                );
            } else if is_default(node) {
                self.bindings.push(Binding {
                    name: "default".to_owned(),
                    declaration: compiler_span(node),
                    scope,
                    role,
                    default_export: true,
                });
            }
        }
        if kind == "function_signature" {
            if let Some(name) = node.child_by_field_name("name") {
                self.signatures.push(Signature {
                    span: compiler_span(node),
                    name: node_text(name, source).to_owned(),
                    scope,
                });
            }
        }
    }

    fn declare_variable(&mut self, node: Node, scope: usize, raw: &RawFile, source: &[u8]) {
        let kind = node.kind();
        if kind == "variable_declarator" {
            if let Some(name) = node.child_by_field_name("name") {
                let value = node.child_by_field_name("value");
                let body = value.and_then(|value| {
                    raw.bodies
                        .iter()
                        .position(|body| body.callable == node_span(value))
                });
                let constant = node
                    .parent()
                    .is_some_and(|parent| has_token(parent, "const"));
                let role = if constant && name.kind() == "identifier" && !namespace_owner(node) {
                    body.map_or(Role::Other, Role::Body)
                } else {
                    Role::Other
                };
                let mut binding_scope = scope;
                if node.parent().is_some_and(|parent| has_token(parent, "var")) {
                    while !self.scopes[binding_scope].function {
                        binding_scope = self.scopes[binding_scope]
                            .parent
                            .expect("scope has a function/program ancestor");
                    }
                }
                self.pattern(name, node_span(node), binding_scope, role, source);
            }
        }
    }

    fn declare_import_or_type(&mut self, node: Node, scope: usize, source: &[u8]) {
        let kind = node.kind();
        if kind == "import_clause" {
            for child in named_children(node) {
                if child.kind() == "identifier" {
                    let role = if type_only_import(node) {
                        Role::Other
                    } else {
                        Role::Import
                    };
                    self.add_binding(child, node_span(node), scope, role, source, false);
                }
            }
        }
        if matches!(kind, "import_specifier" | "namespace_import") {
            let name = node
                .child_by_field_name("alias")
                .or_else(|| node.child_by_field_name("name"))
                .or_else(|| {
                    named_children(node)
                        .into_iter()
                        .find(|child| child.kind() == "identifier")
                });
            if let Some(name) = name.filter(|name| name.kind() == "identifier") {
                let role = if kind == "namespace_import"
                    || type_only_import(node)
                    || has_token(node, "type")
                {
                    Role::Other
                } else {
                    Role::Import
                };
                self.add_binding(name, node_span(node), scope, role, source, false);
            }
        }
        if matches!(
            kind,
            "class_declaration" | "enum_declaration" | "internal_module" | "module"
        ) {
            if let Some(name) = node
                .child_by_field_name("name")
                .filter(|name| name.kind() == "identifier")
            {
                self.add_binding(name, node_span(node), scope, Role::Other, source, false);
            }
        }
    }

    fn declare_parameters(&mut self, node: Node, nested_scope: usize, source: &[u8]) {
        let kind = node.kind();
        if let Some(parameters) = node.child_by_field_name("parameters") {
            for parameter in named_children(parameters) {
                self.pattern(
                    parameter,
                    node_span(parameter),
                    nested_scope,
                    Role::Other,
                    source,
                );
            }
        }
        if let Some(parameter) = node.child_by_field_name("parameter") {
            self.pattern(
                parameter,
                node_span(parameter),
                nested_scope,
                Role::Other,
                source,
            );
        }
        if matches!(kind, "function_expression" | "generator_function") {
            if let Some(name) = node.child_by_field_name("name") {
                self.add_binding(
                    name,
                    node_span(node),
                    nested_scope,
                    Role::Other,
                    source,
                    false,
                );
            }
        }
    }

    fn pattern(&mut self, node: Node, declaration: Span, scope: usize, role: Role, source: &[u8]) {
        match node.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                self.add_binding(node, declaration, scope, role, source, false);
            }
            "required_parameter" | "optional_parameter" => {
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    self.pattern(pattern, declaration, scope, role, source);
                }
            }
            "pair_pattern" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.pattern(value, declaration, scope, role, source);
                }
            }
            "assignment_pattern" | "object_assignment_pattern" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.pattern(left, declaration, scope, role, source);
                }
            }
            "object_pattern" | "array_pattern" | "rest_pattern" => {
                for child in named_children(node) {
                    self.pattern(child, declaration, scope, role, source);
                }
            }
            _ => {}
        }
    }

    fn lookup(&self, name: &str, byte: usize) -> Option<usize> {
        let mut scope = self
            .scopes
            .iter()
            .enumerate()
            .filter(|(_, scope)| scope.span.start_byte <= byte && byte < scope.span.end_byte)
            .min_by_key(|(_, scope)| scope.span.end_byte - scope.span.start_byte)
            .map(|(index, _)| index);
        while let Some(index) = scope {
            let bindings: Vec<_> = self
                .bindings
                .iter()
                .enumerate()
                .filter(|(_, binding)| binding.scope == index && binding.name == name)
                .collect();
            if !bindings.is_empty() {
                return (bindings.len() == 1).then_some(bindings[0].0);
            }
            scope = self.scopes[index].parent;
        }
        None
    }

    fn scan_writes(&mut self, node: Node, source: &[u8]) {
        let target = match node.kind() {
            "assignment_expression" | "augmented_assignment_expression" => {
                node.child_by_field_name("left")
            }
            "update_expression" => node.child_by_field_name("argument"),
            "for_in_statement" => node.child_by_field_name("left").filter(|left| {
                !matches!(left.kind(), "lexical_declaration" | "variable_declaration")
            }),
            _ => None,
        };
        if let Some(target) = target {
            self.write_pattern(target, source);
        }
        if node.kind() == "assignment_expression" {
            if let Some(left) = node.child_by_field_name("left") {
                if left.kind() == "member_expression" {
                    if let Some(object) = left.child_by_field_name("object") {
                        let name = node_text(object, source);
                        if matches!(name, "module" | "exports")
                            && self.lookup(name, object.start_byte()).is_none()
                        {
                            self.module = true;
                        }
                    }
                }
            }
        }
        for child in named_children(node) {
            self.scan_writes(child, source);
        }
    }

    fn write_pattern(&mut self, node: Node, source: &[u8]) {
        let node = transparent_expression(node);
        match node.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                if let Some(binding) = self.lookup(node_text(node, source), node.start_byte()) {
                    self.writes.insert(binding);
                }
            }
            "pair_pattern" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.write_pattern(value, source);
                }
            }
            "assignment_pattern" | "object_assignment_pattern" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.write_pattern(left, source);
                }
            }
            "object_pattern" | "array_pattern" | "rest_pattern" | "parenthesized_expression" => {
                for child in named_children(node) {
                    self.write_pattern(child, source);
                }
            }
            _ => {}
        }
    }
}

fn named_children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn node_text<'a>(node: Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).expect("source parser checked UTF-8")
}

fn node_span(node: Node) -> Span {
    executable_span(node)
}

fn has_token(node: Node, token: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == token)
}

fn is_default(node: Node) -> bool {
    node.parent()
        .is_some_and(|parent| parent.kind() == "export_statement" && has_token(parent, "default"))
}

fn type_only_import(mut node: Node) -> bool {
    loop {
        if node.kind() == "import_statement" {
            return has_token(node, "type");
        }
        let Some(parent) = node.parent() else {
            return false;
        };
        node = parent;
    }
}

fn namespace_owner(mut node: Node) -> bool {
    while let Some(parent) = node.parent() {
        if matches!(
            parent.kind(),
            "function_declaration"
                | "generator_function_declaration"
                | "function_expression"
                | "generator_function"
                | "arrow_function"
                | "method_definition"
        ) {
            return false;
        }
        if matches!(parent.kind(), "internal_module" | "module") {
            return true;
        }
        node = parent;
    }
    false
}

fn transparent_expression(mut node: Node) -> Node {
    loop {
        let next = match node.kind() {
            "parenthesized_expression"
            | "as_expression"
            | "satisfies_expression"
            | "non_null_expression" => node.named_child(0),
            "type_assertion" => node.named_child(node.named_child_count().saturating_sub(1)),
            _ => return node,
        };
        let Some(next) = next else {
            return node;
        };
        node = next;
    }
}

fn is_eval(node: Node, source: &[u8]) -> bool {
    let node = transparent_expression(node);
    node.kind() == "identifier" && node_text(node, source) == "eval"
}

pub struct AdmittedCall {
    pub path: String,
    pub span: Span,
    pub caller_definition: usize,
    pub target_path: String,
    pub target_definition: usize,
}

pub struct AdmissionResult {
    pub calls: Vec<AdmittedCall>,
    pub reasons: BTreeMap<(String, usize, usize), String>,
}

pub fn admit(
    evidence: &CompilerEvidence,
    sources: &BTreeMap<String, Vec<u8>>,
    parsed: &BTreeMap<String, RawFile>,
) -> Result<AdmissionResult> {
    let diagnostics = checked_diagnostics(evidence)?;
    let policies: BTreeMap<_, _> = parsed
        .iter()
        .map(|(path, raw)| (path.clone(), FilePolicy::new(raw, &sources[path], path)))
        .collect();
    let aliases: Vec<Alias> = evidence
        .aliases
        .iter()
        .map(|value| serde_json::from_value(value.clone()))
        .collect::<std::result::Result<_, _>>()?;
    let alias_targets = alias_targets(&aliases, sources, parsed, &policies);
    let mut written = BTreeSet::new();
    for (path, policy) in &policies {
        for index in &policy.writes {
            let key = BindingKey {
                path: path.clone(),
                index: *index,
            };
            written.insert(key.clone());
            if let Some(targets) = alias_targets.get(&key) {
                written.extend(targets.iter().cloned());
            }
        }
    }
    let common_js: BTreeSet<_> = evidence
        .program_files
        .iter()
        .filter(|file| file.common_js)
        .filter_map(|file| file.path.strip_prefix("{source}/").map(str::to_owned))
        .collect();
    let mut result = AdmissionResult {
        calls: Vec::new(),
        reasons: BTreeMap::new(),
    };
    for value in &evidence.sites {
        let site: Site =
            serde_json::from_value(value.clone()).context("malformed compiler call evidence")?;
        if !parsed.contains_key(&site.location.path) {
            continue;
        }
        let key = (
            site.location.path.clone(),
            site.location.start_byte,
            site.location.end_byte,
        );
        ensure!(
            result
                .reasons
                .insert(key, site.admission.reason.clone())
                .is_none(),
            "duplicate compiler call evidence"
        );
        if diagnostics.iter().any(|diagnostic| {
            diagnostic.overlaps(&site.location)
                || site
                    .admission
                    .target
                    .as_ref()
                    .is_some_and(|target| diagnostic.overlaps(target))
        }) {
            continue;
        }
        if let Some(call) = admit_site(
            &site,
            sources,
            parsed,
            &policies,
            &alias_targets,
            &written,
            &common_js,
        ) {
            result.calls.push(call);
        }
    }
    Ok(result)
}

fn alias_targets(
    aliases: &[Alias],
    sources: &BTreeMap<String, Vec<u8>>,
    parsed: &BTreeMap<String, RawFile>,
    policies: &BTreeMap<String, FilePolicy>,
) -> BTreeMap<BindingKey, BTreeSet<BindingKey>> {
    let mut result = BTreeMap::new();
    for alias in aliases {
        let Some(policy) = policies.get(&alias.declaration.path) else {
            continue;
        };
        let Some(source) = sources.get(&alias.declaration.path) else {
            continue;
        };
        for (index, binding) in policy.bindings.iter().enumerate() {
            if matches!(binding.role, Role::Import)
                && alias.declaration.matches(source, binding.declaration)
            {
                let targets = alias
                    .targets
                    .iter()
                    .filter_map(|location| binding_at(location, sources, parsed, policies))
                    .collect();
                result.insert(
                    BindingKey {
                        path: alias.declaration.path.clone(),
                        index,
                    },
                    targets,
                );
            }
        }
    }
    result
}

fn binding_at(
    location: &Location,
    sources: &BTreeMap<String, Vec<u8>>,
    parsed: &BTreeMap<String, RawFile>,
    policies: &BTreeMap<String, FilePolicy>,
) -> Option<BindingKey> {
    let policy = policies.get(&location.path)?;
    let source = sources.get(&location.path)?;
    if parsed.get(&location.path)?.parse_errors != 0 {
        return None;
    }
    let matches: Vec<_> = policy
        .bindings
        .iter()
        .enumerate()
        .filter(|(_, binding)| {
            matches!(binding.role, Role::Body(index) if location.kind == if binding.declaration == parsed[&location.path].bodies[index].compiler_callable { "FunctionDeclaration" } else { "VariableDeclaration" })
                && location.matches(source, binding.declaration)
        })
        .collect();
    (matches.len() == 1).then(|| BindingKey {
        path: location.path.clone(),
        index: matches[0].0,
    })
}

fn matches_callable(location: &Location, source: &[u8], raw: &RawFile, body: &BodyAnchor) -> bool {
    let Some(node) = raw
        .tree
        .root_node()
        .descendant_for_byte_range(body.callable.start_byte, body.callable.end_byte)
    else {
        return false;
    };
    if node_span(node) != body.callable {
        return false;
    }
    let kind = match node.kind() {
        "function_declaration" | "generator_function_declaration" => "FunctionDeclaration",
        "function_expression" | "generator_function" => "FunctionExpression",
        "arrow_function" => "ArrowFunction",
        "method_definition" if has_token(node, "get") => "GetAccessor",
        "method_definition" if has_token(node, "set") => "SetAccessor",
        "method_definition"
            if !has_token(node, "static")
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "class_body")
                && node
                    .child_by_field_name("name")
                    .is_some_and(|name| node_text(name, source) == "constructor") =>
        {
            "Constructor"
        }
        "method_definition" => "MethodDeclaration",
        _ => return false,
    };
    location.kind == kind
        && (location.matches(source, body.callable)
            || location.matches(source, body.compiler_callable))
}

fn call_witness<'a>(
    site: &Site,
    source: &[u8],
    raw: &'a RawFile,
    policy: &FilePolicy,
) -> Option<(&'a crate::js_ts_syntax::CallAnchor, &'a BodyAnchor, usize)> {
    let call = raw.call_anchors.iter().find(|call| {
        site.location.matches(source, call.span)
            && site.callee_location.matches(source, call.callee)
    })?;
    let expected_kind = if call.tagged {
        "TaggedTemplateExpression"
    } else {
        "CallExpression"
    };
    if call.constructor
        || site.location.kind != expected_kind
        || site.callee_location.kind != "Identifier"
        || site.callee_name != site.callee_location
    {
        return None;
    }
    let callee =
        std::str::from_utf8(source.get(call.callee.start_byte..call.callee.end_byte)?).ok()?;
    if callee != site.callee || site.callee_location.path != site.location.path {
        return None;
    }
    let caller_location = site.admission.caller.as_ref()?;
    if site.caller.as_ref()? != caller_location || caller_location.path != site.location.path {
        return None;
    }
    let owner = raw.bodies.iter().find(|body| {
        call.caller_body == Some(body.body) && matches_callable(caller_location, source, raw, body)
    })?;
    Some((call, owner, policy.lookup(callee, call.callee.start_byte)?))
}

fn admit_site(
    site: &Site,
    sources: &BTreeMap<String, Vec<u8>>,
    parsed: &BTreeMap<String, RawFile>,
    policies: &BTreeMap<String, FilePolicy>,
    aliases: &BTreeMap<BindingKey, BTreeSet<BindingKey>>,
    written: &BTreeSet<BindingKey>,
    common_js: &BTreeSet<String>,
) -> Option<AdmittedCall> {
    if site.admission.reason != "proven_binding_candidate"
        || site.callee_any
        || site
            .admission
            .mutations
            .as_ref()
            .is_some_and(|writes| !writes.is_empty())
    {
        return None;
    }
    if [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|extension| site.location.path.to_ascii_lowercase().ends_with(extension))
    {
        return None;
    }
    let source = sources.get(&site.location.path)?;
    let raw = parsed.get(&site.location.path)?;
    let policy = policies.get(&site.location.path)?;
    if raw.parse_errors != 0 || policy.dynamic || policy.escaped_bindings {
        return None;
    }
    let (call, owner, callee_index) = call_witness(site, source, raw, policy)?;
    let target = site.admission.target.as_ref()?;
    let target_body = site.admission.target_body.as_ref()?;
    if target_body.path != target.path {
        return None;
    }
    if target.path != site.location.path
        && (common_js.contains(&target.path) || common_js.contains(&site.location.path))
    {
        return None;
    }
    if [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|extension| target.path.to_ascii_lowercase().ends_with(extension))
    {
        return None;
    }
    let target_raw = parsed.get(&target.path)?;
    let target_source = sources.get(&target.path)?;
    let target_policy = policies.get(&target.path)?;
    if target_raw.parse_errors != 0 || target_policy.dynamic || target_policy.escaped_bindings {
        return None;
    }
    let body = target_raw.bodies.iter().find(|body| {
        target_body.matches(target_source, body.body)
            && matches_callable(target, target_source, target_raw, body)
    })?;
    let target_binding = target_policy.bindings.iter().enumerate().find(|(_, binding)| matches!(binding.role, Role::Body(index) if target_raw.bodies[index].definition == body.definition))?;
    if target_policy
        .bindings
        .iter()
        .filter(|binding| {
            binding.scope == target_binding.1.scope && binding.name == target_binding.1.name
        })
        .count()
        != 1
    {
        return None;
    }
    if target_binding.1.scope == 0 && !target_policy.module {
        return None;
    }
    let target_key = BindingKey {
        path: target.path.clone(),
        index: target_binding.0,
    };
    if written.contains(&target_key) {
        return None;
    }
    let symbol = site.symbol.as_deref()?;
    if symbol != target_binding.1.name && !(symbol == "default" && target_binding.1.default_export)
    {
        return None;
    }
    let callee_key = BindingKey {
        path: site.location.path.clone(),
        index: callee_index,
    };
    if written.contains(&callee_key) {
        return None;
    }
    match policy.bindings[callee_index].role {
        Role::Body(_) if callee_key == target_key => {}
        Role::Import
            if aliases
                .get(&callee_key)
                .is_some_and(|targets| targets.len() == 1 && targets.contains(&target_key)) => {}
        _ => return None,
    }
    compiler_target_witness(site, sources, parsed, policies, &target_key, body)?;
    Some(AdmittedCall {
        path: site.location.path.clone(),
        span: call.span,
        caller_definition: owner.definition,
        target_path: target.path.clone(),
        target_definition: body.definition,
    })
}

fn compiler_target_witness(
    site: &Site,
    sources: &BTreeMap<String, Vec<u8>>,
    parsed: &BTreeMap<String, RawFile>,
    policies: &BTreeMap<String, FilePolicy>,
    target_key: &BindingKey,
    body: &BodyAnchor,
) -> Option<()> {
    let target_source = sources.get(&target_key.path)?;
    let target_raw = parsed.get(&target_key.path)?;
    let target_policy = policies.get(&target_key.path)?;
    let binding = target_policy.bindings.get(target_key.index)?;
    let concrete: Vec<_> = site
        .declarations
        .iter()
        .filter(|declaration| declaration.callable.is_some() && declaration.body.is_some())
        .collect();
    if concrete.len() != 1 {
        return None;
    }
    let declaration = concrete[0];
    if binding_at(&declaration.location, sources, parsed, policies)? != *target_key
        || declaration.callable.as_ref()? != site.admission.target.as_ref()?
        || declaration.body.as_ref()? != site.admission.target_body.as_ref()?
    {
        return None;
    }
    let signature = site.admission.signature.as_ref()?;
    if site.signature.as_ref()? != signature || signature.path != target_key.path {
        return None;
    }
    let implementation = matches_callable(signature, target_source, target_raw, body);
    let overload = signature.kind == "FunctionDeclaration"
        && target_policy.signatures.iter().any(|candidate| {
            candidate.scope == binding.scope
                && candidate.name == binding.name
                && signature.matches(target_source, candidate.span)
        });
    if !implementation && !overload {
        return None;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_scope_rejects_parameters_and_hoisted_redeclarations() {
        let source =
            b"export function known() {}\nexport function caller(known: () => void) { known(); }";
        let raw = crate::js_ts_syntax::extract_file("scope.ts", source).unwrap();
        let policy = FilePolicy::new(&raw, source, "scope.ts");
        let call = raw.call_anchors.first().unwrap();
        let binding = policy.lookup("known", call.span.start_byte).unwrap();
        assert!(matches!(policy.bindings[binding].role, Role::Other));

        let source = b"export function known() {}\nif (true) { var known = () => 1; }\nexport function caller() { known(); }";
        let raw = crate::js_ts_syntax::extract_file("scope.ts", source).unwrap();
        let policy = FilePolicy::new(&raw, source, "scope.ts");
        assert_eq!(
            policy
                .bindings
                .iter()
                .filter(|binding| binding.scope == 0 && binding.name == "known")
                .count(),
            2
        );
        assert!(
            policy
                .lookup("known", raw.call_anchors[0].span.start_byte)
                .is_none()
        );
    }

    #[test]
    fn source_writes_follow_lexical_bindings_and_wrapped_eval_is_dynamic() {
        let source = b"export function known() {}\nfunction mutate() { known = () => 2; }\nexport function caller() { known(); }";
        let raw = crate::js_ts_syntax::extract_file("scope.ts", source).unwrap();
        let policy = FilePolicy::new(&raw, source, "scope.ts");
        let known = policy
            .bindings
            .iter()
            .position(|binding| binding.name == "known")
            .unwrap();
        assert!(policy.writes.contains(&known));
        for assigned in [
            "(known)",
            "(known as typeof known)",
            "(<typeof known>known)",
            "known!",
        ] {
            let source = format!(
                "export function known() {{}}\n{assigned} = () => 2;\nexport function caller() {{ known(); }}"
            );
            let raw = crate::js_ts_syntax::extract_file("scope.ts", source.as_bytes()).unwrap();
            assert_eq!(raw.parse_errors, 0, "{assigned}");
            let policy = FilePolicy::new(&raw, source.as_bytes(), "scope.ts");
            let known = policy
                .bindings
                .iter()
                .position(|binding| binding.name == "known")
                .unwrap();
            assert!(policy.writes.contains(&known), "{assigned}");
        }
        for callee in [
            "(eval)",
            "((eval))",
            "(eval as typeof eval)",
            "(<typeof eval>eval)",
        ] {
            let source = format!(
                "export function known() {{}}\nexport function caller() {{ {callee}('known=()=>2'); known(); }}"
            );
            let raw = crate::js_ts_syntax::extract_file("scope.ts", source.as_bytes()).unwrap();
            assert_eq!(raw.parse_errors, 0, "{callee}");
            assert!(
                FilePolicy::new(&raw, source.as_bytes(), "scope.ts").dynamic,
                "{callee}"
            );
        }
    }

    #[test]
    fn export_wrapper_and_unicode_offsets_are_exact_witnesses() {
        let source = "// π\nexport function known() { return 1; }";
        let raw = crate::js_ts_syntax::extract_file("source.ts", source.as_bytes()).unwrap();
        let body = &raw.bodies[0];
        assert_ne!(body.callable.start_byte, body.compiler_callable.start_byte);
        let location = Location {
            path: "source.ts".to_owned(),
            start: source[..body.compiler_callable.start_byte]
                .encode_utf16()
                .count(),
            end: source[..body.compiler_callable.end_byte]
                .encode_utf16()
                .count(),
            start_byte: body.compiler_callable.start_byte,
            end_byte: body.compiler_callable.end_byte,
            line: 2,
            kind: "FunctionDeclaration".to_owned(),
        };
        assert!(matches_callable(&location, source.as_bytes(), &raw, body));
        let mut wrong = location;
        wrong.start += 1;
        assert!(!matches_callable(&wrong, source.as_bytes(), &raw, body));
    }
    #[test]
    #[allow(clippy::literal_string_with_formatting_args)] // Captured logical root, not interpolation.
    fn contradictory_diagnostics_cannot_upgrade_exact_candidate_anchors() {
        use serde_json::json;
        let source = "export function target() {}\nexport function caller() { target(); }\n";
        let raw = crate::js_ts_syntax::extract_file("source.ts", source.as_bytes()).unwrap();
        let location = |span: Span, kind: &str| {
            json!({
                "path": "source.ts", "start": source[..span.start_byte].encode_utf16().count(),
                "end": source[..span.end_byte].encode_utf16().count(), "start_byte": span.start_byte,
                "end_byte": span.end_byte, "line": span.start_line, "kind": kind,
            })
        };
        let target = location(raw.bodies[0].compiler_callable, "FunctionDeclaration");
        let body = location(raw.bodies[0].body, "Block");
        let owner = location(raw.bodies[1].compiler_callable, "FunctionDeclaration");
        let call = location(raw.call_anchors[0].span, "CallExpression");
        let callee = location(raw.call_anchors[0].callee, "Identifier");
        let mut evidence: CompilerEvidence = serde_json::from_value(json!({
            "phase": "result", "schema": "js-ts-helper/1", "compiler": "5.9.3", "node": "v22.22.2",
            "project": "tsconfig.json", "options": {}, "inputs": {}, "lookups": [], "aliases": [],
            "program_files": [{"path": "{source}/source.ts", "declaration": false, "common_js": false}],
            "program_diagnostics": {"options": 0, "global": 0, "syntactic": 0}, "diagnostics": [],
            "sites": [{"location": call, "callee": "target", "callee_location": callee,
                "callee_name": callee, "callee_any": false, "caller": owner, "symbol": "target",
                "declarations": [{"path": "source.ts", "start": target["start"], "end": target["end"],
                    "start_byte": target["start_byte"], "end_byte": target["end_byte"], "line": 1,
                    "kind": "FunctionDeclaration", "callable": target, "body": body}],
                "signature": target, "admission": {"reason": "proven_binding_candidate",
                    "target": target, "target_body": body, "caller": owner, "signature": target}}]
        })).unwrap();
        let sources = BTreeMap::from([("source.ts".to_owned(), source.as_bytes().to_vec())]);
        let parsed = BTreeMap::from([("source.ts".to_owned(), raw)]);
        assert_eq!(admit(&evidence, &sources, &parsed).unwrap().calls.len(), 1);
        for start in [
            call["start"].as_u64().unwrap(),
            target["start"].as_u64().unwrap(),
        ] {
            evidence.diagnostics =
                vec![json!({"code": 2345, "path": "source.ts", "start": start, "length": 1})];
            assert!(
                admit(&evidence, &sources, &parsed)
                    .unwrap()
                    .calls
                    .is_empty()
            );
        }
        for diagnostic in [
            json!({"code": 2307, "path": "other.ts", "start": 0}),
            json!({"code": 2345}),
            json!({"code": 2345, "path": "source.ts", "start": usize::MAX, "length": 1}),
            json!({"code": 2345, "path": "source.ts", "start": 0, "unexpected": true}),
        ] {
            evidence.diagnostics = vec![diagnostic];
            assert!(admit(&evidence, &sources, &parsed).is_err());
        }
        evidence.diagnostics.clear();
        evidence.program_diagnostics.syntactic = 1;
        assert!(admit(&evidence, &sources, &parsed).is_err());
    }
}
