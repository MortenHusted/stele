//! Apply a checker's relations and rules to evidence supplied by another file.
//!
//! A checker is an ordinary self-contained artifact: its own facts are a
//! fixture and its expectations and mutation policies are its self-test. A
//! check replaces that fixture with the evidence file's facts and evaluates
//! the evidence file's expectations instead. The result is one closed
//! artifact evaluated exactly like a walk; nothing is resolved by reference.

use crate::artifact::{parse_artifact, walk_parsed_artifact, Artifact, ArtifactError, WalkReport};
use crate::printer::print_artifact;

/// Evaluate `checker`'s vocabulary and rules over `evidence`'s facts and
/// expectations. The evidence file may declare only facts and expectations.
pub fn check_artifact(checker: &str, evidence: &str) -> Result<WalkReport, ArtifactError> {
    let checker = parse_artifact(checker)?;
    let evidence = parse_artifact(evidence)?;
    check_parsed_artifact(&checker, &evidence)
}

/// Evaluate a parsed `checker`'s vocabulary and rules over parsed `evidence`.
pub fn check_parsed_artifact(
    checker: &Artifact,
    evidence: &Artifact,
) -> Result<WalkReport, ArtifactError> {
    walk_parsed_artifact(&bind(checker.clone(), evidence.clone())?)
}

/// Bind `checker` to `evidence` and print the result as a self-contained
/// artifact: the proof record for this checker over this evidence, which
/// walk, mutate, project, and render accept unchanged. Its doc names both
/// sources before the evidence's own doc. Notes from both artifacts survive;
/// evidence labels override checker labels for symbols retained by the binding.
pub fn bind_artifact(checker: &str, evidence: &str) -> Result<String, ArtifactError> {
    let checker = parse_artifact(checker)?;
    let evidence = parse_artifact(evidence)?;
    let bound = bind_parsed_artifact(&checker, &evidence)?;
    Ok(print_artifact(&bound))
}

/// Bind a parsed `checker` to parsed `evidence` as a self-contained [`Artifact`].
pub fn bind_parsed_artifact(
    checker: &Artifact,
    evidence: &Artifact,
) -> Result<Artifact, ArtifactError> {
    let header = format!("Bound from {} over {}.", checker.name, evidence.name);
    let mut bound = bind(checker.clone(), evidence.clone())?;
    bound.doc = Some(match bound.doc {
        Some(doc) => format!("{header}\n\n{doc}"),
        None => header,
    });
    Ok(bound)
}

fn bind(checker: Artifact, evidence: Artifact) -> Result<Artifact, ArtifactError> {
    // A mutation cannot be parsed without a rule or relation to target, so
    // rejecting those two is enough to keep the evidence to facts and
    // expectations.
    for (kind, declared) in [
        ("relation", !evidence.relations.is_empty()),
        ("rule", !evidence.rules.is_empty()),
    ] {
        if declared {
            return Err(ArtifactError::new(format!(
                "evidence `{}` declares a {kind}; evidence may only assert facts and expectations",
                evidence.name
            )));
        }
    }

    let mut bound = Artifact {
        name: evidence.name,
        doc: evidence.doc,
        notes: match (checker.notes, evidence.notes) {
            (Some(checker), Some(evidence)) if checker != evidence => {
                Some(format!("{checker}\n\n{evidence}"))
            }
            (Some(notes), _) | (_, Some(notes)) => Some(notes),
            (None, None) => None,
        },
        symbols: evidence.symbols,
        relations: checker.relations,
        facts: evidence.facts,
        rules: checker.rules,
        expectations: evidence.expectations,
        mutations: Vec::new(),
    };
    if !checker.symbols.is_empty() {
        let referenced = bound.referenced_symbols()?;
        for symbol in checker.symbols {
            if referenced.contains(&symbol.value)
                && !bound
                    .symbols
                    .iter()
                    .any(|entry| entry.value == symbol.value)
            {
                bound.symbols.push(symbol);
            }
        }
        bound
            .symbols
            .sort_by(|left, right| left.value.cmp(&right.value));
    }
    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHECKER: &str = r#"
        spec checker {
          relation input { args: [symbol] }
          relation flagged { args: [symbol] }
          fact fixture { relation: input args: [fixture] }
          rule flag_every_input { derive: "flagged(X)" when: ["input(X)"] }
          expect fixture_is_flagged { query: "flagged(fixture)" count: 1 }
          mutation rules_bite { operator: drop_rule }
        }
    "#;

    #[test]
    fn evidence_replaces_the_fixture_and_its_expectations() {
        let evidence = r#"
            spec evidence {
              fact real { relation: input args: [real] }
              expect real_is_flagged { query: "flagged(real)" count: 1 }
              expect fixture_is_gone { query: "flagged(fixture)" count: 0 }
            }
        "#;

        let report = check_artifact(CHECKER, evidence).expect("check succeeds");

        assert_eq!(report.spec, "evidence");
        assert_eq!(report.status, "clean");
        assert_eq!(report.asserted, 1);
        assert_eq!(report.derived, 1);
        assert_eq!(report.expectations.len(), 2);
    }

    #[test]
    fn a_bound_artifact_is_self_contained_and_names_its_sources() {
        let evidence = r#"
            // Is the real input flagged?
            spec evidence {
              fact real { relation: input args: [real] }
              expect real_is_flagged { query: "flagged(real)" count: 1 }
            }
        "#;

        let bound = bind_artifact(CHECKER, evidence).expect("bind succeeds");
        let artifact = parse_artifact(&bound).expect("bound artifact parses");

        assert_eq!(artifact.name, "evidence");
        assert_eq!(
            artifact.doc.as_deref(),
            Some("Bound from checker over evidence.\n\nIs the real input flagged?")
        );
        assert_eq!(artifact.relations.len(), 2);
        assert_eq!(artifact.rules.len(), 1);
        assert_eq!(artifact.facts.len(), 1);
        assert_eq!(artifact.expectations.len(), 1);
        assert!(artifact.mutations.is_empty());
        let report = crate::walk_artifact(&bound).expect("bound artifact walks");
        assert_eq!(report.status, "clean");
    }

    #[test]
    fn evidence_is_validated_against_the_checker_vocabulary() {
        let evidence = r#"
            spec evidence {
              fact stray { relation: unknown args: [x] }
            }
        "#;

        let error = check_artifact(CHECKER, evidence).expect_err("unknown relation is rejected");

        assert!(
            error.to_string().contains("unknown relation `unknown`"),
            "{error}"
        );
    }

    #[test]
    fn evidence_may_not_redefine_the_checker() {
        for (declaration, kind) in [
            ("relation extra { args: [symbol] }", "relation"),
            (
                r#"rule extra { derive: "flagged(X)" when: ["input(X)"] }"#,
                "rule",
            ),
        ] {
            let evidence = format!("spec evidence {{ {declaration} }}");

            let error = check_artifact(CHECKER, &evidence).expect_err("declaration is rejected");

            assert!(
                error.to_string().contains(&format!("declares a {kind}")),
                "{kind}: {error}"
            );
        }
    }
}
