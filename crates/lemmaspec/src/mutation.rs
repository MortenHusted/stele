//! Deterministic mutation analysis for one self-contained artifact.

use std::num::NonZeroUsize;
use std::panic;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use serde::Serialize;

use crate::artifact::{
    evaluate_prepared_expectations, evaluate_prepared_mutant, parse_artifact, prepare_artifact,
    Artifact, ArtifactError, MutationDecl, MutationOperator, PreparedArtifact, WalkExpectation,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MutationTarget {
    Rule {
        rule: String,
    },
    Condition {
        rule: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<String>,
        index: usize,
        expression: String,
    },
    Fact {
        fact: String,
        relation: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationStatus {
    Killed,
    Survived,
    Rejected,
    Excluded,
}

impl MutationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Killed => "killed",
            Self::Survived => "survived",
            Self::Rejected => "rejected",
            Self::Excluded => "excluded",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MutationResult {
    pub id: String,
    pub policy: String,
    pub operator: MutationOperator,
    pub target: MutationTarget,
    pub status: MutationStatus,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed_expectations: Vec<WalkExpectation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MutationSummary {
    pub total: usize,
    pub executed: usize,
    pub killed: usize,
    pub survived: usize,
    pub rejected: usize,
    pub excluded: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MutationPolicyReport {
    pub id: String,
    pub operator: MutationOperator,
    pub status: String,
    pub summary: MutationSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MutationReport {
    pub spec: String,
    pub status: String,
    pub baseline_status: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub baseline_failures: Vec<WalkExpectation>,
    pub summary: MutationSummary,
    pub policies: Vec<MutationPolicyReport>,
    pub mutations: Vec<MutationResult>,
}

pub fn mutate_artifact(source: &str) -> Result<MutationReport, ArtifactError> {
    let artifact = parse_artifact(source)?;
    mutate_parsed_artifact(&artifact)
}

pub fn mutate_parsed_artifact(artifact: &Artifact) -> Result<MutationReport, ArtifactError> {
    if artifact.mutations.is_empty() {
        return Err(ArtifactError::new("artifact declares no mutation policies"));
    }
    artifact.validate_mutations()?;

    let prepared = prepare_artifact(artifact)?;
    let (baseline_status, baseline_expectations) =
        evaluate_prepared_expectations(artifact, &prepared)?;
    let baseline_failures: Vec<_> = baseline_expectations
        .into_iter()
        .filter(|expectation| !expectation.satisfied)
        .collect();
    if !baseline_failures.is_empty() {
        return Ok(MutationReport {
            spec: artifact.name.clone(),
            status: "baseline_incomplete".to_string(),
            baseline_status,
            baseline_failures,
            summary: MutationSummary {
                total: 0,
                executed: 0,
                killed: 0,
                survived: 0,
                rejected: 0,
                excluded: 0,
            },
            policies: Vec::new(),
            mutations: Vec::new(),
        });
    }

    let mut mutants = Vec::new();
    for policy in &artifact.mutations {
        generate_policy_mutants(artifact, policy, &mut mutants);
    }
    let queue: Vec<_> = mutants.iter().filter(|mutant| !mutant.excluded).collect();
    let mut evaluated = evaluate_in_parallel(artifact, &prepared, &queue).into_iter();
    let results = mutants
        .into_iter()
        .map(|mutant| {
            if mutant.excluded {
                Ok(excluded_result(mutant.id, mutant.policy, mutant.target))
            } else {
                evaluated
                    .next()
                    .expect("every queued mutant has an outcome")
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    let summary = summarize(results.iter());
    let policies: Vec<_> = artifact
        .mutations
        .iter()
        .map(|policy| summarize_policy(policy, &results))
        .collect();
    let status = if summary.survived > 0 {
        "survived"
    } else if policies.iter().any(|policy| policy.status == "vacuous") {
        "vacuous"
    } else {
        "clean"
    };

    Ok(MutationReport {
        spec: artifact.name.clone(),
        status: status.to_string(),
        baseline_status,
        baseline_failures,
        summary,
        policies,
        mutations: results,
    })
}

/// One mutant in report order. The target alone determines the mutated artifact.
struct Mutant<'a> {
    id: String,
    policy: &'a MutationDecl,
    target: MutationTarget,
    excluded: bool,
}

impl<'a> Mutant<'a> {
    fn new(policy: &'a MutationDecl, target: MutationTarget, excluded: bool) -> Self {
        Self {
            id: mutation_id(policy, &target),
            policy,
            target,
            excluded,
        }
    }
}

fn generate_policy_mutants<'a>(
    artifact: &Artifact,
    policy: &'a MutationDecl,
    mutants: &mut Vec<Mutant<'a>>,
) {
    match policy.operator {
        MutationOperator::DropRule => {
            for rule in &artifact.rules {
                let target = MutationTarget::Rule {
                    rule: rule.id.clone(),
                };
                mutants.push(Mutant::new(
                    policy,
                    target,
                    policy.except.contains(&rule.id),
                ));
            }
        }
        MutationOperator::DropCondition => {
            for rule in &artifact.rules {
                for (index, expression) in rule.when.iter().enumerate() {
                    let condition = rule.condition_id(index).map(str::to_string);
                    let excluded = policy.excludes_condition(&rule.id, condition.as_deref());
                    let target = MutationTarget::Condition {
                        rule: rule.id.clone(),
                        condition,
                        index: index + 1,
                        expression: expression.clone(),
                    };
                    mutants.push(Mutant::new(policy, target, excluded));
                }
            }
        }
        MutationOperator::DropFact => {
            let relation = policy
                .relation
                .as_deref()
                .expect("validated drop_fact policy has a relation");
            for fact in artifact
                .facts
                .iter()
                .filter(|fact| fact.relation == relation)
            {
                let target = MutationTarget::Fact {
                    fact: fact.id.clone(),
                    relation: fact.relation.clone(),
                };
                mutants.push(Mutant::new(
                    policy,
                    target,
                    policy.except.contains(&fact.id),
                ));
            }
        }
    }
}

/// Evaluates the queued mutants on every available core and returns their
/// outcomes in queue order, so the report never depends on scheduling.
fn evaluate_in_parallel(
    artifact: &Artifact,
    prepared: &PreparedArtifact,
    queue: &[&Mutant],
) -> Vec<Result<MutationResult, ArtifactError>> {
    let workers = thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(queue.len());
    let next = AtomicUsize::new(0);
    let mut outcomes: Vec<_> = thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut outcomes = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(mutant) = queue.get(index) else {
                            return outcomes;
                        };
                        let outcome = evaluate_mutation(
                            mutant.id.clone(),
                            mutant.policy,
                            mutant.target.clone(),
                            artifact,
                            prepared,
                        );
                        outcomes.push((index, outcome));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|payload| panic::resume_unwind(payload))
            })
            .collect()
    });
    outcomes.sort_unstable_by_key(|&(index, _)| index);
    outcomes.into_iter().map(|(_, outcome)| outcome).collect()
}

fn evaluate_mutation(
    id: String,
    policy: &MutationDecl,
    target: MutationTarget,
    artifact: &Artifact,
    prepared: &PreparedArtifact,
) -> Result<MutationResult, ArtifactError> {
    match evaluate_prepared_mutant(artifact, prepared, &target) {
        Ok(expectations) => {
            let failed_expectations: Vec<_> = expectations
                .into_iter()
                .filter(|expectation| !expectation.satisfied)
                .collect();
            let killed = match policy.must_fail.as_deref() {
                Some(required) => failed_expectations
                    .iter()
                    .any(|expectation| expectation.id == required),
                None => !failed_expectations.is_empty(),
            };
            Ok(MutationResult {
                id,
                policy: policy.id.clone(),
                operator: policy.operator,
                target,
                status: if killed {
                    MutationStatus::Killed
                } else {
                    MutationStatus::Survived
                },
                failed_expectations,
                diagnostic: None,
            })
        }
        Err(error) => mutation_error_result(id, policy, target, error),
    }
}

fn mutation_error_result(
    id: String,
    policy: &MutationDecl,
    target: MutationTarget,
    error: ArtifactError,
) -> Result<MutationResult, ArtifactError> {
    if !error.is_invalid_artifact() {
        return Err(error);
    }
    Ok(MutationResult {
        id,
        policy: policy.id.clone(),
        operator: policy.operator,
        target,
        status: MutationStatus::Rejected,
        failed_expectations: Vec::new(),
        diagnostic: Some(error.to_string()),
    })
}

fn excluded_result(id: String, policy: &MutationDecl, target: MutationTarget) -> MutationResult {
    MutationResult {
        id,
        policy: policy.id.clone(),
        operator: policy.operator,
        target,
        status: MutationStatus::Excluded,
        failed_expectations: Vec::new(),
        diagnostic: None,
    }
}

fn mutation_id(policy: &MutationDecl, target: &MutationTarget) -> String {
    match target {
        MutationTarget::Rule { rule } => {
            format!("{}:{}:{rule}", policy.id, policy.operator.as_str())
        }
        MutationTarget::Condition {
            rule,
            condition,
            index,
            ..
        } => {
            let condition = condition
                .as_deref()
                .map_or_else(|| index.to_string(), str::to_string);
            format!(
                "{}:{}:{rule}:{condition}",
                policy.id,
                policy.operator.as_str()
            )
        }
        MutationTarget::Fact { fact, .. } => {
            format!("{}:{}:{fact}", policy.id, policy.operator.as_str())
        }
    }
}

fn summarize<'a>(results: impl Iterator<Item = &'a MutationResult>) -> MutationSummary {
    let mut summary = MutationSummary {
        total: 0,
        executed: 0,
        killed: 0,
        survived: 0,
        rejected: 0,
        excluded: 0,
    };
    for result in results {
        summary.total += 1;
        match result.status {
            MutationStatus::Killed => {
                summary.executed += 1;
                summary.killed += 1;
            }
            MutationStatus::Survived => {
                summary.executed += 1;
                summary.survived += 1;
            }
            MutationStatus::Rejected => summary.rejected += 1,
            MutationStatus::Excluded => summary.excluded += 1,
        }
    }
    summary
}

fn summarize_policy(policy: &MutationDecl, results: &[MutationResult]) -> MutationPolicyReport {
    let summary = summarize(results.iter().filter(|result| result.policy == policy.id));
    let status = if summary.survived > 0 {
        "survived"
    } else if summary.executed == 0 {
        "vacuous"
    } else {
        "clean"
    }
    .to_string();
    MutationPolicyReport {
        id: policy.id.clone(),
        operator: policy.operator,
        status,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluator_errors_abort_instead_of_becoming_rejections() {
        let policy = MutationDecl {
            id: "coverage".to_string(),
            operator: MutationOperator::DropRule,
            relation: None,
            except: Vec::new(),
            must_fail: None,
            doc: None,
        };
        let target = MutationTarget::Rule {
            rule: "derive".to_string(),
        };

        let error = mutation_error_result(
            "coverage:drop_rule:derive".to_string(),
            &policy,
            target,
            ArtifactError::evaluation("engine failure"),
        )
        .expect_err("evaluation failure must abort mutation analysis");

        assert_eq!(error.to_string(), "engine failure");
    }
}
