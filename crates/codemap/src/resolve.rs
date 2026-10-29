//! Cross-file resolution: definition index, `implements` resolution, and
//! call-site omission accounting.
//!
//! Per the AGENTS.md truth boundary, an `implements` edge is emitted only
//! when **both** the type and the trait resolve to a unique in-repository
//! definition. External-trait impls (`impl Display for Foo`) are reported
//! as observations, never upgraded to canonical edges — this is the
//! conservative mirror of the `calls` policy. `calls` edges are produced
//! only by semantic resolution ([`crate::resolve`]'s semantic path, a
//! future phase); in syntax-only mode every call site is an observation.
//!
//! The index is a multi-map keyed by declared name. Names collide across
//! files; the resolver reports `Ambiguous` and drops rather than guessing.

use std::collections::HashMap;

use crate::schema::{Edge, EdgeKind, FactSource, Node, NodeKind, Provenance, TypeKind, edge_id};

/// One definition's identity in the cross-file index.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct DefinitionRef {
    pub node_id: String,
    pub kind: NodeKind,
    pub scope: Vec<String>,
    pub rel_path: String,
    pub span: crate::schema::Span,
    /// For Rust lexical proofs, excludes declarations inside callable bodies.
    pub module_item: bool,
}

/// Outcome of resolving a name against the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Exactly one in-repository definition matched.
    Unique(DefinitionRef),
    /// Multiple matched; cannot be canonical.
    Ambiguous(Vec<DefinitionRef>),
    /// No in-repository definition matched.
    Unknown,
}

/// Cross-file index of every extracted definition, keyed by declared name
/// and by fully qualified name. The qualified key is the `symbol_key` tail
/// (`ruby:path#Admin::User` -> `Admin::User`), which is exactly the name
/// Ruby's constant lookup resolves against.
#[derive(Debug, Default)]
pub struct DefinitionIndex {
    by_name: HashMap<String, Vec<DefinitionRef>>,
    by_qualified: HashMap<String, Vec<DefinitionRef>>,
}

impl DefinitionIndex {
    /// Build an index from emitted nodes. Only definition-bearing kinds
    /// are indexed (not `File` or `ImportRef`).
    pub fn from_nodes(nodes: &[Node]) -> Self {
        let mut callable_spans: HashMap<&str, Vec<crate::schema::Span>> = HashMap::new();
        for node in nodes.iter().filter(|node| {
            node.id.starts_with("rust:")
                && matches!(node.kind, NodeKind::Function | NodeKind::Method)
        }) {
            callable_spans
                .entry(&node.provenance.rel_path)
                .or_default()
                .push(node.provenance.span);
        }
        let mut by_name: HashMap<String, Vec<DefinitionRef>> = HashMap::new();
        let mut by_qualified: HashMap<String, Vec<DefinitionRef>> = HashMap::new();
        for n in nodes {
            if matches!(n.kind, NodeKind::File | NodeKind::ImportRef) {
                continue;
            }
            let def = DefinitionRef {
                node_id: n.id.clone(),
                kind: n.kind,
                // Semantic scope reconstructed from the symbol_key tail.
                // Not used for v1 name resolution; retained for a future
                // receiver-aware disambiguation pass.
                scope: scope_from_symbol_key(&n.symbol_key),
                rel_path: n.provenance.rel_path.clone(),
                span: n.provenance.span,
                module_item: !callable_spans
                    .get(n.provenance.rel_path.as_str())
                    .is_some_and(|spans| {
                        spans.iter().any(|span| {
                            span.start_byte <= n.provenance.span.start_byte
                                && n.provenance.span.end_byte <= span.end_byte
                        })
                    }),
            };
            by_name.entry(n.name.clone()).or_default().push(def.clone());
            if let Some((_, qualified)) = n.symbol_key.split_once('#') {
                by_qualified
                    .entry(qualified.to_string())
                    .or_default()
                    .push(def);
            }
        }
        Self {
            by_name,
            by_qualified,
        }
    }

