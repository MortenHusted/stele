//! Print a parsed artifact back to `.lemmaspec` source.
//!
//! The output parses to an artifact equal to the input and is byte-stable
//! across print/parse cycles. Only what the artifact keeps survives: the
//! spec doc and each declaration's doc are re-emitted as `//` comments, while
//! section headings and other free-standing comments, which document
//! nothing, are gone. Declarations come out in the artifact's own order,
//! which is sorted by name.

use std::fmt::Write;

use crate::artifact::{
    Artifact, ExpectationDecl, FactDecl, FactValue, MutationDecl, RelationDecl, RuleDecl, ValueType,
};

/// Render `artifact` as source text that parses back to an equal artifact.
pub fn print_artifact(artifact: &Artifact) -> String {
    let mut out = String::new();
    if let Some(doc) = &artifact.doc {
        comment(&mut out, doc, "");
        out.push('\n');
    }
    let _ = writeln!(out, "spec {} {{", artifact.name);
    if let Some(notes) = &artifact.notes {
        let _ = writeln!(out, "  notes {{ text: {} }}\n", quoted(notes));
    }

    let mut first = true;
    let mut block = |out: &mut String, doc: &Option<String>, body: String| {
        if !first {
            out.push('\n');
        }
        first = false;
        if let Some(doc) = doc {
            comment(out, doc, "  ");
        }
        out.push_str(&body);
    };

    for symbol in &artifact.symbols {
        let mut body = format!(
            "  symbol {} {{\n    label: {}\n",
            text(&symbol.value),
            quoted(&symbol.label)
        );
        if let Some(source) = &symbol.source {
            let _ = writeln!(body, "    source: {}", quoted(source));
        }
        body.push_str("  }\n");
        block(&mut out, &symbol.doc, body);
    }
    for relation in &artifact.relations {
        block(&mut out, &relation.doc, print_relation(relation));
    }
    for fact in &artifact.facts {
        block(&mut out, &fact.doc, print_fact(fact));
    }
    for rule in &artifact.rules {
        block(&mut out, &rule.doc, print_rule(rule));
    }
    for expectation in &artifact.expectations {
        block(&mut out, &expectation.doc, print_expectation(expectation));
    }
    for mutation in &artifact.mutations {
        block(&mut out, &mutation.doc, print_mutation(mutation));
    }

    out.push_str("}\n");
    out
}

fn comment(out: &mut String, doc: &str, indent: &str) {
    for line in doc.lines() {
        if line.is_empty() {
            let _ = writeln!(out, "{indent}//");
        } else {
            let _ = writeln!(out, "{indent}// {line}");
        }
    }
}

fn print_relation(relation: &RelationDecl) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "  relation {} {{", relation.name);
    let args: Vec<&str> = relation
        .args
        .iter()
        .map(|arg| match arg {
            ValueType::Symbol => "symbol",
            ValueType::Integer => "integer",
        })
        .collect();
    let _ = writeln!(out, "    args: [{}]", args.join(", "));
    if !relation.roles.is_empty() {
        let _ = writeln!(out, "    roles: [{}]", texts(&relation.roles));
    }
    if let Some(reads) = &relation.reads {
        let _ = writeln!(out, "    reads: {}", quoted(reads));
    }
    out.push_str("  }\n");
    out
}

fn print_fact(fact: &FactDecl) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "  fact {} {{", fact.id);
    let _ = writeln!(out, "    relation: {}", text(&fact.relation));
    let args: Vec<String> = fact
        .args
        .iter()
        .map(|arg| match arg {
            FactValue::Symbol(symbol) => text(symbol),
            FactValue::Integer(integer) => integer.to_string(),
        })
        .collect();
    let _ = writeln!(out, "    args: [{}]", args.join(", "));
    #[allow(clippy::cast_possible_truncation)]
    let confidence = (fact.confidence * 100.0).round() as i64;
    if confidence != 100 {
        let _ = writeln!(out, "    confidence: {confidence}");
    }
    if !fact.provenance.is_empty() {
        let _ = writeln!(out, "    provenance: [{}]", texts(&fact.provenance));
    }
    if let Some(basis) = &fact.basis {
        let _ = writeln!(out, "    basis: {}", basis.kind.as_str());
        let _ = writeln!(out, "    source: {}", quoted(&basis.source));
        if let Some(identity) = &basis.identity {
            let _ = writeln!(out, "    identity: {}", quoted(identity));
        }
    }
    out.push_str("  }\n");
    out
}

