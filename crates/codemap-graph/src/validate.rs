//! Snapshot validator: enforces the truth boundary on a complete snapshot.
//!
//! Checks (per plan P0 gates):
//! - no duplicate node or edge IDs;
//! - no dangling edge endpoints (`from`/`to` must reference emitted nodes);
//! - every span is non-empty and ordered (`start_byte <= end_byte`);
//! - file-hash provenance matches the snapshot's file records;
//! - language purity (every node ID must live in the snapshot's declared
//!   language namespace);
//! - `calls` edges exist only with the declared language's authoritative
//!   semantic source (`RustAnalyzer` / `RubyLsp`) and a matching semantic
//!   identity block;
//! - measures, when present, name a counting policy the language defines,
//!   cite existing callable and file nodes, cover every one of them, and
//!   add up (`sloc <= loc` per callable, `blank + comment + sloc == loc`
//!   per file).
//!
//! Returns a single aggregated error so a consumer sees all violations.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Result, bail};

use crate::schema::{
    CARGO_FEATURE_POLICY, CLONE_MIN_LINES, CLONE_MIN_TOKENS, CallSiteCategory, EdgeKind,
    FactSource, FileRecord, Language, NodeKind, Provenance, RustAnalyzerPolicy, Snapshot,
};

/// Validate a complete snapshot. Returns `Ok(())` when all gates pass.
pub fn validate(snapshot: &Snapshot) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    // Identity consistency.
    let expected_id = snapshot.identity.compute_snapshot_id();
    if snapshot.identity.snapshot_id != expected_id {
        errors.push(format!(
            "snapshot_id mismatch: identity says {}, computed {}",
            snapshot.identity.snapshot_id, expected_id
        ));
    }
    if snapshot.identity.schema_version != crate::schema::SCHEMA_VERSION {
        errors.push(format!(
            "schema_version mismatch: {} != {}",
            snapshot.identity.schema_version,
            crate::schema::SCHEMA_VERSION
        ));
    }
    validate_tool_identity(snapshot, &mut errors);
    let expected_structure = crate::schema::structure_policy(snapshot.identity.language);
    if snapshot.identity.structure_policy.as_deref() != expected_structure {
        errors.push(format!(
            "structure_policy {:?} is not the {} policy {expected_structure:?}",
            snapshot.identity.structure_policy,
            snapshot.identity.language.tag()
        ));
    }

    // Exact path -> file identity map. A hash existing elsewhere in the
    // snapshot is not sufficient provenance for this path.
    let mut files_by_path: HashMap<&str, &FileRecord> = HashMap::new();
    for f in &snapshot.files {
        if files_by_path.insert(&f.rel_path, f).is_some() {
            errors.push(format!("duplicate file record: {}", f.rel_path));
        }
    }

    let mut node_ids: HashSet<&str> = HashSet::new();
    validate_nodes(snapshot, &files_by_path, &mut node_ids, &mut errors);
    validate_edges(snapshot, &files_by_path, &node_ids, &mut errors);
    validate_python_nodes(snapshot, &files_by_path, &mut errors);
    validate_call_sites(snapshot, &files_by_path, &mut errors);
    validate_profiled_census(snapshot, &mut errors);
    validate_measures(snapshot, &mut errors);
    validate_twins(snapshot, &mut errors);
    validate_report(snapshot, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        bail!("snapshot validation failed:\n  - {}", errors.join("\n  - "));
    }
}

fn validate_python_nodes(
    snapshot: &Snapshot,
    files: &HashMap<&str, &FileRecord>,
    errors: &mut Vec<String>,
) {
    for node in &snapshot.nodes {
        let definition = matches!(
            node.kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Type(crate::schema::TypeKind::Class)
        );
        if snapshot.identity.language != Language::Python {
            if node.python.is_some() {
                errors.push(format!(
                    "Python definition observations on another language node {}",
                    node.id
                ));
            }
            continue;
        }
        if definition != node.python.is_some() {
            errors.push(format!(
                "Python definition observations disagree with node kind {}",
                node.id
            ));
        }
        if std::path::Path::new(&node.provenance.rel_path).extension()
            == Some(std::ffi::OsStr::new("pyi"))
            && !matches!(node.kind, NodeKind::File | NodeKind::Module)
        {
            errors.push(format!(
                "Python declaration input contains a body/import node {}",
                node.id
            ));
        }
        if !matches!(node.kind, NodeKind::File | NodeKind::Module)
            && node.provenance.span.start_byte >= node.provenance.span.end_byte
        {
            errors.push(format!(
                "empty Python source definition/import span on {}",
                node.id
            ));
        }
        if let Some(observation) = &node.python {
            if observation.is_async && !matches!(node.kind, NodeKind::Function | NodeKind::Method)
                || !observation.bases.is_empty()
                    && node.kind != NodeKind::Type(crate::schema::TypeKind::Class)
            {
                errors.push(format!(
                    "invalid Python async/base observations on {}",
                    node.id
                ));
            }
            let mut previous = 0;
            for expression in &observation.decorators {
                validate_python_expression(node, expression, snapshot, files, errors);
                let span = expression.provenance.span;
                if span.start_byte < previous || span.end_byte > node.provenance.span.start_byte {
                    errors.push(format!(
                        "Python decorator order/span outside definition {}",
                        node.id
                    ));
                }
                previous = span.end_byte;
            }
            previous = node.provenance.span.start_byte;
            for expression in &observation.bases {
                validate_python_expression(node, expression, snapshot, files, errors);
                let span = expression.provenance.span;
                if span.start_byte < previous || span.end_byte > node.provenance.span.end_byte {
                    errors.push(format!("Python base order/span outside class {}", node.id));
                }
                previous = span.end_byte;
            }
        }
    }
    if snapshot.identity.language == Language::Python {
        for file in &snapshot.files {
            let extension = std::path::Path::new(&file.rel_path).extension();
            if extension != Some(std::ffi::OsStr::new("py"))
                && !(extension == Some(std::ffi::OsStr::new("pyi"))
                    && snapshot.identity.is_semantic())
            {
                errors.push(format!(
                    "Python snapshot contains non-.py source {}",
                    file.rel_path
                ));
            }
        }
        for site in &snapshot.call_sites {
            if std::path::Path::new(&site.rel_path).extension() == Some(std::ffi::OsStr::new("pyi"))
                && site.category != crate::schema::CallSiteCategory::Unsupported
            {
                errors.push("Python declaration call site must remain unsupported".to_owned());
            }
        }
        validate_python_parents(snapshot, errors);
    }
}

fn validate_python_expression(
    node: &crate::schema::Node,
    expression: &crate::schema::PythonExpression,
    snapshot: &Snapshot,
    files: &HashMap<&str, &FileRecord>,
    errors: &mut Vec<String>,
) {
    validate_provenance(
        "Python expression",
        &node.id,
        &expression.provenance,
        snapshot,
        files,
        errors,
    );
    if expression.spelling.is_empty()
        || expression.provenance.rel_path != node.provenance.rel_path
        || expression.provenance.fact_source != FactSource::Cpython
        || expression.provenance.confidence != u8::MAX
    {
        errors.push(format!(
            "invalid Python expression provenance on {}",
            node.id
        ));
    }
}

