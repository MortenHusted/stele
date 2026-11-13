use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::schema::{EdgeKind, Node, NodeKind, Provenance, Snapshot};

pub const BLADE_PARSER: &str = "tree-sitter-blade b5291d1ba207a8ebb8383b2ecb8a8a6535210a50";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhpSyntax {
    pub incomplete_files: Vec<String>,
    pub anonymous_classes: Vec<Provenance>,
    pub classes: Vec<PhpClass>,
    pub methods: Vec<PhpMethod>,
    pub blade: Vec<BladeReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhpName {
    pub written: String,
    pub qualified: String,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhpClass {
    pub node: String,
    /// PHP namespace and declared name; callable scopes never enter this binding.
    pub qualified: String,
    pub parent: Option<PhpName>,
    pub traits: Vec<PhpName>,
    pub trait_adaptations: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhpMethod {
    pub member_calls: Vec<PhpMemberCall>,
    pub node: String,
    pub is_static: bool,
    pub body: PhpMethodBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhpMemberCall {
    pub receiver: String,
    pub name: String,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PhpMethodBody {
    Other,
    ReturnCall {
        receiver: String,
        name: String,
        arguments: Vec<PhpLiteral>,
        provenance: Box<Provenance>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PhpLiteral {
    Class {
        name: PhpName,
    },
    String {
        value: String,
        provenance: Provenance,
    },
    Other {
        provenance: Provenance,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BladeReferenceKind {
    Extends,
    Include,
    IncludeIf,
    IncludeWhen,
    IncludeUnless,
    IncludeFirst,
    Component,
    Tag,
    DynamicComponent,
    ParseError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BladeReference {
    pub kind: BladeReferenceKind,
    pub target: Option<String>,
    pub provenance: Provenance,
}

/// Admits only a unique containment edge to the nearest lexical PHP scope.
pub struct PhpMethodOwners<'a> {
    scopes: Vec<(&'a Provenance, Option<&'a Node>)>,
    files: HashMap<&'a str, &'a Node>,
    owners: HashMap<&'a str, Vec<&'a str>>,
}

impl<'a> PhpMethodOwners<'a> {
    pub fn new(snapshot: &'a Snapshot) -> Self {
        let mut scopes: Vec<_> = snapshot
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Type(_) | NodeKind::Trait))
            .map(|node| (&node.provenance, Some(node)))
            .collect();
        if let Some(php) = &snapshot.php {
            scopes.extend(php.anonymous_classes.iter().map(|source| (source, None)));
        }
        let files = snapshot
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::File)
            .map(|node| (node.provenance.rel_path.as_str(), node))
            .collect();
        let mut owners: HashMap<&str, Vec<&str>> = HashMap::new();
        for edge in snapshot
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Contains)
        {
            owners.entry(&edge.to).or_default().push(&edge.from);
        }
        Self {
            scopes,
            files,
            owners,
        }
    }

    pub fn owner(&self, method: &Node) -> Option<&'a Node> {
        let enclosing: Vec<_> = self
            .scopes
            .iter()
            .filter(|(source, _)| {
                source.rel_path == method.provenance.rel_path
                    && source.span.start_byte <= method.provenance.span.start_byte
                    && source.span.end_byte >= method.provenance.span.end_byte
            })
            .collect();
        let nearest = enclosing
            .iter()
            .min_by_key(|(source, _)| source.span.end_byte.saturating_sub(source.span.start_byte));
        let declaration = match nearest {
            Some((source, owner)) => {
                if enclosing
                    .iter()
                    .filter(|(other, _)| other.span == source.span)
                    .count()
                    != 1
                {
                    return None;
                }
                owner.or_else(|| self.files.get(source.rel_path.as_str()).copied())?
            }
            None => *self.files.get(method.provenance.rel_path.as_str())?,
        };
        let owners = self.owners.get(method.id.as_str())?;
        (owners == &[declaration.id.as_str()]).then_some(declaration)
    }
}