    /// Ruby-style lexical constant resolution, mirroring how Ruby (and
    /// Zeitwerk's file->module mapping) resolve a written constant from a
    /// call site: walk fully qualified lexical owners innermost-first, prefixing
    /// the written receiver (`User` inside `module Admin` -> `Admin::User`,
    /// then top-level `User`), and stop at the first level that matches at
    /// least one in-repository definition.
    ///
    /// A nested constant shadows outer ones in Ruby, so a matching inner
    /// level that is ambiguous (two in-repo files define it) is reported as
    /// `Ambiguous` and never walked outward: the truth boundary forbids
    /// guessing which shadow the writer meant.
    pub fn resolve_constant(&self, receiver: &str, scope: &[String]) -> Resolution {
        let (receiver, scope) = receiver
            .strip_prefix("::")
            .map_or((receiver, scope), |absolute| (absolute, &[][..]));
        // Try each enclosing-scope prefix innermost-first, then the
        // written constant at top level (Ruby's constant lookup order).
        for prefix in scope
            .iter()
            .rev()
            .map(String::as_str)
            .chain(std::iter::once(""))
        {
            let candidate = if prefix.is_empty() {
                receiver.to_string()
            } else {
                format!("{prefix}::{receiver}")
            };
            match self.by_qualified.get(&candidate) {
                Some(refs) if refs.len() == 1 => return Resolution::Unique(refs[0].clone()),
                Some(refs) if !refs.is_empty() => return Resolution::Ambiguous(refs.clone()),
                _ => {} // no match at this level: walk outward
            }
        }
        Resolution::Unknown
    }

    fn ruby_declaration(&self, qualified: &str, prov: &Provenance) -> Option<DefinitionRef> {
        let mut declarations = self.by_qualified.get(qualified)?.iter().filter(|def| {
            def.rel_path == prov.rel_path
                && def.span.start_byte <= prov.span.start_byte
                && prov.span.end_byte <= def.span.end_byte
        });
        let declaration = declarations.next()?;
        declarations.next().is_none().then(|| declaration.clone())
    }

    /// Look up a PHP qualified name (`App\Model::User`) against the
    /// qualified-symbol-key index. PHP heritage declarants are recorded
    /// qualified, which the leaf-name index cannot find.
    pub fn resolve_qualified_with_sep(&self, qualified: &str) -> Resolution {
        // Both separator styles exist in the key space: `\` for PHP
        // namespaces, `::` for type nesting. Try as written first.
        match self.by_qualified.get(qualified) {
            Some(refs) if refs.len() == 1 => return Resolution::Unique(refs[0].clone()),
            Some(refs) if !refs.is_empty() => return Resolution::Ambiguous(refs.clone()),
            _ => {}
        }
        // `::` segments may include the namespace: `App\Model::User` as
        // written is the exact key form (`node_id` joins the namespace
        // scope entry with `::`). No fallback needed beyond normalization.
        Resolution::Unknown
    }

    /// Look up a fully qualified PHP class reference (`App\Model\User`, as
    /// [`crate::php_syntax`] qualifies every written name) against the
    /// symbol-key form, where the namespace and the type name are joined
    /// by `::` (`App\Model::User`).
    pub fn resolve_php_class(&self, fully_qualified: &str) -> Resolution {
        let key = fully_qualified.rsplit_once('\\').map_or_else(
            || fully_qualified.to_string(),
            |(namespace, leaf)| format!("{namespace}::{leaf}"),
        );
        self.resolve_qualified_with_sep(&key)
    }