fn validate_python_parents(snapshot: &Snapshot, errors: &mut Vec<String>) {
    let nodes: HashMap<_, _> = snapshot
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut parents = HashMap::new();
    for edge in snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Contains)
    {
        if parents
            .insert(edge.to.as_str(), edge.from.as_str())
            .is_some()
        {
            errors.push(format!("multiple Python lexical parents of {}", edge.to));
        }
    }
    validate_python_source_tree(snapshot, &parents, errors);
    let mut groups: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for node in &snapshot.nodes {
        if node.kind == NodeKind::File {
            continue;
        }
        let Some(parent) = parents.get(node.id.as_str()).and_then(|id| nodes.get(id)) else {
            errors.push(format!("missing Python lexical parent of {}", node.id));
            continue;
        };
        let span = node.provenance.span;
        let start = node
            .python
            .as_ref()
            .and_then(|value| value.decorators.first())
            .map_or(span.start_byte, |expression| {
                expression.provenance.span.start_byte
            });
        if parent.provenance.rel_path != node.provenance.rel_path
            || parent.provenance.span.start_byte > start
            || parent.provenance.span.end_byte < span.end_byte
            || !matches!(
                parent.kind,
                NodeKind::File
                    | NodeKind::Module
                    | NodeKind::Function
                    | NodeKind::Method
                    | NodeKind::Type(crate::schema::TypeKind::Class)
            )
        {
            errors.push(format!(
                "Python child outside its lexical parent {}",
                node.id
            ));
        }
        if let Some(observation) = &node.python {
            groups
                .entry((parent.id.as_str(), node.name.as_str()))
                .or_default()
                .push((start, observation));
        }
    }
    for group in groups.values_mut() {
        group.sort_by_key(|(start, _)| *start);
        for (ordinal, (_, observation)) in group.iter().enumerate() {
            if observation.ordinal != ordinal || observation.ambiguous != (group.len() > 1) {
                errors.push("invalid Python repeated-definition accounting".into());
            }
        }
    }
}

fn validate_python_source_tree(
    snapshot: &Snapshot,
    parents: &HashMap<&str, &str>,
    errors: &mut Vec<String>,
) {
    let mut files: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for node in &snapshot.nodes {
        files
            .entry(node.provenance.rel_path.as_str())
            .or_default()
            .push(node);
    }
    for nodes in files.values_mut() {
        nodes.sort_by_key(|node| {
            (
                node.python
                    .as_ref()
                    .and_then(|value| value.decorators.first())
                    .map_or(node.provenance.span.start_byte, |expression| {
                        expression.provenance.span.start_byte
                    }),
                match node.kind {
                    NodeKind::File => 0,
                    NodeKind::Module => 1,
                    _ => 2,
                },
            )
        });
        let mut stack: Vec<&crate::schema::Node> = Vec::new();
        for node in nodes.iter() {
            let start = node
                .python
                .as_ref()
                .and_then(|value| value.decorators.first())
                .map_or(node.provenance.span.start_byte, |expression| {
                    expression.provenance.span.start_byte
                });
            while stack.last().is_some_and(|parent| {
                parent.kind != NodeKind::File && parent.provenance.span.end_byte <= start
            }) {
                stack.pop();
            }
            if node.kind != NodeKind::File {
                let expected = stack.last().map(|parent| parent.id.as_str());
                if parents.get(node.id.as_str()).copied() != expected {
                    errors.push(format!(
                        "incorrect Python source lexical parent of {}",
                        node.id
                    ));
                }
            }
            if matches!(node.kind, NodeKind::File | NodeKind::Module) || node.python.is_some() {
                stack.push(node);
            }
        }
    }
}

