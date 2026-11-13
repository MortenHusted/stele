//! Versioned Laravel source conventions over PHP parser observations.

use std::collections::{HashMap, HashSet};

use codemap_graph::schema::{
    BladeReference, BladeReferenceKind, Language, Node, NodeKind, PhpClass, PhpLiteral, PhpMethod,
    PhpMethodBody, PhpName, Provenance, Snapshot, TypeKind,
};

use crate::template::{Derived, Related};

pub const ASSOCIATION: &str = "laravel-association-v1";
pub const TEMPLATE_RENDER: &str = "laravel-template-render-v1";

const ROOTS: [&str; 2] = [
    "Illuminate\\Database\\Eloquent\\Model",
    "Illuminate\\Foundation\\Auth\\User",
];
const TRAITS: [&str; 7] = [
    "Illuminate\\Database\\Eloquent\\Factories\\HasFactory",
    "Illuminate\\Database\\Eloquent\\SoftDeletes",
    "Illuminate\\Notifications\\Notifiable",
    "Illuminate\\Notifications\\HasDatabaseNotifications",
    "Illuminate\\Notifications\\RoutesNotifications",
    "Laravel\\Sanctum\\HasApiTokens",
    "Laravel\\Scout\\Searchable",
];

struct Classes<'a> {
    nodes: HashMap<&'a str, &'a Node>,
    names: HashMap<String, Vec<&'a PhpClass>>,
    owners: HashMap<&'a str, &'a str>,
    methods: HashMap<&'a str, Vec<&'a PhpMethod>>,
    incomplete: HashSet<&'a str>,
    valid_ownership: bool,
}