fn print_rule(rule: &RuleDecl) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "  rule {} {{", rule.id);
    let _ = writeln!(out, "    derive: {}", quoted(&rule.derive));
    if rule.condition_ids.is_empty() {
        out.push_str("    when: [\n");
        for condition in &rule.when {
            let _ = writeln!(out, "      {},", quoted(condition));
        }
        out.push_str("    ]\n");
    } else {
        out.push_str("    when: {\n");
        for (id, condition) in rule.condition_ids.iter().zip(&rule.when) {
            let _ = writeln!(out, "      {id}: {}", quoted(condition));
        }
        out.push_str("    }\n");
    }
    out.push_str("  }\n");
    out
}

fn print_expectation(expectation: &ExpectationDecl) -> String {
    format!(
        "  expect {} {{ query: {} count: {} }}\n",
        expectation.id,
        quoted(&expectation.query),
        expectation.count
    )
}

fn print_mutation(mutation: &MutationDecl) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "  mutation {} {{", mutation.id);
    let _ = writeln!(out, "    operator: {}", mutation.operator.as_str());
    if let Some(relation) = &mutation.relation {
        let _ = writeln!(out, "    relation: {}", text(relation));
    }
    if !mutation.except.is_empty() {
        let _ = writeln!(out, "    except: [{}]", texts(&mutation.except));
    }
    if let Some(must_fail) = &mutation.must_fail {
        let _ = writeln!(out, "    must_fail: {}", text(must_fail));
    }
    out.push_str("  }\n");
    out
}

fn texts(values: &[String]) -> String {
    values
        .iter()
        .map(|value| text(value))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A value the parser accepts as either an identifier or a string: bare
/// when it lexes as an identifier, quoted otherwise.
fn text(value: &str) -> String {
    if is_identifier(value) {
        value.to_string()
    } else {
        quoted(value)
    }
}

fn is_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_artifact;

    fn examples() -> Vec<(String, String)> {
        let directory = concat!(env!("CARGO_MANIFEST_DIR"), "/examples");
        let mut sources: Vec<_> = std::fs::read_dir(directory)
            .expect("examples directory")
            .map(|entry| entry.expect("directory entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "stele"))
            .map(|path| {
                let source = std::fs::read_to_string(&path).expect("read example");
                (path.display().to_string(), source)
            })
            .collect();
        sources.sort();
        assert!(!sources.is_empty(), "no examples found");
        sources
    }

    #[test]
    fn every_example_round_trips_through_the_printer() {
        for (path, source) in examples() {
            let original = parse_artifact(&source).expect("example parses");
            let printed = print_artifact(&original);
            let reparsed = parse_artifact(&printed)
                .unwrap_or_else(|error| panic!("{path}: printed source does not parse: {error}"));
            assert_eq!(
                reparsed, original,
                "{path}: round trip changed the artifact"
            );
            assert_eq!(
                print_artifact(&reparsed),
                printed,
                "{path}: printing is not byte-stable"
            );
        }
    }

    #[test]
    fn docs_escapes_confidence_and_quoted_symbols_round_trip() {
        let source = r#"
            // What does this artifact ask?
            //
            // A second paragraph, with a "quote" and a back\slash.
            spec printer_fixture {
              // The item relation.
              relation item { args: [symbol, integer] roles: [item, score] reads: "{item} scored {score}" }
              relation flagged { args: [symbol] }

              // A fact whose symbol needs quoting.
              fact odd_symbol {
                relation: item
                args: ["needs quoting", -3]
                confidence: 42
                provenance: [plan, "docs/a b.md", "tab\there"]
              }
              fact plain { relation: item args: [plain, 1] } // trailing note

              rule named { derive: "flagged(X)" when: { present: "item(X, S)", enough: "S >= 1" } }
              rule listed { derive: "flagged(\"needs quoting\")" when: ["item(\"needs quoting\", _)"] }

              expect flagged_two { query: "flagged(X)" count: 2 }

              mutation rules { operator: drop_rule except: [listed] }
              mutation conditions { operator: drop_condition except: ["named.enough"] must_fail: flagged_two }
              mutation facts { operator: drop_fact relation: item except: [plain] }
            }
        "#;

        let original = parse_artifact(source).expect("fixture parses");
        let printed = print_artifact(&original);
        let reparsed = parse_artifact(&printed).expect("printed fixture parses");

        assert_eq!(reparsed, original, "{printed}");
        assert_eq!(print_artifact(&reparsed), printed);
        assert_eq!(
            original.doc.as_deref(),
            Some("What does this artifact ask?\n\nA second paragraph, with a \"quote\" and a back\\slash.")
        );
        assert!(
            printed.contains("args: [\"needs quoting\", -3]"),
            "{printed}"
        );
        assert!(printed.contains("confidence: 42"), "{printed}");
        assert!(printed.contains("\"tab\\there\""), "{printed}");
        assert!(
            printed.contains("// A fact whose symbol needs quoting."),
            "{printed}"
        );
        assert!(printed.contains("// trailing note"), "{printed}");
    }
}