fn validate_nodes<'a>(
    snapshot: &'a Snapshot,
    files_by_path: &HashMap<&str, &FileRecord>,
    node_ids: &mut HashSet<&'a str>,
    errors: &mut Vec<String>,
) {
    let prefix = snapshot.identity.language.id_prefix();
    for n in &snapshot.nodes {
        if !node_ids.insert(&n.id) {
            errors.push(format!("duplicate node id: {}", n.id));
        }
        if !n.id.starts_with(prefix) {
            errors.push(format!(
                "node id `{}` outside the `{}` namespace (language purity: snapshot is {})",
                n.id,
                prefix.trim_end_matches(':'),
                snapshot.identity.language.tag()
            ));
        }
        validate_provenance(
            "node",
            &n.id,
            &n.provenance,
            snapshot,
            files_by_path,
            errors,
        );
        let compiled_callable = cpp_semantic(snapshot)
            && n.provenance.fact_source == FactSource::Clangd
            && matches!(n.kind, NodeKind::Function | NodeKind::Method);
        if n.provenance.fact_source != snapshot.identity.language.structural_fact_source()
            && !compiled_callable
        {
            errors.push(format!(
                "node {} has non-authoritative source {:?}",
                n.id, n.provenance.fact_source
            ));
        }
        if n.provenance.confidence != u8::MAX {
            errors.push(format!(
                "node {} has non-canonical confidence {}",
                n.id, n.provenance.confidence
            ));
        }
        if n.kind == crate::schema::NodeKind::File {
            if let Some(file) = files_by_path.get(n.provenance.rel_path.as_str()) {
                if n.provenance.span.start_byte != 0 || n.provenance.span.end_byte != file.byte_len
                {
                    errors.push(format!(
                        "file node {} span {}..{} does not cover complete file bytes 0..{}",
                        n.id,
                        n.provenance.span.start_byte,
                        n.provenance.span.end_byte,
                        file.byte_len
                    ));
                }
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn validate_edges(
    snapshot: &Snapshot,
    files_by_path: &HashMap<&str, &FileRecord>,
    node_ids: &HashSet<&str>,
    errors: &mut Vec<String>,
) {
    let mut edge_ids: HashSet<&str> = HashSet::new();
    let nodes_by_id: HashMap<&str, &crate::schema::Node> = snapshot
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    for e in &snapshot.edges {
        if snapshot.identity.language == Language::Python
            && matches!(
                e.kind,
                EdgeKind::Implements | EdgeKind::Inherits | EdgeKind::Extends
            )
        {
            errors.push(format!(
                "Python edge {} has no accepted relation policy",
                e.id
            ));
        }
        if !edge_ids.insert(&e.id) {
            errors.push(format!("duplicate edge id: {}", e.id));
        }
        if !node_ids.contains(e.from.as_str()) {
            errors.push(format!("dangling edge from: {} (edge {})", e.from, e.id));
        }
        if !node_ids.contains(e.to.as_str()) {
            errors.push(format!("dangling edge to: {} (edge {})", e.to, e.id));
        }
        validate_provenance(
            "edge",
            &e.id,
            &e.provenance,
            snapshot,
            files_by_path,
            errors,
        );
        if e.provenance.confidence != u8::MAX {
            errors.push(format!(
                "edge {} has non-canonical confidence {}",
                e.id, e.provenance.confidence
            ));
        }
        match e.kind {
            EdgeKind::Implements if snapshot.identity.language == Language::Rust => {
                validate_rust_implements(snapshot, &nodes_by_id, e, errors);
            }
            EdgeKind::Calls => {
                let (expected_source, semantic_identity_present) = match snapshot.identity.language
                {
                    Language::Rust => (
                        FactSource::RustAnalyzer,
                        snapshot.identity.tools.rust_analyzer.is_some(),
                    ),
                    Language::Ruby => (
                        FactSource::RubyLsp,
                        snapshot.identity.tools.ruby_lsp.is_some(),
                    ),
                    // PHP semantic resolution is phpactor under the pinned
                    // tool bundle (P2).
                    Language::Php => (
                        FactSource::Phpactor,
                        snapshot.identity.tools.phpactor.is_some(),
                    ),
                    // Go semantic resolution is gopls under the pinned tool
                    // bundle (G2).
                    Language::Go => (FactSource::Gopls, snapshot.identity.tools.gopls.is_some()),
                    Language::Cpp => (FactSource::Clangd, cpp_semantic(snapshot)),
                    Language::JsTs => (FactSource::TypeScript, snapshot.identity.is_semantic()),
                    Language::Python => (FactSource::Mypyc, snapshot.identity.is_semantic()),
                };
                if snapshot.identity.language == Language::Cpp {
                    validate_callable_call(&nodes_by_id, e, "C++", errors);
                }
                if snapshot.identity.language == Language::JsTs {
                    validate_callable_call(&nodes_by_id, e, "JS/TS", errors);
                }
                if snapshot.identity.language == Language::Python {
                    validate_callable_call(&nodes_by_id, e, "Python", errors);
                }
                if e.provenance.fact_source != expected_source {
                    errors.push(format!(
                        "calls edge {} has non-authoritative source {:?}",
                        e.id, e.provenance.fact_source
                    ));
                } else if !semantic_identity_present {
                    errors.push(format!(
                        "calls edge {} is present in a syntax-only snapshot",
                        e.id
                    ));
                }
            }
            EdgeKind::Contains
            | EdgeKind::Imports
            | EdgeKind::Implements
            | EdgeKind::Inherits
            | EdgeKind::Extends
                if e.provenance.fact_source
                    != snapshot.identity.language.structural_fact_source()
                    && !cpp_compiled_contains(snapshot, &nodes_by_id, e) =>
            {
                errors.push(format!(
                    "structural edge {} has non-authoritative source {:?}",
                    e.id, e.provenance.fact_source
                ));
            }
            _ => {}
        }
    }
}

fn validate_rust_implements(
    snapshot: &Snapshot,
    nodes_by_id: &HashMap<&str, &crate::schema::Node>,
    e: &crate::schema::Edge,
    errors: &mut Vec<String>,
) {
    let source = e.provenance.fact_source;
    if source != FactSource::TreeSitter
        && !(source == FactSource::RustAnalyzer && snapshot.identity.tools.rust_analyzer.is_some())
    {
        errors.push(format!(
            "Rust implements edge {} has non-authoritative source or missing semantic identity",
            e.id
        ));
    }
    let concrete_type = nodes_by_id.get(e.from.as_str()).is_some_and(|node| {
        matches!(
            node.kind,
            NodeKind::Type(
                crate::schema::TypeKind::Struct
                    | crate::schema::TypeKind::Enum
                    | crate::schema::TypeKind::Union
            )
        )
    });
    let trait_target = nodes_by_id
        .get(e.to.as_str())
        .is_some_and(|node| node.kind == NodeKind::Trait);
    if !concrete_type || !trait_target {
        errors.push(format!(
            "Rust implements edge {} requires a concrete type and a trait declaration",
            e.id
        ));
    }
}

fn validate_profiled_census(snapshot: &Snapshot, errors: &mut Vec<String>) {
    let label = match snapshot.identity.language {
        Language::Cpp => "C++",
        Language::JsTs => "JS/TS",
        Language::Python => "Python",
        _ => return,
    };
    let mut calls: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for edge in snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
    {
        calls
            .entry((
                edge.provenance.rel_path.as_str(),
                edge.provenance.span.start_byte,
                edge.provenance.span.end_byte,
            ))
            .or_default()
            .push(edge);
    }
    for site in &snapshot.call_sites {
        let key = (
            site.rel_path.as_str(),
            site.span.start_byte,
            site.span.end_byte,
        );
        if let Some(edges) = calls.remove(&key) {
            if site.category != CallSiteCategory::Resolved
                || edges.len() != 1
                || edges[0].provenance.span != site.span
            {
                errors.push(format!(
                    "{label} calls edge contradicts its exact resolved call site"
                ));
            }
        } else if site.category == CallSiteCategory::Resolved {
            errors.push(format!("{label} resolved call site has no exact call edge"));
        }
    }
    if !calls.is_empty() {
        errors.push(format!("{label} call edge has no exact resolved call site"));
    }
}

fn cpp_semantic(snapshot: &Snapshot) -> bool {
    snapshot.identity.language == Language::Cpp && snapshot.identity.is_semantic()
}

fn cpp_compiled_contains(
    snapshot: &Snapshot,
    nodes: &HashMap<&str, &crate::schema::Node>,
    edge: &crate::schema::Edge,
) -> bool {
    if !cpp_semantic(snapshot)
        || edge.kind != EdgeKind::Contains
        || edge.provenance.fact_source != FactSource::Clangd
    {
        return false;
    }
    let parent = nodes.get(edge.from.as_str()).copied();
    let child = nodes.get(edge.to.as_str()).copied();
    match (parent, child) {
        (Some(parent), Some(child)) => {
            parent.id != child.id
                && matches!(
                    parent.kind,
                    NodeKind::File
                        | NodeKind::Module
                        | NodeKind::Type(_)
                        | NodeKind::Function
                        | NodeKind::Method
                )
                && child.provenance.fact_source == FactSource::Clangd
                && matches!(child.kind, NodeKind::Function | NodeKind::Method)
                && edge.provenance == child.provenance
                && parent.provenance.rel_path == child.provenance.rel_path
                && parent.provenance.span.start_byte <= child.provenance.span.start_byte
                && parent.provenance.span.end_byte >= child.provenance.span.end_byte
        }
        _ => false,
    }
}

fn validate_callable_call(
    nodes: &HashMap<&str, &crate::schema::Node>,
    edge: &crate::schema::Edge,
    language: &str,
    errors: &mut Vec<String>,
) {
    let caller = nodes.get(edge.from.as_str()).copied();
    let target = nodes.get(edge.to.as_str()).copied();
    let callable =
        |node: &crate::schema::Node| matches!(node.kind, NodeKind::Function | NodeKind::Method);
    if !caller.is_some_and(callable) || !target.is_some_and(callable) {
        errors.push(format!(
            "{language} calls edge {} must connect callable bodies",
            edge.id
        ));
    }
    if let Some(caller) = caller {
        if edge.provenance.rel_path != caller.provenance.rel_path
            || edge.provenance.span.start_byte < caller.provenance.span.start_byte
            || edge.provenance.span.end_byte > caller.provenance.span.end_byte
        {
            errors.push(format!(
                "{language} calls edge {} lies outside its caller body",
                edge.id
            ));
        }
    }
}

/// Measures are all-or-nothing and fail closed: a snapshot carries them
/// only under a counting policy its language defines, every callable and
/// every file has exactly one record, every record names a node of the
/// right kind, and the line classes add up.
fn validate_measures(snapshot: &Snapshot, errors: &mut Vec<String>) {
    use crate::schema::{NodeKind, measures_policy};

    let has_measures = !snapshot.measures.is_empty() || !snapshot.file_measures.is_empty();
    let language = snapshot.identity.language.tag();
    match (&snapshot.identity.measures_policy, has_measures) {
        (None, false) => return,
        (None, true) => {
            errors.push("measures present but the identity names no measures_policy".into());
            return;
        }
        (Some(policy), false) => {
            errors.push(format!(
                "identity names measures_policy `{policy}` but the snapshot carries no measures"
            ));
            return;
        }
        (Some(policy), true) => match measures_policy(snapshot.identity.language) {
            Some(expected) if policy == expected => {}
            Some(expected) => errors.push(format!(
                "measures_policy `{policy}` is not the {language} counting policy `{expected}`"
            )),
            None => {
                errors.push(format!(
                        "no counting policy is defined for {language}; a measured {language} snapshot cannot be valid"
                    ));
                return;
            }
        },
    }

    let kinds: HashMap<&str, NodeKind> = snapshot
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.kind))
        .collect();
    let is_callable = |kind: NodeKind| {
        matches!(
            kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Macro
        )
    };

    let mut measured: HashSet<&str> = HashSet::new();
    for measure in &snapshot.measures {
        if !measured.insert(&measure.node_id) {
            errors.push(format!("duplicate measure for {}", measure.node_id));
        }
        match kinds.get(measure.node_id.as_str()) {
            Some(kind) if is_callable(*kind) => {}
            Some(kind) => errors.push(format!(
                "measure for {} names a {kind:?} node, not a callable",
                measure.node_id
            )),
            None => errors.push(format!(
                "measure for {} names no node in the snapshot",
                measure.node_id
            )),
        }
        if measure.cc == 0 {
            errors.push(format!(
                "measure for {} has cc 0; cyclomatic complexity starts at 1",
                measure.node_id
            ));
        }
        if measure.sloc > measure.loc {
            errors.push(format!(
                "measure for {} has sloc {} above loc {}",
                measure.node_id, measure.sloc, measure.loc
            ));
        }
        validate_form(snapshot.identity.language, measure, errors);
    }
    for node in &snapshot.nodes {
        if is_callable(node.kind) && !measured.contains(node.id.as_str()) {
            errors.push(format!("callable {} has no measure", node.id));
        }
    }

    validate_file_measures(snapshot, &kinds, errors);
    validate_clones(snapshot, errors);
}

/// Every file has exactly one file measure whose line classes add up.
fn validate_file_measures(
    snapshot: &Snapshot,
    kinds: &HashMap<&str, crate::schema::NodeKind>,
    errors: &mut Vec<String>,
) {
    use crate::schema::NodeKind;

    let mut measured_files: HashSet<&str> = HashSet::new();
    for file in &snapshot.file_measures {
        if !measured_files.insert(&file.file_id) {
            errors.push(format!("duplicate file measure for {}", file.file_id));
        }
        match kinds.get(file.file_id.as_str()) {
            Some(NodeKind::File) => {}
            Some(kind) => errors.push(format!(
                "file measure for {} names a {kind:?} node, not a file",
                file.file_id
            )),
            None => errors.push(format!(
                "file measure for {} names no node in the snapshot",
                file.file_id
            )),
        }
        if file.blank + file.comment + file.sloc != file.loc {
            errors.push(format!(
                "file measure for {} does not add up: blank {} + comment {} + sloc {} != loc {}",
                file.file_id, file.blank, file.comment, file.sloc, file.loc
            ));
        }
    }
    for node in &snapshot.nodes {
        if node.kind == NodeKind::File && !measured_files.contains(node.id.as_str()) {
            errors.push(format!("file {} has no file measure", node.id));
        }
    }
}

/// The form measures are all-or-nothing per policy: under a policy that
/// carries them every measure has `params`, `nesting`, and `chain`, and
/// `ivars` exactly for a language with instance variables; under any
/// other policy none of the four is present, so a consumer never reads a
/// count the counting rules did not define.
fn validate_form(language: Language, measure: &crate::schema::Measure, errors: &mut Vec<String>) {
    use crate::schema::form_measures_available;

    let expected_ivars = language == Language::Ruby;
    let fields = [
        ("params", measure.params.is_some(), true),
        ("nesting", measure.nesting.is_some(), true),
        ("chain", measure.chain.is_some(), true),
        ("ivars", measure.ivars.is_some(), expected_ivars),
    ];
    for (name, present, belongs) in fields {
        let expected = form_measures_available(language) && belongs;
        if present && !expected {
            errors.push(format!(
                "measure for {} carries `{name}`, which the {} counting policy does not define",
                measure.node_id,
                language.tag()
            ));
        }
        if !present && expected {
            errors.push(format!(
                "measure for {} lacks `{name}`, which the {} counting policy requires",
                measure.node_id,
                language.tag()
            ));
        }
    }
}

/// Twin pairs: present exactly when the identity names a twin policy,
/// which needs a counting policy; the language's own policy name; each
/// side a measured callable, `a` before `b`, no pair twice; shared at
/// most the smaller side; each side at least the shingles the token floor
/// implies; and the containment threshold met, so a pair the producer
/// would not have emitted cannot be asserted.
fn validate_twins(snapshot: &Snapshot, errors: &mut Vec<String>) {
    use crate::schema::{
        NodeKind, TWIN_MIN_CONTAINMENT_PERCENT, TWIN_MIN_TOKENS, TWIN_SHINGLE_TOKENS, twins_policy,
    };

    let pairs = &snapshot.twin_pairs;
    match (&snapshot.identity.twins_policy, pairs.is_empty()) {
        (None, true) => return,
        (None, false) => {
            errors.push("twin pairs present but the identity names no twins_policy".into());
            return;
        }
        (Some(policy), _) => {
            if snapshot.identity.measures_policy.is_none() {
                errors.push(format!(
                    "twins_policy `{policy}` without a measures_policy; twins are found over the measured token stream"
                ));
            }
            let Some(expected) = twins_policy(snapshot.identity.language) else {
                errors.push(format!(
                    "twins_policy `{policy}` but {} has no twin policy",
                    snapshot.identity.language.tag()
                ));
                return;
            };
            if policy != expected {
                errors.push(format!(
                    "twins_policy `{policy}` is not the {} twin policy `{expected}`",
                    snapshot.identity.language.tag()
                ));
            }
        }
    }

    let measured: HashSet<&str> = snapshot
        .measures
        .iter()
        .map(|measure| measure.node_id.as_str())
        .collect();
    let callable: HashSet<&str> = snapshot
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Macro
            )
        })
        .map(|node| node.id.as_str())
        .collect();
    let floor = u32::try_from(TWIN_MIN_TOKENS - TWIN_SHINGLE_TOKENS + 1).unwrap_or(u32::MAX);
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    for pair in pairs {
        for side in [&pair.a, &pair.b] {
            if !callable.contains(side.as_str()) {
                errors.push(format!(
                    "twin pair names {side}, which is not a callable in the snapshot"
                ));
            } else if !measured.contains(side.as_str()) {
                errors.push(format!("twin pair names {side}, which has no measure"));
            }
        }
        if pair.a >= pair.b {
            errors.push(format!(
                "twin pair {} / {} is not ordered by node id",
                pair.a, pair.b
            ));
        }
        if !seen.insert((&pair.a, &pair.b)) {
            errors.push(format!("duplicate twin pair {} / {}", pair.a, pair.b));
        }
        let smaller = pair.shingles_a.min(pair.shingles_b);
        if pair.shared > smaller {
            errors.push(format!(
                "twin pair {} / {} shares {} shingles, more than its smaller side's {smaller}",
                pair.a, pair.b, pair.shared
            ));
        }
        if smaller < floor {
            errors.push(format!(
                "twin pair {} / {} has a side with {smaller} shingles, under the {TWIN_MIN_TOKENS}-token floor",
                pair.a, pair.b
            ));
        }
        if u64::from(pair.shared) * 100
            < u64::from(TWIN_MIN_CONTAINMENT_PERCENT) * u64::from(smaller)
        {
            errors.push(format!(
                "twin pair {} / {} shares {} of {smaller} shingles, under {TWIN_MIN_CONTAINMENT_PERCENT} percent",
                pair.a, pair.b, pair.shared
            ));
        }
    }
}

