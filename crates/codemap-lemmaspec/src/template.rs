//! `policy:template-render/ruby` and `policy:template-owner/ruby`: what a
//! template renders, and which action or class a template belongs to.
//!
//! The extractor records a template as a file and the call sites of its
//! tags. That `render "posts/card"` means the file `posts/_card.html.erb`,
//! or that `show.html.erb` belongs to `PostsController#show`, is a framework
//! convention, so both relations are derived here under their policy's name
//! and never enter a snapshot's edges.
//!
//! A `render` relates a template to a partial or a class only when exactly
//! one file or class answers to the name; anything else is counted by
//! reason. Lookup through a controller's superclasses and through custom
//! view paths is not read.

use std::collections::{BTreeMap, HashMap};

use codemap_graph::schema::{
    CallSite, Language, Literal, Node, NodeKind, Snapshot, TypeKind, is_erb_template,
};

use crate::association::{Named, autoload_home, classes, named, qualified};

/// What a template's `render` calls render: a partial, another template or
/// a class.
pub const TEMPLATE_RENDER: &str = "template-render";
/// Which controller or mailer action, or which class beside it, a template
/// is the template of.
pub const TEMPLATE_OWNER: &str = "template-owner";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Related {
    pub from: String,
    pub to: String,
    /// `partial`, `template` or `component` for a render; `action` or
    /// `sidecar` for an owner.
    pub rule: &'static str,
    /// What the relation rests on, as a reader would write it.
    pub declaration: String,
    pub rel_path: String,
    pub lines: (u32, u32),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Derived {
    pub relations: Vec<Related>,
    /// Renders that relate nothing, counted by reason: `dynamic`,
    /// `no_partial`, `no_template`, `no_class` or `ambiguous`.
    pub unresolved: BTreeMap<&'static str, usize>,
}

/// Where a template sits: `app/views/posts/_card.html.erb` is the partial
/// `card` of `posts` under `app/views/`, in format `html`.
struct Place<'a> {
    /// The path up to and including its `views/` directory; empty for a
    /// template outside any.
    root: &'a str,
    /// The directories between the root and the file.
    dir: &'a str,
    /// The file name up to its first dot, without a partial's underscore.
    name: &'a str,
    partial: bool,
    /// What lies between the name and `.erb`.
    format: &'a str,
}

fn place(rel_path: &str) -> Place<'_> {
    let views = rel_path
        .rfind("/views/")
        .map(|at| at + "/views/".len())
        .or_else(|| rel_path.starts_with("views/").then_some("views/".len()));
    let (root, rest) = rel_path.split_at(views.unwrap_or(0));
    let (dir, file) = rest.rsplit_once('/').unwrap_or(("", rest));
    let (stem, extensions) = file.split_once('.').unwrap_or((file, ""));
    Place {
        root,
        dir,
        name: stem.strip_prefix('_').unwrap_or(stem),
        partial: stem.starts_with('_'),
        format: extensions
            .rsplit_once('.')
            .map_or("", |(format, _erb)| format),
    }
}

/// Whether it is a partial, its directory and its name.
type Name<'a> = (bool, &'a str, &'a str);

struct Views<'a> {
    /// The templates answering to each name.
    by_name: HashMap<Name<'a>, Vec<(&'a Node, Place<'a>)>>,
}

impl<'a> Views<'a> {
    fn new(snapshot: &'a Snapshot) -> Self {
        let mut by_name: HashMap<_, Vec<_>> = HashMap::new();
        for node in &snapshot.nodes {
            let path = node.provenance.rel_path.as_str();
            if node.kind == NodeKind::File && is_erb_template(path) {
                let place = place(path);
                by_name
                    .entry((place.partial, place.dir, place.name))
                    .or_default()
                    .push((node, place));
            }
        }
        Self { by_name }
    }

