//! `policy:association/ruby`: which classes an association macro relates.
//!
//! The extractor proves what a class body says: `belongs_to :owner,
//! class_name: "Person"` is a call named `belongs_to` with those literal
//! arguments. That the call relates the class to `Person` is a framework
//! convention, so the relation is derived here, under this policy's name,
//! and never enters a snapshot's edges.
//!
//! A declaration relates two classes only when the policy resolves it to
//! exactly one class on record. Anything else is returned as unresolved
//! with its reason. Custom inflections are not read: an inferred name is
//! tried as written and under a few singular forms, and it counts only when
//! exactly one of them names a class.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use codemap_graph::lookup::{Lexical, lexical_constant};
use codemap_graph::schema::{
    CallArgument, CallSite, EdgeKind, Language, Literal, Node, NodeKind, Snapshot, Span, TypeKind,
};

/// Classes related by `belongs_to`, `has_one`, `has_many`,
/// `has_and_belongs_to_many` and `delegated_type`.
pub const ASSOCIATION: &str = "association";

const TO_ONE: [&str; 2] = ["belongs_to", "has_one"];
const TO_MANY: [&str; 2] = ["has_many", "has_and_belongs_to_many"];
const DELEGATED_TYPE: &str = "delegated_type";

/// Plural endings English does not form with `s`, tried at a word boundary.
const IRREGULAR: [(&str, &str); 9] = [
    ("people", "person"),
    ("children", "child"),
    ("mice", "mouse"),
    ("geese", "goose"),
    ("feet", "foot"),
    ("teeth", "tooth"),
    ("data", "datum"),
    ("media", "medium"),
    ("criteria", "criterion"),
];

/// How a declaration's target was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    /// The `class_name:` option names the class.
    ClassName,
    /// The association's own name does.
    Inferred,
    /// A polymorphic `belongs_to :x`: the target declares `as: :x` back.
    PolymorphicAs,
    /// A class listed in `delegated_type … types:`.
    DelegatedType,
}

impl Rule {
    pub const fn name(self) -> &'static str {
        match self {
            Self::ClassName => "class_name",
            Self::Inferred => "inferred",
            Self::PolymorphicAs => "polymorphic_as",
            Self::DelegatedType => "delegated_type",
        }
    }
}

/// Why a declaration relates nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    /// The name or the class is not a literal.
    Dynamic,
    /// The call is not inside a class or module body.
    NoOwner,
    /// The call is in a module no class on record includes.
    NoIncluder,
    /// No class on record has the name.
    NoClass,
    /// Several classes do, or several candidate names each name one.
    Ambiguous,
    /// A polymorphic `belongs_to` no class declares `as:` back to.
    NoPolymorphicTarget,
}

impl Reason {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dynamic => "dynamic",
            Self::NoOwner => "no_owner",
            Self::NoIncluder => "no_includer",
            Self::NoClass => "no_class",
            Self::Ambiguous => "ambiguous",
            Self::NoPolymorphicTarget => "no_polymorphic_target",
        }
    }
}