/// Clone groups: only inside a measured snapshot, two or more occurrences
/// each, occurrences inside inventory files with eligible-line counts
/// bounded by the file's SLOC, unique ids, per-file clone lines bounded by
/// SLOC, and callable memberships that name a group with an occurrence
/// overlapping the callable.
fn validate_clones(snapshot: &Snapshot, errors: &mut Vec<String>) {
    if !crate::schema::clone_measures_available(snapshot.identity.language)
        && (!snapshot.clone_groups.is_empty()
            || snapshot
                .file_measures
                .iter()
                .any(|file| file.clone_sloc != 0)
            || snapshot
                .measures
                .iter()
                .any(|measure| !measure.clone_group_ids.is_empty()))
    {
        errors.push("clone measures are unavailable under this language's counting policy".into());
    }
    let file_sloc: HashMap<&str, u32> = snapshot
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::File)
        .filter_map(|node| {
            snapshot
                .file_measures
                .iter()
                .find(|file| file.file_id == node.id)
                .map(|file| (node.provenance.rel_path.as_str(), file.sloc))
        })
        .collect();
    for file in &snapshot.file_measures {
        if file.clone_sloc > file.sloc {
            errors.push(format!(
                "file measure for {} has clone_sloc {} above sloc {}",
                file.file_id, file.clone_sloc, file.sloc
            ));
        }
    }

    let mut groups: HashMap<&str, &crate::schema::CloneGroup> = HashMap::new();
    for group in &snapshot.clone_groups {
        if groups.insert(&group.id, group).is_some() {
            errors.push(format!("duplicate clone group id {}", group.id));
        }
        if group.occurrences.len() < 2 {
            errors.push(format!(
                "clone group {} has {} occurrence(s); a clone needs two",
                group.id,
                group.occurrences.len()
            ));
        }
        if (group.tokens as usize) < CLONE_MIN_TOKENS {
            errors.push(format!(
                "clone group {} spans {} tokens, under the {CLONE_MIN_TOKENS}-token threshold",
                group.id, group.tokens
            ));
        }
        let shortest = group
            .occurrences
            .iter()
            .map(|occ| occ.end_line.saturating_sub(occ.start_line) + 1)
            .min()
            .unwrap_or(0);
        if group.lines != shortest || shortest < CLONE_MIN_LINES {
            errors.push(format!(
                "clone group {} declares {} lines; its shortest occurrence covers {shortest} and the threshold is {CLONE_MIN_LINES}",
                group.id, group.lines
            ));
        }
        for occ in &group.occurrences {
            if occ.start_line == 0 || occ.end_line < occ.start_line {
                errors.push(format!(
                    "clone group {} has an occurrence with lines {}-{}",
                    group.id, occ.start_line, occ.end_line
                ));
            }
            match file_sloc.get(occ.rel_path.as_str()) {
                Some(sloc) if occ.sloc <= *sloc => {}
                Some(sloc) => errors.push(format!(
                    "clone group {} occurrence in {} counts {} source lines, above the file's sloc {}",
                    group.id, occ.rel_path, occ.sloc, sloc
                )),
                None => errors.push(format!(
                    "clone group {} names {}, which is not a measured file",
                    group.id, occ.rel_path
                )),
            }
            if occ.sloc > occ.end_line - occ.start_line + 1 {
                errors.push(format!(
                    "clone group {} occurrence in {} counts more source lines than it spans",
                    group.id, occ.rel_path
                ));
            }
        }
    }
    if !snapshot.clone_groups.is_empty() && snapshot.measures.is_empty() {
        errors.push("clone groups present but the snapshot carries no measures".into());
    }

    validate_clone_memberships(snapshot, &groups, errors);
}