impl<'a> Classes<'a> {
    fn new(snapshot: &'a Snapshot) -> Self {
        let nodes: HashMap<_, _> = snapshot
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let mut names: HashMap<String, Vec<&PhpClass>> = HashMap::new();
        let mut methods: HashMap<&str, Vec<&PhpMethod>> = HashMap::new();
        let method_owners = codemap_graph::php::PhpMethodOwners::new(snapshot);
        let owners: HashMap<_, _> = snapshot
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Method)
            .filter_map(|method| {
                method_owners
                    .owner(method)
                    .map(|owner| (method.id.as_str(), owner.id.as_str()))
            })
            .collect();
        if let Some(php) = &snapshot.php {
            for class in &php.classes {
                names
                    .entry(class.qualified.to_ascii_lowercase())
                    .or_default()
                    .push(class);
            }
            for method in &php.methods {
                if let Some(owner) = owners.get(method.node.as_str()) {
                    methods.entry(owner).or_default().push(method);
                }
            }
        }
        Self {
            valid_ownership: snapshot
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKind::Method)
                .all(|method| owners.contains_key(method.id.as_str())),
            nodes,
            names,
            owners,
            methods,
            incomplete: snapshot
                .php
                .as_ref()
                .map(|php| php.incomplete_files.iter().map(String::as_str).collect())
                .unwrap_or_default(),
        }
    }

    fn class(&self, name: &str) -> Option<&'a PhpClass> {
        let classes = self.names.get(&name.to_ascii_lowercase())?;
        (classes.len() == 1).then_some(classes[0])
    }

    fn model(&self, class: &PhpClass, seen: &mut HashSet<String>) -> bool {
        if !self.valid_ownership
            || self
                .nodes
                .get(class.node.as_str())
                .is_some_and(|node| self.incomplete.contains(node.provenance.rel_path.as_str()))
            || !seen.insert(class.node.clone())
            || self
                .names
                .get(&class.qualified.to_ascii_lowercase())
                .is_none_or(|names| names.len() != 1)
            || self
                .nodes
                .get(class.node.as_str())
                .is_none_or(|node| node.kind != NodeKind::Type(TypeKind::Class))
        {
            return false;
        }
        let Some(parent) = &class.parent else {
            return false;
        };
        if ROOTS
            .iter()
            .any(|root| root.eq_ignore_ascii_case(&parent.qualified))
        {
            return !self
                .names
                .contains_key(&parent.qualified.to_ascii_lowercase());
        }
        self.class(&parent.qualified)
            .is_some_and(|parent| self.model(parent, seen))
    }

    fn builder(
        &self,
        class: &PhpClass,
        name: &str,
        seen: &mut HashSet<String>,
    ) -> Result<(), &'static str> {
        if self
            .nodes
            .get(class.node.as_str())
            .is_some_and(|node| self.incomplete.contains(node.provenance.rel_path.as_str()))
        {
            return Err("incomplete_source");
        }
        if !seen.insert(class.node.clone()) {
            return Err("cyclic_composition");
        }
        if class.trait_adaptations {
            return Err("trait_adaptation");
        }
        if self.declares_method(class, name) {
            return Err("builder_override");
        }
        self.builder_composition(class, name, seen)?;
        seen.remove(&class.node);
        Ok(())
    }
    fn builder_composition(
        &self,
        class: &PhpClass,
        name: &str,
        seen: &mut HashSet<String>,
    ) -> Result<(), &'static str> {
        for reference in &class.traits {
            if let Some(trait_class) = self.trait_class(&reference.qualified)? {
                self.builder(trait_class, name, seen)?;
            }
        }
        if let Some(parent) = self.parent_class(class)? {
            self.builder(parent, name, seen)?;
        }
        Ok(())
    }

    fn declares_method(&self, class: &PhpClass, name: &str) -> bool {
        self.methods
            .get(class.node.as_str())
            .is_some_and(|methods| {
                methods.iter().any(|method| {
                    self.nodes
                        .get(method.node.as_str())
                        .is_some_and(|node| node.name.eq_ignore_ascii_case(name))
                })
            })
    }

    fn trait_class(&self, qualified: &str) -> Result<Option<&'a PhpClass>, &'static str> {
        if let Some(traits) = self.names.get(&qualified.to_ascii_lowercase()) {
            if traits.len() != 1
                || self
                    .nodes
                    .get(traits[0].node.as_str())
                    .is_none_or(|node| node.kind != NodeKind::Trait)
            {
                return Err("ambiguous_trait");
            }
            return Ok(Some(traits[0]));
        }
        if TRAITS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(qualified))
        {
            Ok(None)
        } else {
            Err("unknown_trait")
        }
    }

    fn parent_class(&self, class: &PhpClass) -> Result<Option<&'a PhpClass>, &'static str> {
        let Some(parent) = &class.parent else {
            return Ok(None);
        };
        if let Some(parent) = self.class(&parent.qualified) {
            return Ok(Some(parent));
        }
        if ROOTS
            .iter()
            .any(|root| root.eq_ignore_ascii_case(&parent.qualified))
        {
            Ok(None)
        } else {
            Err("unknown_parent")
        }
    }

    fn association_candidates(&self, owner: &PhpClass, method: &PhpMethod) -> usize {
        if !self.model(owner, &mut HashSet::new()) {
            return 0;
        }
        method
            .member_calls
            .iter()
            .filter(|call| call.receiver == "$this" && relationship(&call.name).is_some())
            .count()
    }

    fn association_target<'b>(
        &self,
        arguments: &'b [PhpLiteral],
    ) -> Result<(&'a PhpClass, &'b PhpName), &'static str> {
        if arguments
            .iter()
            .skip(1)
            .any(|argument| !matches!(argument, PhpLiteral::String { .. }))
        {
            return Err("unsupported_arguments");
        }
        let Some(PhpLiteral::Class { name: target }) = arguments.first() else {
            return Err("dynamic_target");
        };
        let target_class = self
            .class(&target.qualified)
            .ok_or("missing_or_ambiguous_target")?;
        if !self.model(target_class, &mut HashSet::new()) {
            return Err("unverified_target_model");
        }
        Ok((target_class, target))
    }

    fn association(
        &self,
        owner: &PhpClass,
        method: &PhpMethod,
        call: &ReturnedAssociation<'_>,
    ) -> Result<Option<Related>, &'static str> {
        if method.is_static || call.receiver != "$this" {
            return Err("unsupported_receiver");
        }
        if !self.model(owner, &mut HashSet::new()) {
            return Err("unverified_model");
        }
        self.builder(owner, call.name, &mut HashSet::new())?;
        let (target_class, target) = self.association_target(call.arguments)?;
        let Some(node) = self.nodes.get(method.node.as_str()) else {
            return Ok(None);
        };
        Ok(Some(Related {
            from: owner.node.clone(),
            to: target_class.node.clone(),
            rule: call.rule,
            declaration: format!("{}: {}({})", node.name, call.name, target.qualified),
            rel_path: call.provenance.rel_path.clone(),
            lines: (
                call.provenance.span.start_line,
                call.provenance.span.end_line,
            ),
        }))
    }
}