/// The macro call a relation or an unresolved entry comes from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Declaration {
    pub rel_path: String,
    pub span: (usize, usize),
    pub lines: (u32, u32),
    pub macro_name: String,
    /// The association name, when it is a literal.
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Association {
    /// Node id of the class the association belongs to.
    pub from: String,
    /// Node id of the class it reaches.
    pub to: String,
    pub rule: Rule,
    /// The `through:` association, when one is named.
    pub through: Option<String>,
    /// Node id of the module that declares it for the classes including it.
    pub via: Option<String>,
    pub declaration: Declaration,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Unresolved {
    pub reason: Reason,
    pub declaration: Declaration,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Associations {
    pub relations: Vec<Association>,
    pub unresolved: Vec<Unresolved>,
}

fn qualified(node: &Node) -> &str {
    node.symbol_key
        .split_once('#')
        .map_or(node.name.as_str(), |(_, name)| name)
}

/// Constant names compare without case or underscores, so `:api_keys`
/// finds `APIKey` without knowing which words a project treats as acronyms.
fn key(written: &str) -> String {
    written.replace('_', "").replace('/', "::").to_lowercase()
}

/// `name` as written and under the singular forms it could be a plural of.
fn singular_candidates(name: &str) -> Vec<String> {
    let mut out = vec![name.to_string()];
    for (plural, singular) in IRREGULAR {
        if let Some(stem) = name.strip_suffix(plural)
            && (stem.is_empty() || stem.ends_with('_'))
        {
            out.push(format!("{stem}{singular}"));
        }
    }
    if let Some(stem) = name.strip_suffix("men") {
        out.push(format!("{stem}man"));
    }
    if let Some(stem) = name.strip_suffix("ies") {
        out.push(format!("{stem}y"));
    }
    if let Some(stem) = name.strip_suffix("ves") {
        out.push(format!("{stem}f"));
        out.push(format!("{stem}fe"));
    }
    if let Some(stem) = name.strip_suffix("es") {
        out.push(stem.to_string());
        out.push(format!("{stem}is"));
    }
    if let Some(stem) = name.strip_suffix('s') {
        out.push(stem.to_string());
    }
    let mut seen = BTreeSet::new();
    out.retain(|candidate| !candidate.is_empty() && seen.insert(candidate.clone()));
    out
}

fn option<'a>(arguments: &'a [CallArgument], keyword: &str) -> Option<&'a Literal> {
    arguments
        .iter()
        .find(|argument| argument.keyword.as_deref() == Some(keyword))
        .map(|argument| &argument.value)
}

fn written(literal: &Literal) -> Option<&str> {
    match literal {
        Literal::Symbol(text) | Literal::String(text) | Literal::Constant(text) => Some(text),
        _ => None,
    }
}

struct Index<'a> {
    classes: HashMap<String, Vec<&'a Node>>,
    containers: HashMap<&'a str, Vec<&'a Node>>,
    nodes: HashMap<&'a str, &'a Node>,
    includers: HashMap<&'a str, Vec<&'a str>>,
}

impl<'a> Index<'a> {
    fn new(snapshot: &'a Snapshot) -> Self {
        let mut index = Self {
            classes: HashMap::new(),
            containers: HashMap::new(),
            nodes: HashMap::new(),
            includers: HashMap::new(),
        };
        for node in &snapshot.nodes {
            let class = node.kind == NodeKind::Type(TypeKind::Class);
            if class {
                index
                    .classes
                    .entry(key(qualified(node)))
                    .or_default()
                    .push(node);
            }
            if class || node.kind == NodeKind::Module {
                index
                    .containers
                    .entry(node.provenance.rel_path.as_str())
                    .or_default()
                    .push(node);
                index.nodes.insert(node.id.as_str(), node);
            }
        }
        for edge in &snapshot.edges {
            if edge.kind == EdgeKind::Implements {
                index
                    .includers
                    .entry(edge.to.as_str())
                    .or_default()
                    .push(edge.from.as_str());
            }
        }
        index
    }