/// Every group id a measure names exists and has an occurrence overlapping
/// the callable.
fn validate_clone_memberships(
    snapshot: &Snapshot,
    groups: &HashMap<&str, &crate::schema::CloneGroup>,
    errors: &mut Vec<String>,
) {
    let spans: HashMap<&str, (&str, u32, u32)> = snapshot
        .nodes
        .iter()
        .map(|node| {
            (
                node.id.as_str(),
                (
                    node.provenance.rel_path.as_str(),
                    node.provenance.span.start_line,
                    node.provenance.span.end_line,
                ),
            )
        })
        .collect();
    for measure in &snapshot.measures {
        let mut seen: HashSet<&str> = HashSet::new();
        for id in &measure.clone_group_ids {
            if !seen.insert(id) {
                errors.push(format!(
                    "measure for {} names clone group {id} twice",
                    measure.node_id
                ));
            }
            let Some(group) = groups.get(id.as_str()) else {
                errors.push(format!(
                    "measure for {} names clone group {id}, which the snapshot does not carry",
                    measure.node_id
                ));
                continue;
            };
            let overlaps = spans
                .get(measure.node_id.as_str())
                .is_some_and(|(path, start, end)| {
                    group.occurrences.iter().any(|occ| {
                        occ.rel_path == *path && occ.start_line <= *end && *start <= occ.end_line
                    })
                });
            if !overlaps {
                errors.push(format!(
                    "measure for {} names clone group {id}, which has no occurrence overlapping it",
                    measure.node_id
                ));
            }
        }
    }
}

fn validate_report(snapshot: &Snapshot, errors: &mut Vec<String>) {
    if snapshot.report.files_scanned != snapshot.files.len() {
        errors.push(format!(
            "report files_scanned {} does not match {} file records",
            snapshot.report.files_scanned,
            snapshot.files.len()
        ));
    }
    if snapshot.report.files_skipped != 0 {
        errors.push(format!(
            "successful snapshot reports {} skipped files",
            snapshot.report.files_skipped
        ));
    }

    let mut node_counts = BTreeMap::new();
    for node in &snapshot.nodes {
        *node_counts.entry(node.kind.to_string()).or_insert(0) += 1;
    }
    if snapshot.report.node_counts != node_counts {
        errors.push(format!(
            "report node counts do not match nodes: {:?} != {:?}",
            snapshot.report.node_counts, node_counts
        ));
    }

    let mut edge_counts = BTreeMap::new();
    for edge in &snapshot.edges {
        *edge_counts.entry(edge.kind.tag().to_string()).or_insert(0) += 1;
    }
    if snapshot.report.edge_counts != edge_counts {
        errors.push(format!(
            "report edge counts do not match edges: {:?} != {:?}",
            snapshot.report.edge_counts, edge_counts
        ));
    }

    let import_ref_nodes = snapshot
        .nodes
        .iter()
        .filter(|node| node.kind == crate::schema::NodeKind::ImportRef)
        .count();
    if snapshot.report.import_ref_nodes != import_ref_nodes {
        errors.push(format!(
            "report import_ref_nodes {} does not match {import_ref_nodes} import-reference nodes",
            snapshot.report.import_ref_nodes
        ));
    }
}

