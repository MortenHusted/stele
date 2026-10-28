//! Rendering facts as a `.lemmaspec` evidence file.
//!
//! An evidence file is a `spec` holding only facts and expectations; the
//! checker it is evaluated against supplies the relations and rules. This
//! module knows the artifact grammar's lexical rules and nothing about
//! codemap.

use std::fmt::Write as _;

/// One asserted fact with a producer-assigned basis. Legacy CLI review
/// selections have no provenance; rule-card claims retain their input citation
/// while their explicit basis stays reviewer-declared.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Fact {
    pub relation: &'static str,
    pub args: Vec<String>,
    pub provenance: Option<String>,
    pub basis: Basis,
}

/// Explicit producer classification, independent of any provenance string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Basis {
    Snapshot,
    Policy,
    ReviewerDeclared,
}

impl Basis {
    const fn tag(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Policy => "policy",
            Self::ReviewerDeclared => "reviewer_declared",
        }
    }
}

/// Presentation-only metadata. Lexical symbol identities stay unchanged.
#[derive(Debug, Clone)]
pub struct SymbolLabel {
    pub symbol: String,
    pub label: String,
    pub source: Option<String>,
}

pub struct Readable<'a> {
    pub snapshot_id: &'a str,
    pub notes: &'a str,
    pub labels: &'a [SymbolLabel],
}

/// One claim the evidence must satisfy: a single-atom query and the exact
/// number of rows it may return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expectation {
    pub id: &'static str,
    pub query: &'static str,
    pub count: usize,
}