    /// The class or module whose body holds the call: the smallest one
    /// enclosing it, or none when two tie.
    fn body(&self, site: &CallSite) -> Option<&'a Node> {
        let mut enclosing: Vec<_> = self
            .containers
            .get(site.rel_path.as_str())?
            .iter()
            .filter(|node| {
                node.provenance.span.start_byte <= site.span.start_byte
                    && site.span.end_byte <= node.provenance.span.end_byte
            })
            .collect();
        let extent =
            |node: &&&Node| node.provenance.span.end_byte - node.provenance.span.start_byte;
        enclosing.sort_by_key(extent);
        match enclosing.as_slice() {
            [only] => Some(only),
            [first, second, ..] if extent(first) < extent(second) => Some(first),
            _ => None,
        }
    }

    /// The classes a declaration in `body` belongs to: the class itself, or
    /// every class that includes the module, directly or through modules.
    fn owners(&self, body: &'a Node) -> Vec<&'a Node> {
        if body.kind != NodeKind::Module {
            return vec![body];
        }
        let mut classes = BTreeMap::new();
        let mut seen = BTreeSet::from([body.id.as_str()]);
        let mut pending = vec![body.id.as_str()];
        while let Some(module) = pending.pop() {
            for includer in self.includers.get(module).into_iter().flatten() {
                let Some(node) = self.nodes.get(includer) else {
                    continue;
                };
                if node.kind == NodeKind::Module {
                    if seen.insert(includer) {
                        pending.push(includer);
                    }
                } else {
                    classes.insert(node.id.as_str(), *node);
                }
            }
        }
        classes.into_values().collect()
    }

    /// The one class `candidates` name from inside `owner`, looked up as a
    /// class resolves a constant: under the owner, under each of its
    /// namespaces, then at the top level.
    fn class(&self, owner: &Node, candidates: &[String]) -> Result<&'a Node, Reason> {
        let name = key(qualified(owner));
        let scope: Vec<String> = name
            .match_indices("::")
            .map(|(end, _)| name[..end].to_string())
            .chain(std::iter::once(name.clone()))
            .collect();
        let mut found = BTreeMap::new();
        for candidate in candidates {
            let written = candidate
                .strip_prefix("::")
                .map_or_else(|| key(candidate), |absolute| format!("::{}", key(absolute)));
            match lexical_constant(&self.classes, &written, &scope) {
                Lexical::Unique(class) => {
                    found.insert(class.id.as_str(), *class);
                }
                Lexical::Ambiguous(_) => return Err(Reason::Ambiguous),
                Lexical::Unknown => {}
            }
        }
        let mut found = found.into_values();
        match (found.next(), found.next()) {
            (Some(class), None) => Ok(class),
            (None, _) => Err(Reason::NoClass),
            (Some(_), Some(_)) => Err(Reason::Ambiguous),
        }
    }
}

struct Declared<'a> {
    owner: &'a Node,
    via: Option<&'a Node>,
    site: &'a CallSite,
    declaration: Declaration,
}

impl Declared<'_> {
    fn option(&self, keyword: &str) -> Option<&Literal> {
        option(&self.site.arguments, keyword)
    }

    fn polymorphic(&self) -> bool {
        self.site.raw_name == "belongs_to"
            && self.option("polymorphic") == Some(&Literal::Boolean(true))
    }
}

fn declaration(site: &CallSite) -> Declaration {
    let Span {
        start_byte,
        end_byte,
        start_line,
        end_line,
        ..
    } = site.span;
    Declaration {
        rel_path: site.rel_path.clone(),
        span: (start_byte, end_byte),
        lines: (start_line, end_line),
        macro_name: site.raw_name.clone(),
        name: site
            .arguments
            .first()
            .filter(|argument| argument.keyword.is_none())
            .and_then(|argument| match &argument.value {
                Literal::Symbol(name) | Literal::String(name) => Some(name.clone()),
                _ => None,
            }),
    }
}

/// Every association macro with a literal name, once per class it belongs
/// to. Those without a name or a class go to `out` as unresolved.
fn declared<'a>(
    snapshot: &'a Snapshot,
    index: &Index<'a>,
    out: &mut Associations,
) -> Vec<Declared<'a>> {
    let mut declared = Vec::new();
    for site in &snapshot.call_sites {
        let name = site.raw_name.as_str();
        if site.arguments.is_empty()
            || !(TO_ONE.contains(&name) || TO_MANY.contains(&name) || name == DELEGATED_TYPE)
        {
            continue;
        }
        let declaration = declaration(site);
        let body = index.body(site);
        let owners = body.map_or_else(Vec::new, |body| index.owners(body));
        let reason = if declaration.name.is_none() {
            Some(Reason::Dynamic)
        } else if body.is_none() {
            Some(Reason::NoOwner)
        } else if owners.is_empty() {
            Some(Reason::NoIncluder)
        } else {
            None
        };
        if let Some(reason) = reason {
            out.unresolved.push(Unresolved {
                reason,
                declaration,
            });
            continue;
        }
        for owner in owners {
            declared.push(Declared {
                owner,
                via: body.filter(|body| body.kind == NodeKind::Module),
                site,
                declaration: declaration.clone(),
            });
        }
    }
    declared
}