fn validate_call_sites(
    snapshot: &Snapshot,
    files_by_path: &HashMap<&str, &FileRecord>,
    errors: &mut Vec<String>,
) {
    let semantic = snapshot.identity.is_semantic();
    let mut expected_counts = BTreeMap::new();
    for (index, call_site) in snapshot.call_sites.iter().enumerate() {
        *expected_counts
            .entry(call_site.category.report_key().to_string())
            .or_insert(0) += 1;
        match files_by_path.get(call_site.rel_path.as_str()) {
            Some(file) => {
                if !validate_span(&call_site.span) || call_site.span.end_byte > file.byte_len {
                    errors.push(format!(
                        "invalid or out-of-file span on call site {index} in {}",
                        call_site.rel_path
                    ));
                }
            }
            None => errors.push(format!(
                "call site {index} references unknown path {}",
                call_site.rel_path
            )),
        }
        if (call_site.category == CallSiteCategory::Inactive
            && snapshot.identity.language != Language::Cpp)
            || (call_site.category == CallSiteCategory::Unsupported
                && !matches!(
                    snapshot.identity.language,
                    Language::Cpp | Language::JsTs | Language::Python
                ))
            || (call_site.category == CallSiteCategory::Unprofiled
                && !matches!(snapshot.identity.language, Language::Cpp | Language::JsTs))
        {
            errors.push(format!(
                "C++ call classification in a non-C++ snapshot at site {index}"
            ));
        }
        match call_site.category {
            CallSiteCategory::Failed => errors.push(format!(
                "call site {index} records an infrastructure failure in a successful snapshot"
            )),
            CallSiteCategory::Observed if semantic => errors.push(format!(
                "call site {index} is unclassified in a semantic snapshot"
            )),
            CallSiteCategory::Resolved
            | CallSiteCategory::ExternalCrate
            | CallSiteCategory::StandardLibrary
            | CallSiteCategory::Ambiguous
            | CallSiteCategory::Candidate
            | CallSiteCategory::Unsupported
            | CallSiteCategory::Inactive
            | CallSiteCategory::Unprofiled
                if !semantic =>
            {
                errors.push(format!(
                    "call site {index} claims semantic category {:?} in a syntax-only snapshot",
                    call_site.category
                ));
            }
            CallSiteCategory::Observed
            | CallSiteCategory::Unmapped
            | CallSiteCategory::Resolved
            | CallSiteCategory::ExternalCrate
            | CallSiteCategory::StandardLibrary
            | CallSiteCategory::Ambiguous
            | CallSiteCategory::Candidate
            | CallSiteCategory::Unsupported
            | CallSiteCategory::Inactive
            | CallSiteCategory::Unprofiled => {}
        }
    }
    if snapshot.report.call_sites_by_category != expected_counts {
        errors.push(format!(
            "call-site report counts do not match observations: {:?} != {:?}",
            snapshot.report.call_sites_by_category, expected_counts
        ));
    }
}

#[allow(clippy::too_many_lines)] // per-language identity gates read as one table
fn validate_tool_identity(snapshot: &Snapshot, errors: &mut Vec<String>) {
    let tools = &snapshot.identity.tools;
    // Grammar fields must match the snapshot language, and a snapshot may
    // not carry the other language's grammar version.
    match snapshot.identity.language {
        Language::Rust => {
            if tools.tree_sitter_rust.is_empty() {
                errors.push("rust snapshot identity is missing tree_sitter_rust version".into());
            }
            if !tools.ruby_parser.is_empty() {
                errors.push("rust snapshot carries a ruby_parser identity".into());
            }
        }
        Language::Ruby => {
            if tools.ruby_parser.is_empty() {
                errors.push("ruby snapshot identity is missing the ruby_parser version".into());
            }
            if !tools.tree_sitter_rust.is_empty() {
                errors.push("ruby snapshot carries a tree_sitter_rust version".into());
            }
            if !tools.tree_sitter.is_empty() {
                errors.push(
                    "ruby snapshot carries a tree_sitter version (Ruby is Prism-parsed)".into(),
                );
            }
            if !tools.tree_sitter_php.is_empty() {
                errors.push("ruby snapshot carries a tree_sitter_php version".into());
            }
        }
        Language::Php => {
            if tools.tree_sitter.is_empty() {
                errors.push("php snapshot identity is missing the tree_sitter version".into());
            }
            if tools.tree_sitter_php.is_empty() {
                errors.push("php snapshot identity is missing the tree_sitter_php version".into());
            }
            if !tools.tree_sitter_rust.is_empty() {
                errors.push("php snapshot carries a tree_sitter_rust version".into());
            }
            if !tools.ruby_parser.is_empty() {
                errors.push("php snapshot carries a ruby_parser identity".into());
            }
        }
        Language::Go => {
            if tools.tree_sitter.is_empty() {
                errors.push("go snapshot identity is missing the tree_sitter version".into());
            }
            if tools.tree_sitter_go.is_empty() {
                errors.push("go snapshot identity is missing the tree_sitter_go version".into());
            }
            if !tools.tree_sitter_rust.is_empty() {
                errors.push("go snapshot carries a tree_sitter_rust version".into());
            }
            if !tools.tree_sitter_php.is_empty() {
                errors.push("go snapshot carries a tree_sitter_php version".into());
            }
            if !tools.ruby_parser.is_empty() {
                errors.push("go snapshot carries a ruby_parser identity".into());
            }
        }
        Language::JsTs => {
            if tools.tree_sitter.is_empty()
                || tools.tree_sitter_javascript.is_empty()
                || tools.tree_sitter_typescript.is_empty()
            {
                errors.push("js_ts snapshot is missing parser versions".into());
            }
            if !tools.tree_sitter_rust.is_empty() || !tools.ruby_parser.is_empty() {
                errors.push("js_ts snapshot carries another language parser identity".into());
            }
        }
        Language::Cpp => {
            if tools.tree_sitter.is_empty() {
                errors.push("cpp snapshot identity is missing the tree_sitter version".into());
            }
            if !tools.tree_sitter_rust.is_empty() {
                errors.push("cpp snapshot carries a tree_sitter_rust version".into());
            }
            if !tools.tree_sitter_php.is_empty() {
                errors.push("cpp snapshot carries a tree_sitter_php version".into());
            }
            if !tools.tree_sitter_go.is_empty() {
                errors.push("cpp snapshot carries a tree_sitter_go version".into());
            }
            if !tools.ruby_parser.is_empty() {
                errors.push("cpp snapshot carries a ruby_parser identity".into());
            }
            if tools.tree_sitter_cpp.is_empty() {
                errors.push("cpp snapshot identity is missing the tree_sitter_cpp version".into());
            }
        }
        Language::Python => {
            if tools.python.as_deref() != Some("3.14.7")
                || !tools.tree_sitter.is_empty()
                || !tools.ruby_parser.is_empty()
            {
                errors.push("Python snapshot requires its exact CPython parser identity".into());
            }
            let inputs = snapshot.identity.python.as_ref();
            if !inputs.is_some_and(crate::schema::PythonInputs::is_valid) {
                errors.push("Python snapshot requires complete parser input identity".into());
            }
            let native = inputs.is_some_and(|inputs| inputs.native.is_some());
            if native != tools.mypyc.is_some()
                || tools
                    .mypyc
                    .as_deref()
                    .is_some_and(|version| version != "2.3.1")
            {
                errors.push("Python native tool and input identities disagree".into());
            }
        }
    }
    if snapshot.identity.language != Language::Python
        && (snapshot.identity.python.is_some() || tools.python.is_some() || tools.mypyc.is_some())
    {
        errors.push("Python identity in another language snapshot".into());
    }
    if snapshot.identity.language != Language::JsTs
        && (!tools.tree_sitter_javascript.is_empty() || !tools.tree_sitter_typescript.is_empty())
    {
        errors.push("JS/TS parser identity in another language snapshot".into());
    }
    if snapshot.identity.language != Language::Cpp && !tools.tree_sitter_cpp.is_empty() {
        errors.push(format!(
            "tree_sitter_cpp identity in a {} snapshot",
            snapshot.identity.language.tag()
        ));
    }
    if snapshot.identity.language != Language::Go && !tools.tree_sitter_go.is_empty() {
        errors.push(format!(
            "tree_sitter_go identity in a {} snapshot",
            snapshot.identity.language.tag()
        ));
    }
    if snapshot.identity.language != Language::Go
        && (tools.gopls.is_some() || tools.go.is_some() || tools.go_tool_bundle_sha256.is_some())
    {
        errors.push(format!(
            "gopls semantic identity in a {} snapshot",
            snapshot.identity.language.tag()
        ));
    }
    if snapshot.identity.language != Language::Rust && tools.rust_analyzer.is_some() {
        errors.push(format!(
            "rust-analyzer semantic identity in a {} snapshot",
            snapshot.identity.language.tag()
        ));
    }
    if snapshot.identity.language != Language::Ruby
        && (tools.ruby_lsp.is_some()
            || tools.ruby.is_some()
            || tools.rbs.is_some()
            || tools.ruby_tool_bundle_sha256.is_some())
    {
        errors.push(format!(
            "ruby-lsp semantic identity in a {} snapshot",
            snapshot.identity.language.tag()
        ));
    }
    if snapshot.identity.language != Language::Php && !tools.tree_sitter_php.is_empty() {
        errors.push(format!(
            "tree_sitter_php identity in a {} snapshot",
            snapshot.identity.language.tag()
        ));
    }
    if snapshot.identity.language != Language::Php
        && (tools.phpactor.is_some()
            || tools.php.is_some()
            || tools.php_tool_bundle_sha256.is_some())
    {
        errors.push(format!(
            "phpactor semantic identity in a {} snapshot",
            snapshot.identity.language.tag()
        ));
    }
    if tools.rust_analyzer.is_some() {
        if tools.rustc.as_deref().is_none_or(str::is_empty) {
            errors.push("semantic identity is missing companion rustc version".into());
        }
        if tools.cargo.as_deref().is_none_or(str::is_empty) {
            errors.push("semantic identity is missing companion cargo version".into());
        }
        if tools.target_triple.as_deref().is_none_or(str::is_empty) {
            errors.push("semantic identity is missing target triple".into());
        }
        if tools.cargo_feature_policy.as_deref() != Some(CARGO_FEATURE_POLICY) {
            errors.push(format!(
                "semantic identity must use cargo feature policy {CARGO_FEATURE_POLICY}"
            ));
        }
        if tools.rust_analyzer_policy.as_ref() != Some(&RustAnalyzerPolicy::fixed()) {
            errors.push("semantic identity has non-canonical rust-analyzer policy".into());
        }
    }
    if tools.ruby_lsp.is_some() {
        if tools.ruby.as_deref().is_none_or(str::is_empty) {
            errors.push("ruby semantic identity is missing companion ruby version".into());
        }
        if tools.rbs.as_deref().is_none_or(str::is_empty) {
            errors.push("ruby semantic identity is missing companion rbs version".into());
        }
        if tools
            .ruby_tool_bundle_sha256
            .as_deref()
            .is_none_or(str::is_empty)
        {
            errors.push("ruby semantic identity is missing the tool bundle digest".into());
        }
    }
    if tools.gopls.is_some() {
        if tools.go.as_deref().is_none_or(str::is_empty) {
            errors.push("go semantic identity is missing companion go version".into());
        }
        if tools
            .go_tool_bundle_sha256
            .as_deref()
            .is_none_or(str::is_empty)
        {
            errors.push("go semantic identity is missing the tool bundle digest".into());
        }
    }
    if let Some(inputs) = &snapshot.identity.cpp_semantic {
        if snapshot.identity.language != Language::Cpp {
            errors.push("C++ semantic input identity in a non-C++ snapshot".into());
        }
        if inputs.policy != crate::schema::CPP_SEMANTIC_POLICY {
            errors.push("C++ semantic identity has a non-canonical policy".into());
        }
        for (name, digest) in [
            ("profile", &inputs.profile_sha256),
            ("inputs", &inputs.inputs_sha256),
            ("runtime", &inputs.runtime_sha256),
            ("clangd", &inputs.clangd_sha256),
        ] {
            if !crate::schema::valid_sha256(digest) {
                errors.push(format!(
                    "C++ semantic identity has an invalid {name} SHA-256"
                ));
            }
        }
    }
    if let Some(inputs) = &snapshot.identity.js_ts_semantic {
        if snapshot.identity.language != Language::JsTs {
            errors.push("JS/TS semantic input identity in a non-JS/TS snapshot".into());
        }
        if !inputs.is_valid() {
            errors.push(
                "JS/TS semantic identity has invalid project, versions, digests or policy".into(),
            );
        }
    }
    let rust_semantic_only = tools.rustc.is_some()
        || tools.cargo.is_some()
        || tools.target_triple.is_some()
        || tools.cargo_feature_policy.is_some()
        || tools.cargo_lock_sha256.is_some()
        || tools.rust_analyzer_policy.is_some();
    if tools.rust_analyzer.is_none() && rust_semantic_only {
        errors.push("syntax-only identity carries incomplete semantic-only fields".into());
    }
}