fn relationship(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "hasone" => Some("has_one"),
        "hasmany" => Some("has_many"),
        "belongsto" => Some("belongs_to"),
        "belongstomany" => Some("belongs_to_many"),
        _ => None,
    }
}

fn unresolved(derived: &mut Derived, reason: &'static str) {
    *derived.unresolved.entry(reason).or_default() += 1;
}

struct ReturnedAssociation<'a> {
    receiver: &'a str,
    name: &'a str,
    arguments: &'a [PhpLiteral],
    provenance: &'a Provenance,
    rule: &'static str,
}

impl<'a> ReturnedAssociation<'a> {
    fn from_method(method: &'a PhpMethod) -> Result<Self, &'static str> {
        let PhpMethodBody::ReturnCall {
            receiver,
            name,
            arguments,
            provenance,
        } = &method.body
        else {
            return Err("unsupported_body");
        };
        let rule = relationship(name).ok_or("unsupported_chain")?;
        Ok(Self {
            receiver,
            name,
            arguments,
            provenance,
            rule,
        })
    }
}

pub fn associations(snapshot: &Snapshot) -> Derived {
    let mut derived = Derived::default();
    if snapshot.identity.language != Language::Php {
        return derived;
    }
    let Some(php) = &snapshot.php else {
        return derived;
    };
    let classes = Classes::new(snapshot);
    for method in &php.methods {
        let Some(owner) = classes
            .owners
            .get(method.node.as_str())
            .and_then(|owner| php.classes.iter().find(|class| class.node == *owner))
        else {
            continue;
        };
        let call = match ReturnedAssociation::from_method(method) {
            Ok(call) => call,
            Err(reason) => {
                for _ in 0..classes.association_candidates(owner, method) {
                    unresolved(&mut derived, reason);
                }
                continue;
            }
        };
        match classes.association(owner, method, &call) {
            Ok(Some(relation)) => derived.relations.push(relation),
            Ok(None) => {}
            Err(reason) => unresolved(&mut derived, reason),
        }
    }
    derived
}

fn component_class(target: &str) -> String {
    format!(
        "App\\View\\Components\\{}",
        target
            .split('.')
            .map(|part| part
                .split('-')
                .map(|word| {
                    let mut letters = word.chars();
                    letters
                        .next()
                        .map(|first| first.to_uppercase().collect::<String>() + letters.as_str())
                        .unwrap_or_default()
                })
                .collect::<String>())
            .collect::<Vec<_>>()
            .join("\\")
    )
}

fn supported_view_name(target: &str) -> bool {
    target
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !target.split('.').any(str::is_empty)
}

struct Views<'a> {
    files: HashMap<&'a str, &'a str>,
    classes: Classes<'a>,
}