    /// Look up a Go type named `name` declared in the package directory
    /// `dir`. A Go package is its directory and forbids two top-level
    /// declarations of one name, so the directory is the whole scope: a
    /// same-named type in another directory is a different package.
    pub fn resolve_go_type(&self, name: &str, dir: &str) -> Resolution {
        let refs: Vec<DefinitionRef> = self
            .by_name
            .get(name)
            .map(|refs| {
                refs.iter()
                    .filter(|def| {
                        matches!(def.kind, NodeKind::Type(_)) && parent_dir(&def.rel_path) == dir
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        match refs.len() {
            0 => Resolution::Unknown,
            1 => Resolution::Unique(refs[0].clone()),
            _ => Resolution::Ambiguous(refs),
        }
    }

    /// Total indexed definitions.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.by_name.values().map(std::vec::Vec::len).sum()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.by_name.values().all(std::vec::Vec::is_empty)
    }
}

/// Extract the scope chain from a `symbol_key` of the form
/// `rust:{path}#{Scope::Name}`.
fn scope_from_symbol_key(key: &str) -> Vec<String> {
    let Some(tail) = key.split_once('#').map(|(_, t)| t) else {
        return Vec::new();
    };
    tail.split("::")
        .map(std::string::ToString::to_string)
        .collect()
}

/// Attempt to build a mixin edge from a raw Ruby mixin observation.
///
/// Returns `Some(edge)` only when the receiving class/module is identified
/// by its source declaration and the mixin target resolves to a unique
/// in-repository definition. The edge kind
/// carries the mixin flavor: `include`/`prepend` become `Implements`
/// (instance-method ancestry); `extend` becomes `Extends` (singleton-method
/// ancestry), so the semantic edge policy can tell the regimes apart.
pub fn resolve_mixin(
    mixin: &crate::ruby_syntax::RawMixin,
    index: &DefinitionIndex,
    prov: &Provenance,
) -> Option<Edge> {
    let receiver_name = mixin.receiver_name.as_deref()?;
    let from = index.ruby_declaration(receiver_name, prov)?;
    let Resolution::Unique(to) = index.resolve_constant(&mixin.target, &mixin.scope) else {
        return None;
    };
    if !matches!(
        from.kind,
        NodeKind::Type(TypeKind::Class) | NodeKind::Module
    ) || to.kind != NodeKind::Module
    {
        return None;
    }
    let kind = match mixin.mixin {
        crate::ruby_syntax::MixinKind::Include | crate::ruby_syntax::MixinKind::Prepend => {
            EdgeKind::Implements
        }
        crate::ruby_syntax::MixinKind::Extend => EdgeKind::Extends,
    };
    let edge = Edge {
        id: edge_id(kind, &from.node_id, &to.node_id, prov.span.start_byte),
        kind,
        from: from.node_id,
        to: to.node_id,
        provenance: prov.clone(),
    };
    debug_assert!(matches!(
        prov.fact_source,
        FactSource::TreeSitter | FactSource::Prism
    ));
    Some(edge)
}

/// Attempt to build an `inherits` edge from a raw Ruby superclass
/// observation. Same source-binding and target uniqueness discipline as
/// [`resolve_mixin`].
pub fn resolve_inherits(
    inherit: &crate::ruby_syntax::RawInherit,
    index: &DefinitionIndex,
    prov: &Provenance,
) -> Option<Edge> {
    let from = index.ruby_declaration(&inherit.class_name, prov)?;
    let Resolution::Unique(to) = index.resolve_constant(&inherit.superclass, &inherit.scope) else {
        return None;
    };
    if from.kind != NodeKind::Type(TypeKind::Class) || to.kind != NodeKind::Type(TypeKind::Class) {
        return None;
    }
    let edge = Edge {
        id: edge_id(
            EdgeKind::Inherits,
            &from.node_id,
            &to.node_id,
            prov.span.start_byte,
        ),
        kind: EdgeKind::Inherits,
        from: from.node_id,
        to: to.node_id,
        provenance: prov.clone(),
    };
    debug_assert!(matches!(
        prov.fact_source,
        FactSource::TreeSitter | FactSource::Prism
    ));
    Some(edge)
}

/// Attempt to build an `implements` edge from a raw impl observation.
///
/// Syntax proves only unique concrete-type and trait declarations in the
/// same file and lexical module, without imports or generic/local bindings.
/// Other relations require exact compiler declarations or stay observations.
pub fn resolve_implements(
    impl_obs: &crate::rust_syntax::RawImpl,
    index: &DefinitionIndex,
    prov: &Provenance,
) -> Option<Edge> {
    let trait_name = impl_obs.trait_name.as_deref()?;
    if impl_obs.local_scope
        || impl_obs.negative
        || impl_obs.generic
        || impl_obs.visible_glob
        || impl_obs.imported_binding
        || impl_obs.type_name.contains("::")
        || trait_name.contains("::")
        || impl_obs.type_name_start.is_none()
        || impl_obs.trait_name_start.is_none()
    {
        return None;
    }
    let local = |name: &str, is_trait: bool| {
        let qualified = impl_obs
            .scope
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(name))
            .collect::<Vec<_>>()
            .join("::");
        let declarations: Vec<_> = index
            .by_qualified
            .get(&qualified)?
            .iter()
            .filter(|definition| {
                definition.rel_path == prov.rel_path
                    && definition.module_item
                    && if is_trait {
                        definition.kind == NodeKind::Trait
                    } else {
                        matches!(
                            definition.kind,
                            NodeKind::Type(TypeKind::Struct | TypeKind::Enum | TypeKind::Union)
                        )
                    }
            })
            .collect();
        (declarations.len() == 1).then(|| declarations[0].node_id.clone())
    };
    let from_id = local(&impl_obs.type_name, false)?;
    let to_id = local(trait_name, true)?;
    let edge = Edge {
        id: edge_id(EdgeKind::Implements, &from_id, &to_id, prov.span.start_byte),
        kind: EdgeKind::Implements,
        from: from_id,
        to: to_id,
        provenance: prov.clone(),
    };
    debug_assert!(matches!(
        prov.fact_source,
        FactSource::TreeSitter | FactSource::Prism
    ));
    Some(edge)
}

/// Attempt to build an edge from a raw PHP `extends` / `implements`
/// clause observation. Same uniqueness discipline as [`resolve_inherits`]:
/// both the declaring type and the target, already qualified by the
/// syntax layer from the file's imports and namespace, must resolve to
/// unique in-repository definitions, else `None`.
pub fn resolve_php_heritage(
    heritage: &crate::php_syntax::RawHeritage,
    index: &DefinitionIndex,
    prov: &Provenance,
) -> Option<Edge> {
    let Resolution::Unique(from) = index.resolve_qualified_with_sep(&heritage.declarant) else {
        return None;
    };
    let Resolution::Unique(to) = index.resolve_php_class(&heritage.target) else {
        return None;
    };
    let edge = Edge {
        id: edge_id(
            heritage.kind,
            &from.node_id,
            &to.node_id,
            prov.span.start_byte,
        ),
        kind: heritage.kind,
        from: from.node_id,
        to: to.node_id,
        provenance: prov.clone(),
    };
    debug_assert!(matches!(
        prov.fact_source,
        FactSource::TreeSitter | FactSource::Prism
    ));
    Some(edge)
}

/// Attempt to build an `implements` edge from a raw PHP trait-`use`
/// observation (trait composition inside a class body). Same uniqueness
/// discipline as the other syntax-candidate resolvers.
pub fn resolve_php_trait_use(
    trait_use: &crate::php_syntax::RawTraitUse,
    index: &DefinitionIndex,
    prov: &Provenance,
) -> Option<Edge> {
    let Resolution::Unique(from) = index.resolve_qualified_with_sep(&trait_use.class_name) else {
        return None;
    };
    let Resolution::Unique(to) = index.resolve_php_class(&trait_use.trait_name) else {
        return None;
    };
    let edge = Edge {
        id: edge_id(
            EdgeKind::Implements,
            &from.node_id,
            &to.node_id,
            prov.span.start_byte,
        ),
        kind: EdgeKind::Implements,
        from: from.node_id,
        to: to.node_id,
        provenance: prov.clone(),
    };
    debug_assert!(matches!(
        prov.fact_source,
        FactSource::TreeSitter | FactSource::Prism
    ));
    Some(edge)
}

/// Directory part of a repo-relative path (`""` at the root).
pub fn parent_dir(rel_path: &str) -> &str {
    rel_path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The repository directory an import path names, given the root module
/// path: `example.invalid/app/internal/svc` under module
/// `example.invalid/app` is `internal/svc`; the module path itself is the
/// root. Any other import path is outside the module.
fn go_package_dir(import_path: &str, module_path: &str) -> Option<String> {
    if import_path == module_path {
        return Some(String::new());
    }
    import_path
        .strip_prefix(module_path)
        .and_then(|rest| rest.strip_prefix('/'))
        .map(str::to_string)
}

/// Attempt to build an edge from a raw Go embedding observation. The
/// declaring type is looked up in the file's own package directory; the
/// embedded type in the same directory when written bare, or in the
/// directory its import path names under the root module when written
/// `pkg.Type`. Both must be unique in-repository definitions, else `None`:
/// an embedded standard-library or dependency type is an observation.
pub fn resolve_go_embed(
    embed: &crate::go_syntax::RawEmbed,
    file_dir: &str,
    package_imports: &[(String, String)],
    module_path: Option<&str>,
    index: &DefinitionIndex,
    prov: &Provenance,
) -> Option<Edge> {
    let Resolution::Unique(from) = index.resolve_go_type(&embed.declarant, file_dir) else {
        return None;
    };
    let target_dir = match &embed.target.package {
        None => file_dir.to_string(),
        Some(selector) => {
            let (_, import_path) = package_imports
                .iter()
                .find(|(local, _)| local == selector)?;
            go_package_dir(import_path, module_path?)?
        }
    };
    let Resolution::Unique(to) = index.resolve_go_type(&embed.target.name, &target_dir) else {
        return None;
    };
    let edge = Edge {
        id: edge_id(embed.kind, &from.node_id, &to.node_id, prov.span.start_byte),
        kind: embed.kind,
        from: from.node_id,
        to: to.node_id,
        provenance: prov.clone(),
    };
    debug_assert!(matches!(prov.fact_source, FactSource::TreeSitter));
    Some(edge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{NodeKind, Span};

    fn mk_node(name: &str, id: &str, kind: NodeKind, path: &str) -> Node {
        Node {
            python: None,
            id: id.to_string(),
            kind,
            name: name.to_string(),
            symbol_key: format!("rust:{path}#{name}"),
            provenance: Provenance {
                repository: "r".into(),
                commit_sha: "c".into(),
                rel_path: path.to_string(),
                span: Span::from_ts(0, 10, (0, 0), (0, 10)),
                file_hash: "h".into(),
                extractor_version: "0.1.0".to_string(),
                fact_source: FactSource::TreeSitter,
                confidence: 1,
            },
        }
    }

    #[test]
    fn unique_resolution() {
        let nodes = vec![mk_node(
            "Foo",
            "rust:type:a.rs#Foo",
            NodeKind::Type(crate::schema::TypeKind::Struct),
            "a.rs",
        )];
        let idx = DefinitionIndex::from_nodes(&nodes);
        assert!(matches!(
            idx.resolve_qualified_with_sep("Foo"),
            Resolution::Unique(_)
        ));
        assert!(matches!(
            idx.resolve_qualified_with_sep("Bar"),
            Resolution::Unknown
        ));
    }

    #[test]
    fn ambiguous_resolution_drops() {
        let nodes = vec![
            mk_node(
                "Foo",
                "rust:type:a.rs#Foo",
                NodeKind::Type(crate::schema::TypeKind::Struct),
                "a.rs",
            ),
            mk_node(
                "Foo",
                "rust:type:b.rs#Foo",
                NodeKind::Type(crate::schema::TypeKind::Struct),
                "b.rs",
            ),
        ];
        let idx = DefinitionIndex::from_nodes(&nodes);
        assert!(matches!(
            idx.resolve_qualified_with_sep("Foo"),
            Resolution::Ambiguous(_)
        ));
    }

    #[test]
    fn a_go_type_is_scoped_to_its_package_directory() {
        let nodes = vec![
            mk_node(
                "Base",
                "go:type:internal/svc/base.go#svc::Base",
                NodeKind::Type(crate::schema::TypeKind::Struct),
                "internal/svc/base.go",
            ),
            mk_node(
                "Base",
                "go:type:pkg/other/base.go#other::Base",
                NodeKind::Type(crate::schema::TypeKind::Struct),
                "pkg/other/base.go",
            ),
            mk_node(
                "Base",
                "go:function:internal/svc/ctor.go#svc::Base",
                NodeKind::Function,
                "internal/svc/ctor.go",
            ),
        ];
        let idx = DefinitionIndex::from_nodes(&nodes);
        let Resolution::Unique(def) = idx.resolve_go_type("Base", "internal/svc") else {
            panic!("one type named Base in internal/svc");
        };
        assert_eq!(def.node_id, "go:type:internal/svc/base.go#svc::Base");
        assert!(matches!(
            idx.resolve_go_type("Base", "cmd"),
            Resolution::Unknown
        ));
    }

    #[test]
    fn an_import_path_maps_to_a_directory_only_under_the_root_module() {
        let module = "example.invalid/app";
        assert_eq!(
            go_package_dir("example.invalid/app/internal/svc", module).as_deref(),
            Some("internal/svc")
        );
        assert_eq!(go_package_dir(module, module).as_deref(), Some(""));
        assert_eq!(go_package_dir("example.invalid/apple/x", module), None);
        assert_eq!(go_package_dir("github.com/x/y", module), None);
    }
}