    /// The one template `(dir, name)` names for `from`: under the same
    /// views root when several roots have one, and in the same format when
    /// several formats do.
    fn one(
        &self,
        partial: bool,
        dir: &str,
        name: &str,
        from: &Place<'_>,
    ) -> Result<&'a Node, bool> {
        let Some(all) = self.by_name.get(&(partial, dir, name)) else {
            return Err(false);
        };
        let mut matching: Vec<_> = all.iter().collect();
        for narrower in [
            |candidate: &Place<'_>, from: &Place<'_>| candidate.root == from.root,
            |candidate: &Place<'_>, from: &Place<'_>| candidate.format == from.format,
        ] {
            if matching.len() > 1 && matching.iter().any(|(_, place)| narrower(place, from)) {
                matching.retain(|(_, place)| narrower(place, from));
            }
        }
        match matching.as_slice() {
            [(node, _)] => Ok(node),
            _ => Err(true),
        }
    }
}

/// What a `render` call names: a partial or template path, or a class.
enum Target<'a> {
    Partial(&'a str),
    Template(&'a str),
    Class(&'a str),
    Dynamic,
}

fn target(site: &CallSite) -> Target<'_> {
    let keyword = |name: &str| {
        site.arguments
            .iter()
            .find(|argument| argument.keyword.as_deref() == Some(name))
            .and_then(|argument| match &argument.value {
                Literal::String(path) => Some(path.as_str()),
                _ => None,
            })
    };
    match site.arguments.first() {
        Some(first) if first.keyword.is_none() => match &first.value {
            Literal::String(path) => Target::Partial(path),
            Literal::Constant(class)
            | Literal::ConstantCall {
                constant: class, ..
            } => Target::Class(class),
            _ => Target::Dynamic,
        },
        _ => keyword("partial").map_or_else(
            || keyword("template").map_or(Target::Dynamic, Target::Template),
            Target::Partial,
        ),
    }
}

/// Derive what each template of a Ruby snapshot renders.
pub fn renders(snapshot: &Snapshot) -> Derived {
    let mut out = Derived::default();
    if snapshot.identity.language != Language::Ruby {
        return out;
    }
    let views = Views::new(snapshot);
    let classes = classes(snapshot);
    let files: HashMap<&str, &Node> = snapshot
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::File)
        .map(|node| (node.provenance.rel_path.as_str(), node))
        .collect();
    for site in &snapshot.call_sites {
        if site.raw_name != "render" || !is_erb_template(&site.rel_path) {
            continue;
        }
        let Some(from) = files.get(site.rel_path.as_str()) else {
            continue;
        };
        let here = place(&site.rel_path);
        let path = |written: &'_ str, partial: bool| {
            let (dir, name) = written.rsplit_once('/').unwrap_or((here.dir, written));
            views
                .one(partial, dir.trim_start_matches('/'), name, &here)
                .map_err(|several| match (several, partial) {
                    (true, _) => "ambiguous",
                    (false, true) => "no_partial",
                    (false, false) => "no_template",
                })
        };
        let (found, rule, declaration) = match target(site) {
            Target::Partial(written) => (
                path(written, true),
                "partial",
                format!("render \"{written}\""),
            ),
            Target::Template(written) => (
                path(written, false),
                "template",
                format!("render template: \"{written}\""),
            ),
            Target::Class(written) => (
                match named(&classes, written, &[]) {
                    Named::One(class) => Ok(class),
                    Named::None => Err("no_class"),
                    Named::Several => Err("ambiguous"),
                },
                "component",
                format!("render {written}"),
            ),
            Target::Dynamic => (Err("dynamic"), "", String::new()),
        };
        match found {
            Ok(to) => out.relations.push(Related {
                from: from.id.clone(),
                to: to.id.clone(),
                rule,
                declaration,
                rel_path: site.rel_path.clone(),
                lines: (site.span.start_line, site.span.end_line),
            }),
            Err(reason) => *out.unresolved.entry(reason).or_default() += 1,
        }
    }
    out.relations.sort();
    out.relations.dedup();
    out
}