/// Where a declaration that names its class points, or why nowhere.
fn targets<'a>(index: &Index<'a>, entry: &Declared<'a>) -> Vec<(Result<&'a Node, Reason>, Rule)> {
    let name = entry.declaration.name.as_deref().unwrap_or_default();
    if entry.site.raw_name == DELEGATED_TYPE {
        return match entry.option("types") {
            Some(Literal::Array(types)) if types.iter().all(|t| written(t).is_some()) => types
                .iter()
                .filter_map(written)
                .map(|class| {
                    (
                        index.class(entry.owner, &[class.to_string()]),
                        Rule::DelegatedType,
                    )
                })
                .collect(),
            _ => vec![(Err(Reason::Dynamic), Rule::DelegatedType)],
        };
    }
    let (candidates, rule) = match entry.option("class_name").map(written) {
        Some(Some(class)) => (vec![class.to_string()], Rule::ClassName),
        Some(None) => return vec![(Err(Reason::Dynamic), Rule::ClassName)],
        None if TO_MANY.contains(&entry.site.raw_name.as_str()) => {
            (singular_candidates(name), Rule::Inferred)
        }
        None => (vec![name.to_string()], Rule::Inferred),
    };
    vec![(index.class(entry.owner, &candidates), rule)]
}

impl Associations {
    fn record(&mut self, entry: &Declared<'_>, target: Result<&str, Reason>, rule: Rule) {
        match target {
            Ok(class) => self.relations.push(Association {
                from: entry.owner.id.clone(),
                to: class.to_string(),
                rule,
                through: entry
                    .option("through")
                    .and_then(written)
                    .map(str::to_string),
                via: entry.via.map(|module| module.id.clone()),
                declaration: entry.declaration.clone(),
            }),
            Err(reason) => self.unresolved.push(Unresolved {
                reason,
                declaration: entry.declaration.clone(),
            }),
        }
    }
}

/// Derive the associations of a Ruby snapshot. Other languages have none.
pub fn associations(snapshot: &Snapshot) -> Associations {
    let mut out = Associations::default();
    if snapshot.identity.language != Language::Ruby {
        return out;
    }
    let index = Index::new(snapshot);
    let declared = declared(snapshot, &index, &mut out);
    // (class reached, its `as:` name) -> the classes that declare it.
    let mut declares_as: BTreeMap<(&str, &str), BTreeSet<&str>> = BTreeMap::new();
    for entry in declared.iter().filter(|entry| !entry.polymorphic()) {
        for (target, rule) in targets(&index, entry) {
            if let (Ok(class), Some(Literal::Symbol(role))) = (&target, entry.option("as")) {
                declares_as
                    .entry((class.id.as_str(), role.as_str()))
                    .or_default()
                    .insert(entry.owner.id.as_str());
            }
            out.record(entry, target.map(|class| class.id.as_str()), rule);
        }
    }
    for entry in declared.iter().filter(|entry| entry.polymorphic()) {
        let role = entry.declaration.name.as_deref().unwrap_or_default();
        match declares_as.get(&(entry.owner.id.as_str(), role)) {
            Some(classes) => {
                for class in classes {
                    out.record(entry, Ok(class), Rule::PolymorphicAs);
                }
            }
            None => out.record(entry, Err(Reason::NoPolymorphicTarget), Rule::PolymorphicAs),
        }
    }
    out.relations.sort();
    out.relations.dedup();
    out.unresolved.sort();
    out.unresolved.dedup();
    out
}