/// Render a value as a fact argument: bare when it is a lowercase-initial
/// identifier or an integer, quoted otherwise.
///
/// Uppercase-initial bare tokens are variables inside a query, so anything
/// that is not a plain lowercase identifier or a number is quoted to stay
/// a symbol wherever it is repeated; an integer must stay bare to be an
/// integer argument.
pub fn term(value: &str) -> String {
    let mut chars = value.chars();
    let bare = matches!(chars.next(), Some(first) if first.is_ascii_lowercase() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if bare || is_integer(value) {
        value.to_string()
    } else {
        quoted(value)
    }
}

fn is_integer(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Derive a legal spec name (`[A-Za-z_][A-Za-z0-9_]*`) from any text, such
/// as a repository URL.
pub fn spec_name(raw: &str) -> String {
    let mut name: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        name.insert(0, '_');
    }
    name
}

/// Render the evidence file. `doc` lines become the comment above the
/// spec; facts are numbered per relation in the order given, so callers
/// sort them first for a byte-stable file.
pub fn render(doc: &[String], spec: &str, facts: &[Fact], expectations: &[Expectation]) -> String {
    render_with_metadata(doc, spec, facts, expectations, None)
}

pub fn render_with_metadata(
    doc: &[String],
    spec: &str,
    facts: &[Fact],
    expectations: &[Expectation],
    readable: Option<&Readable<'_>>,
) -> String {
    render_with_declarations(doc, spec, facts, expectations, readable, "")
}

/// Render a self-contained artifact with typed relations and rules.
pub fn render_with_declarations(
    doc: &[String],
    spec: &str,
    facts: &[Fact],
    expectations: &[Expectation],
    readable: Option<&Readable<'_>>,
    declarations: &str,
) -> String {
    let mut out = String::new();
    for line in doc {
        if line.is_empty() {
            out.push_str("//\n");
        } else {
            let _ = writeln!(out, "// {line}");
        }
    }
    let _ = writeln!(out, "\nspec {spec} {{");
    out.push_str(declarations);

    if let Some(metadata) = readable {
        let _ = writeln!(out, "  notes {{ text: {} }}", quoted(metadata.notes));
        for label in metadata.labels {
            let _ = write!(
                out,
                "  symbol {} {{ label: {}",
                term(&label.symbol),
                quoted(&label.label)
            );
            if let Some(source) = &label.source {
                let _ = write!(out, " source: {}", quoted(source));
            }
            out.push_str(" }\n");
        }
        out.push('\n');
    }

    let mut previous_relation: Option<&str> = None;
    let mut index = 0usize;
    for fact in facts {
        if previous_relation != Some(fact.relation) {
            if previous_relation.is_some() {
                out.push('\n');
            }
            previous_relation = Some(fact.relation);
            index = 0;
        }
        let args: Vec<String> = fact.args.iter().map(|a| term(a)).collect();
        let _ = write!(
            out,
            "  fact {}_{index} {{ relation: {} args: [{}]",
            fact.relation,
            fact.relation,
            args.join(", ")
        );
        if let Some(metadata) = readable {
            let source = fact
                .provenance
                .as_deref()
                .unwrap_or("reviewer:explicit-selection");
            let location_link = (fact.basis == Basis::Snapshot && !source.starts_with("snapshot:"))
                .then(|| source.parse::<codemap_graph::SourceLocation>().ok())
                .flatten()
                .and_then(|location| crate::source_link(&location.rel_path, location.start_line));
            let _ = write!(
                out,
                " basis: {} source: {}",
                fact.basis.tag(),
                quoted(location_link.as_deref().unwrap_or(source))
            );
            if fact.basis == Basis::Snapshot {
                let _ = write!(out, " identity: {}", quoted(metadata.snapshot_id));
            }
        }
        if let Some(provenance) = &fact.provenance {
            let _ = write!(out, " provenance: [{}]", quoted(provenance));
        }
        out.push_str(" }\n");
        index += 1;
    }

    if !expectations.is_empty() {
        out.push_str("\n  // ---- the claims this evidence must satisfy ----\n");
    }
    for expectation in expectations {
        let _ = write!(
            out,
            "\n  expect {} {{\n    query: {}\n    count: {}\n  }}\n",
            expectation.id,
            quoted(expectation.query),
            expectation.count
        );
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provenance_string_cannot_upgrade_a_reviewer_declaration() {
        let fact = Fact {
            relation: "claim",
            args: vec!["sample".into()],
            provenance: Some("snapshot:pretended-identity".into()),
            basis: Basis::ReviewerDeclared,
        };
        let text = render_with_metadata(
            &[],
            "evidence",
            &[fact],
            &[],
            Some(&Readable {
                snapshot_id: "actual-identity",
                notes: "Review this claim.",
                labels: &[],
            }),
        );
        assert!(text.contains("basis: reviewer_declared source: \"snapshot:pretended-identity\""));
        assert!(!text.contains(" identity: "));
    }

    #[test]
    fn readable_snapshot_sources_link_to_files_but_preserve_legacy_provenance() {
        for (provenance, link) in [
            ("src/a file.rs:3", "./src/a%20file.rs#L3"),
            ("src/a.rs:3-5", "./src/a.rs#L3"),
        ] {
            let fact = Fact {
                relation: "located",
                args: vec!["a".into()],
                provenance: Some(provenance.into()),
                basis: Basis::Snapshot,
            };
            let text = render_with_metadata(
                &[],
                "evidence",
                &[fact],
                &[],
                Some(&Readable {
                    snapshot_id: "tree",
                    notes: "",
                    labels: &[],
                }),
            );
            assert!(text.contains(&format!("source: {}", quoted(link))));
            assert!(text.contains(&format!("provenance: [{}]", quoted(provenance))));
        }
    }

    #[test]
    fn integers_stay_bare_so_they_are_integer_arguments() {
        assert_eq!(term("12"), "12");
        assert_eq!(term("-3"), "-3");
        assert_eq!(term("0"), "0");
        assert_eq!(term("1e3"), "\"1e3\"");
        assert_eq!(term("12a"), "\"12a\"");
        assert_eq!(term("-"), "\"-\"");
    }

    #[test]
    fn lowercase_identifiers_stay_bare_and_everything_else_is_quoted() {
        assert_eq!(term("main"), "main");
        assert_eq!(term("_x1"), "_x1");
        assert_eq!(term("Main"), "\"Main\"");
        assert_eq!(term("src/a.rs"), "\"src/a.rs\"");
        assert_eq!(
            term("rust:function:src/a.rs#m::f"),
            "\"rust:function:src/a.rs#m::f\""
        );
        assert_eq!(term("1st"), "\"1st\"");
        assert_eq!(term(""), "\"\"");
    }

    #[test]
    fn quoting_escapes_exactly_what_the_artifact_lexer_accepts() {
        assert_eq!(term("a\"b"), "\"a\\\"b\"");
        assert_eq!(term("a\\b"), "\"a\\\\b\"");
        assert_eq!(term("a\nb\tc\rd"), "\"a\\nb\\tc\\rd\"");
    }

    #[test]
    fn spec_names_are_identifiers_derived_from_any_text() {
        assert_eq!(
            spec_name("https://github.com/Example/project.git"),
            "https___github_com_Example_project_git"
        );
        assert_eq!(spec_name("local:abc123"), "local_abc123");
        assert_eq!(spec_name("9lives"), "_9lives");
        assert_eq!(spec_name(""), "_");
    }

    #[test]
    fn facts_are_numbered_per_relation_and_review_decisions_carry_no_provenance() {
        let facts = vec![
            Fact {
                basis: Basis::Snapshot,
                relation: "calls",
                args: vec!["a".into(), "b".into()],
                provenance: Some("src/a.rs:3".into()),
            },
            Fact {
                basis: Basis::ReviewerDeclared,
                relation: "core_file",
                args: vec!["src/a.rs".into()],
                provenance: None,
            },
            Fact {
                basis: Basis::ReviewerDeclared,
                relation: "core_file",
                args: vec!["src/b.rs".into()],
                provenance: None,
            },
        ];
        let expectations = vec![Expectation {
            id: "nothing_is_dead",
            query: "dead(F)",
            count: 0,
        }];
        let text = render(
            &["first line".into(), String::new()],
            "evidence",
            &facts,
            &expectations,
        );
        assert_eq!(
            text,
            "// first line\n//\n\nspec evidence {\n  fact calls_0 { relation: calls args: [a, b] provenance: [\"src/a.rs:3\"] }\n\n  fact core_file_0 { relation: core_file args: [\"src/a.rs\"] }\n  fact core_file_1 { relation: core_file args: [\"src/b.rs\"] }\n\n  // ---- the claims this evidence must satisfy ----\n\n  expect nothing_is_dead {\n    query: \"dead(F)\"\n    count: 0\n  }\n}\n"
        );
    }
}