fn validate_provenance(
    kind: &str,
    id: &str,
    provenance: &Provenance,
    snapshot: &Snapshot,
    files_by_path: &HashMap<&str, &FileRecord>,
    errors: &mut Vec<String>,
) {
    if provenance.repository != snapshot.identity.repository.id {
        errors.push(format!("{kind} {id} repository provenance mismatch"));
    }
    if provenance.commit_sha != snapshot.identity.commit_sha {
        errors.push(format!("{kind} {id} commit provenance mismatch"));
    }
    if provenance.extractor_version != snapshot.identity.extractor_version {
        errors.push(format!("{kind} {id} extractor provenance mismatch"));
    }
    match files_by_path.get(provenance.rel_path.as_str()) {
        Some(file) => {
            if provenance.file_hash != file.sha256 {
                errors.push(format!(
                    "{kind} {id} file hash does not match path {}",
                    provenance.rel_path
                ));
            }
            if !validate_span(&provenance.span) || provenance.span.end_byte > file.byte_len {
                errors.push(format!(
                    "invalid or out-of-file span on {kind} {id} for {}",
                    provenance.rel_path
                ));
            }
        }
        None => errors.push(format!(
            "{kind} {id} references unknown path {}",
            provenance.rel_path
        )),
    }
}

const fn validate_span(s: &crate::schema::Span) -> bool {
    s.start_byte <= s.end_byte && s.start_line <= s.end_line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::*;

    fn identity_for(language: Language) -> SnapshotIdentity {
        let tools = match language {
            Language::Rust => ToolVersions::syntax(),
            Language::Ruby => ToolVersions::syntax_ruby(),
            Language::Php => ToolVersions::syntax_php(),
            Language::Go => ToolVersions::syntax_go(),
            Language::Cpp => ToolVersions::syntax_cpp(),
            Language::JsTs => ToolVersions::syntax_js_ts(),
            Language::Python => ToolVersions::syntax_python(),
        };
        SnapshotIdentity::build(
            language,
            RepoIdentity {
                id: "r".into(),
                origin: None,
                is_canonical: false,
            },
            "c".into(),
            "t".into(),
            tools,
            false,
        )
    }

    fn good_identity() -> SnapshotIdentity {
        identity_for(Language::Rust)
    }

    fn file_node(id: &str, rel_path: &str) -> Node {
        let fact_source = if id.starts_with("ruby:") {
            FactSource::Prism
        } else {
            FactSource::TreeSitter
        };
        Node {
            python: None,
            id: id.into(),
            kind: NodeKind::File,
            name: rel_path.into(),
            symbol_key: id.into(),
            provenance: Provenance {
                repository: "r".into(),
                commit_sha: "c".into(),
                rel_path: rel_path.into(),
                span: Span::from_ts(0, 1, (0, 0), (0, 1)),
                file_hash: "h".into(),
                extractor_version: EXTRACTOR_VERSION.to_string(),
                fact_source,
                confidence: u8::MAX,
            },
        }
    }

    fn snapshot_with(identity: SnapshotIdentity, node: Node, rel_path: &str) -> Snapshot {
        let mut node_counts = std::collections::BTreeMap::new();
        node_counts.insert("file".to_string(), 1);
        Snapshot {
            identity,
            files: vec![FileRecord {
                rel_path: rel_path.into(),
                sha256: "h".into(),
                byte_len: 1,
            }],
            nodes: vec![node],
            edges: vec![],
            call_sites: vec![],
            measures: Vec::new(),
            file_measures: Vec::new(),
            clone_groups: Vec::new(),
            twin_pairs: Vec::new(),
            report: Report {
                files_scanned: 1,
                files_skipped: 0,
                parse_errors: 0,
                node_counts,
                edge_counts: std::collections::BTreeMap::new(),
                call_sites_by_category: std::collections::BTreeMap::new(),
                import_ref_nodes: 0,
                observations: std::collections::BTreeMap::new(),
            },
        }
    }

    #[test]
    fn rust_analyzer_implements_requires_identity_and_concrete_endpoint_kinds() {
        let mut identity = good_identity();
        identity.tools = ToolVersions::semantic(
            "ra".into(),
            "rustc".into(),
            "cargo".into(),
            "aarch64-apple-darwin".into(),
            None,
        );
        identity.snapshot_id = identity.compute_snapshot_id();
        let mut snapshot = snapshot_with(identity, file_node("rust:file:a.rs", "a.rs"), "a.rs");
        for (id, kind) in [
            ("rust:type:a.rs#Item", NodeKind::Type(TypeKind::Struct)),
            ("rust:trait:a.rs#Marker", NodeKind::Trait),
        ] {
            let mut node = file_node(id, "a.rs");
            node.kind = kind;
            snapshot.nodes.push(node);
        }
        let mut provenance = snapshot.nodes[0].provenance.clone();
        provenance.fact_source = FactSource::RustAnalyzer;
        snapshot.edges.push(Edge {
            id: "implementation".into(),
            kind: EdgeKind::Implements,
            from: snapshot.nodes[1].id.clone(),
            to: snapshot.nodes[2].id.clone(),
            provenance,
        });
        let refresh = |snapshot: &mut Snapshot| {
            snapshot.identity.snapshot_id = snapshot.identity.compute_snapshot_id();
            snapshot.report.node_counts.clear();
            for node in &snapshot.nodes {
                *snapshot
                    .report
                    .node_counts
                    .entry(node.kind.to_string())
                    .or_default() += 1;
            }
            snapshot.report.edge_counts =
                BTreeMap::from([(snapshot.edges[0].kind.tag().to_string(), 1)]);
        };
        refresh(&mut snapshot);
        validate(&snapshot).unwrap();
        for change in 0..8 {
            let mut invalid = snapshot.clone();
            match change {
                0 => invalid.identity.tools.rust_analyzer = None,
                1 => invalid.nodes[1].kind = NodeKind::Type(TypeKind::TypeAlias),
                2 => invalid.nodes[2].kind = NodeKind::Type(TypeKind::Struct),
                3 => invalid.edges[0].kind = EdgeKind::Inherits,
                4 => invalid.edges[0].provenance.fact_source = FactSource::RubyLsp,
                5 => invalid.edges[0].kind = EdgeKind::Contains,
                6 => invalid.edges[0].kind = EdgeKind::Imports,
                _ => invalid.edges[0].provenance.confidence = 1,
            }
            refresh(&mut invalid);
            assert!(
                validate(&invalid).is_err(),
                "invalid semantic implementation case {change}"
            );
        }
    }

    #[test]
    fn foreign_language_node_id_fails_purity_in_both_directions() {
        // ruby fact inside a rust snapshot
        let rust_snap = snapshot_with(
            identity_for(Language::Rust),
            file_node("ruby:file:app.rb", "app.rb"),
            "app.rb",
        );
        let err = validate(&rust_snap).unwrap_err().to_string();
        assert!(err.contains("language purity"), "{err}");

        // rust fact inside a ruby snapshot
        let ruby_snap = snapshot_with(
            identity_for(Language::Ruby),
            file_node("rust:file:src/lib.rs", "src/lib.rs"),
            "src/lib.rs",
        );
        let err = validate(&ruby_snap).unwrap_err().to_string();
        assert!(err.contains("language purity"), "{err}");

        // php fact inside a rust snapshot, and rust fact inside a php one
        let rust_snap = snapshot_with(
            identity_for(Language::Rust),
            file_node("php:file:index.php", "index.php"),
            "index.php",
        );
        let err = validate(&rust_snap).unwrap_err().to_string();
        assert!(err.contains("language purity"), "{err}");
        let php_snap = snapshot_with(
            identity_for(Language::Php),
            file_node("rust:file:src/lib.rs", "src/lib.rs"),
            "src/lib.rs",
        );
        let err = validate(&php_snap).unwrap_err().to_string();
        assert!(err.contains("language purity"), "{err}");

        // go fact inside a php snapshot, and php fact inside a go one
        let php_snap = snapshot_with(
            identity_for(Language::Php),
            file_node("go:file:main.go", "main.go"),
            "main.go",
        );
        let err = validate(&php_snap).unwrap_err().to_string();
        assert!(err.contains("language purity"), "{err}");
        let go_snap = snapshot_with(
            identity_for(Language::Go),
            file_node("php:file:index.php", "index.php"),
            "index.php",
        );
        let err = validate(&go_snap).unwrap_err().to_string();
        assert!(err.contains("language purity"), "{err}");
    }

    #[test]
    fn matching_language_node_id_passes_purity() {
        let ruby_snap = snapshot_with(
            identity_for(Language::Ruby),
            file_node("ruby:file:app.rb", "app.rb"),
            "app.rb",
        );
        validate(&ruby_snap).unwrap();
    }

    #[test]
    fn ruby_snapshot_rejects_rust_semantic_tools() {
        let mut identity = identity_for(Language::Ruby);
        identity.tools = ToolVersions::semantic(
            "ra".into(),
            "rustc".into(),
            "cargo".into(),
            "aarch64-apple-darwin".into(),
            None,
        );
        identity.snapshot_id = identity.compute_snapshot_id();
        let snap = snapshot_with(identity, file_node("ruby:file:app.rb", "app.rb"), "app.rb");
        let err = validate(&snap).unwrap_err().to_string();
        assert!(err.contains("rust-analyzer semantic identity"), "{err}");
    }

    #[test]
    fn valid_empty_snapshot_passes() {
        let id = good_identity();
        let snap = Snapshot {
            identity: id,
            files: vec![],
            nodes: vec![],
            edges: vec![],
            call_sites: vec![],
            measures: Vec::new(),
            file_measures: Vec::new(),
            clone_groups: Vec::new(),
            twin_pairs: Vec::new(),
            report: Report::default(),
        };
        validate(&snap).unwrap();
    }

    #[test]
    fn duplicate_node_id_fails() {
        let id = good_identity();
        let prov = Provenance {
            repository: "r".into(),
            commit_sha: "c".into(),
            rel_path: "a.rs".into(),
            span: Span::from_ts(0, 1, (0, 0), (0, 1)),
            file_hash: "h".into(),
            extractor_version: "0.1.0".to_string(),
            fact_source: FactSource::TreeSitter,
            confidence: 1,
        };
        let n = Node {
            python: None,
            id: "rust:file:a.rs".into(),
            kind: NodeKind::File,
            name: "a.rs".into(),
            symbol_key: "rust:file:a.rs".into(),
            provenance: prov,
        };
        let snap = Snapshot {
            identity: id,
            files: vec![FileRecord {
                rel_path: "a.rs".into(),
                sha256: "h".into(),
                byte_len: 1,
            }],
            nodes: vec![n.clone(), n],
            edges: vec![],
            call_sites: vec![],
            measures: Vec::new(),
            file_measures: Vec::new(),
            clone_groups: Vec::new(),
            twin_pairs: Vec::new(),
            report: Report::default(),
        };
        assert!(validate(&snap).is_err());
    }
}