/// The directory of templates a controller or mailer file renders from:
/// `app/controllers/admin/posts_controller.rb` -> `app/views/`, `admin/posts`.
fn rendering_home(rel_path: &str) -> Option<(String, &str)> {
    for (directory, suffix, keep) in [
        ("controllers/", "_controller.rb", false),
        ("mailers/", "_mailer.rb", true),
    ] {
        let Some(at) = rel_path.rfind(directory) else {
            continue;
        };
        if at > 0 && !rel_path[..at].ends_with('/') {
            continue;
        }
        let inside = &rel_path[at + directory.len()..];
        let Some(stem) = inside.strip_suffix(suffix) else {
            continue;
        };
        let dir = if keep {
            &inside[..inside.len() - ".rb".len()]
        } else {
            stem
        };
        return Some((format!("{}views/", &rel_path[..at]), dir));
    }
    None
}

/// Derive which action or class each template of a Ruby snapshot belongs to.
pub fn owners(snapshot: &Snapshot) -> Derived {
    let mut out = Derived::default();
    if snapshot.identity.language != Language::Ruby {
        return out;
    }
    let views = Views::new(snapshot);
    // Directory and file stem -> the templates sitting there.
    let mut beside: HashMap<(&str, &str), Vec<&Node>> = HashMap::new();
    for templates in views.by_name.values() {
        for (node, _) in templates {
            let path = node.provenance.rel_path.as_str();
            let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
            let stem = file.split_once('.').map_or(file, |(stem, _)| stem);
            beside.entry((dir, stem)).or_default().push(node);
        }
    }
    for node in &snapshot.nodes {
        let path = node.provenance.rel_path.as_str();
        let lines = (
            node.provenance.span.start_line,
            node.provenance.span.end_line,
        );
        let mut relate = |template: &Node, rule, declaration: String| {
            out.relations.push(Related {
                from: node.id.clone(),
                to: template.id.clone(),
                rule,
                declaration,
                rel_path: path.to_string(),
                lines,
            });
        };
        if node.kind == NodeKind::Method && !node.name.starts_with("self.") {
            let Some((root, dir)) = rendering_home(path) else {
                continue;
            };
            let action = views.by_name.get(&(false, dir, node.name.as_str()));
            for (template, place) in action.into_iter().flatten() {
                if place.root == root {
                    relate(template, "action", format!("def {}", node.name));
                }
            }
        } else if node.kind == NodeKind::Type(TypeKind::Class) && autoload_home(node) {
            // `card_component.rb` beside `card_component.html.erb`, or
            // beside a directory of that name holding it.
            let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
            let Some(stem) = file.strip_suffix(".rb") else {
                continue;
            };
            let nested = format!("{dir}/{stem}");
            let nested = nested.trim_start_matches('/');
            for place in [dir, nested] {
                for template in beside.get(&(place, stem)).into_iter().flatten() {
                    relate(template, "sidecar", format!("class {}", qualified(node)));
                }
            }
        }
    }
    out.relations.sort();
    out.relations.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_template_path_names_its_root_directory_name_and_format() {
        let card = place("engines/shop/app/views/admin/posts/_card.html.erb");
        assert_eq!(
            (card.root, card.dir, card.name, card.partial, card.format),
            (
                "engines/shop/app/views/",
                "admin/posts",
                "card",
                true,
                "html"
            )
        );
        let show = place("app/views/posts/show.turbo_stream.erb");
        assert_eq!(
            (show.root, show.dir, show.name, show.partial, show.format),
            ("app/views/", "posts", "show", false, "turbo_stream")
        );
        let beside = place("app/components/card_component.html.erb");
        assert_eq!(
            (beside.root, beside.dir, beside.name),
            ("", "app/components", "card_component")
        );
    }

    #[test]
    fn controllers_and_mailers_name_the_directory_they_render_from() {
        assert_eq!(
            rendering_home("app/controllers/admin/posts_controller.rb"),
            Some(("app/views/".to_string(), "admin/posts"))
        );
        assert_eq!(
            rendering_home("engines/shop/app/mailers/order_mailer.rb"),
            Some(("engines/shop/app/views/".to_string(), "order_mailer"))
        );
        assert_eq!(rendering_home("app/models/post.rb"), None);
        assert_eq!(
            rendering_home("app/subcontrollers/posts_controller.rb"),
            None
        );
    }
}