impl<'a> Views<'a> {
    fn new(snapshot: &'a Snapshot) -> Self {
        Self {
            files: snapshot
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKind::File)
                .map(|node| (node.provenance.rel_path.as_str(), node.id.as_str()))
                .collect(),
            classes: Classes::new(snapshot),
        }
    }

    fn target(&self, target: &str, component: bool) -> Result<&'a str, &'static str> {
        let view = format!(
            "resources/views/{}{}.blade.php",
            if component { "components/" } else { "" },
            target.replace('.', "/")
        );
        if component
            && self
                .classes
                .names
                .contains_key(&component_class(target).to_ascii_lowercase())
        {
            return Err(if self.files.contains_key(view.as_str()) {
                "ambiguous_component"
            } else {
                "class_component"
            });
        }
        self.files
            .get(view.as_str())
            .copied()
            .ok_or("missing_template")
    }

    fn reference(&self, reference: &BladeReference) -> Result<Option<Related>, &'static str> {
        let (rule, component) = template_rule(reference.kind)?;
        let target = reference.target.as_deref().ok_or("dynamic_target")?;
        if component && (target == "slot" || target.starts_with("slot:")) {
            return Err("unsupported_slot");
        }
        if !supported_view_name(target) {
            return Err("unsupported_view_name");
        }
        if !reference
            .provenance
            .rel_path
            .starts_with("resources/views/")
        {
            return Err("custom_view_root");
        }
        let to = self.target(target, component)?;
        let Some(from) = self.files.get(reference.provenance.rel_path.as_str()) else {
            return Ok(None);
        };
        Ok(Some(Related {
            from: (*from).to_owned(),
            to: to.to_owned(),
            rule,
            declaration: format!("{rule} {target}"),
            rel_path: reference.provenance.rel_path.clone(),
            lines: (
                reference.provenance.span.start_line,
                reference.provenance.span.end_line,
            ),
        }))
    }
}

const fn template_rule(kind: BladeReferenceKind) -> Result<(&'static str, bool), &'static str> {
    match kind {
        BladeReferenceKind::Extends => Ok(("extends", false)),
        BladeReferenceKind::Include => Ok(("include", false)),
        BladeReferenceKind::Component => Ok(("component", false)),
        BladeReferenceKind::Tag => Ok(("component", true)),
        BladeReferenceKind::ParseError => Err("parse_error"),
        _ => Err("unsupported_directive"),
    }
}

pub fn templates(snapshot: &Snapshot) -> Derived {
    let mut derived = Derived::default();
    if snapshot.identity.language != Language::Php {
        return derived;
    }
    let Some(php) = &snapshot.php else {
        return derived;
    };
    let views = Views::new(snapshot);
    let erroneous: HashSet<_> = php
        .blade
        .iter()
        .filter(|reference| reference.kind == BladeReferenceKind::ParseError)
        .map(|reference| reference.provenance.rel_path.as_str())
        .collect();
    for reference in &php.blade {
        if erroneous.contains(reference.provenance.rel_path.as_str())
            && reference.kind != BladeReferenceKind::ParseError
        {
            continue;
        }
        match views.reference(reference) {
            Ok(Some(relation)) => derived.relations.push(relation),
            Ok(None) => {}
            Err(reason) => unresolved(&mut derived, reason),
        }
    }
    derived
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_trait_surface_agrees_with_the_profile() {
        let profile: serde_json::Value =
            serde_json::from_str(include_str!("../../../benchmarks/php/laravel-profile.json"))
                .unwrap();
        let traits: Vec<_> = profile["trusted_traits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["qualified_name"].as_str().unwrap())
            .collect();
        assert_eq!(traits.len(), TRAITS.len());
        assert!(TRAITS.iter().all(|name| traits.contains(name)));
        for entry in profile["trusted_traits"].as_array().unwrap() {
            assert!(
                !entry["declared_methods"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|method| {
                        ["hasOne", "hasMany", "belongsTo", "belongsToMany"]
                            .iter()
                            .any(|name| method.as_str().unwrap().eq_ignore_ascii_case(name))
                    })
            );
        }
    }
}
