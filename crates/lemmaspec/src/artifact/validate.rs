use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{Atom, Clause, CmpOp, Expr, Lit};
use crate::eval::Support;
use crate::mutation::MutationTarget;
use crate::{parse_program, Ann, Engine, Term, Value};

use super::{
    parse_artifact, Artifact, ArtifactError, ExpectationDecl, FactDecl, FactValue, MutationDecl,
    MutationOperator, RuleDecl, ValueType, WalkExpectation, WalkFact, WalkReport,
};

impl Artifact {
    pub(crate) fn referenced_symbols(&self) -> Result<BTreeSet<String>, ArtifactError> {
        fn collect(term: &Term, symbols: &mut BTreeSet<String>) {
            match term {
                Term::Sym(value) => {
                    symbols.insert(value.clone());
                }
                Term::Agg(_, inner) => collect(inner, symbols),
                _ => {}
            }
        }

        let mut symbols = BTreeSet::new();
        for fact in &self.facts {
            for arg in &fact.args {
                if let FactValue::Symbol(value) = arg {
                    symbols.insert(value.clone());
                }
            }
        }
        let sources = self
            .rules
            .iter()
            .map(|rule| format!("{}: {} :- {}.", rule.id, rule.derive, rule.when.join(", ")))
            .chain(
                self.expectations
                    .iter()
                    .map(|expectation| format!("{}.", expectation.query)),
            );
        for source in sources {
            let clauses =
                parse_program(&source).map_err(|error| ArtifactError::new(error.to_string()))?;
            for clause in clauses {
                for arg in &clause.head.args {
                    collect(arg, &mut symbols);
                }
                for literal in &clause.body {
                    match literal {
                        Lit::Pos(atom) | Lit::Neg(atom) => {
                            for arg in &atom.args {
                                collect(arg, &mut symbols);
                            }
                        }
                        Lit::Cmp(_, term, expression) => {
                            collect(term, &mut symbols);
                            crate::check_expression(expression, &mut |term| {
                                collect(term, &mut symbols)
                            });
                        }
                        Lit::Now(term) => collect(term, &mut symbols),
                    }
                }
            }
        }
        Ok(symbols)
    }

    pub(crate) fn validate_mutations(&self) -> Result<(), ArtifactError> {
        let relations: BTreeSet<_> = self
            .relations
            .iter()
            .map(|relation| relation.name.as_str())
            .collect();
        let rules: BTreeSet<_> = self.rules.iter().map(|rule| rule.id.as_str()).collect();
        let facts: BTreeSet<_> = self.facts.iter().map(|fact| fact.id.as_str()).collect();
        let expectations: BTreeSet<_> = self
            .expectations
            .iter()
            .map(|expectation| expectation.id.as_str())
            .collect();
        let mut policy_signatures = BTreeMap::new();

        for mutation in &self.mutations {
            match mutation.operator {
                MutationOperator::DropRule | MutationOperator::DropCondition => {
                    if mutation.relation.is_some() {
                        return Err(ArtifactError::new(format!(
                            "mutation `{}` operator `{}` does not accept `relation`",
                            mutation.id,
                            mutation.operator.as_str()
                        )));
                    }
                    if self.rules.is_empty() {
                        return Err(ArtifactError::new(format!(
                            "mutation `{}` has no rules to target",
                            mutation.id
                        )));
                    }
                    if mutation.operator == MutationOperator::DropCondition {
                        validate_condition_exceptions(mutation, &rules, &self.rules)?;
                    } else {
                        validate_exceptions(mutation, &rules, "rule")?;
                    }
                }
                MutationOperator::DropFact => {
                    let relation = mutation.relation.as_deref().ok_or_else(|| {
                        ArtifactError::new(format!(
                            "mutation `{}` operator `drop_fact` requires `relation`",
                            mutation.id
                        ))
                    })?;
                    if !relations.contains(relation) {
                        return Err(ArtifactError::new(format!(
                            "mutation `{}` references unknown relation `{relation}`",
                            mutation.id
                        )));
                    }
                    let matching_facts: BTreeSet<_> = self
                        .facts
                        .iter()
                        .filter(|fact| fact.relation == relation)
                        .map(|fact| fact.id.as_str())
                        .collect();
                    if matching_facts.is_empty() {
                        return Err(ArtifactError::new(format!(
                            "mutation `{}` has no facts of relation `{relation}` to target",
                            mutation.id
                        )));
                    }
                    validate_exceptions(mutation, &facts, "fact")?;
                    if let Some(except) = mutation
                        .except
                        .iter()
                        .find(|except| !matching_facts.contains(except.as_str()))
                    {
                        return Err(ArtifactError::new(format!(
                            "mutation `{}` excludes fact `{except}`, which is not of relation `{relation}`",
                            mutation.id
                        )));
                    }
                }
            }

            if let Some(expectation) = mutation.must_fail.as_deref() {
                if !expectations.contains(expectation) {
                    return Err(ArtifactError::new(format!(
                        "mutation `{}` requires unknown expectation `{expectation}` to fail",
                        mutation.id
                    )));
                }
            }

            let signature = (
                mutation.operator.as_str(),
                mutation.relation.as_deref(),
                self.effective_mutation_targets(mutation),
                mutation.must_fail.as_deref(),
            );
            if let Some(existing) = policy_signatures.insert(signature, mutation.id.as_str()) {
                return Err(ArtifactError::new(format!(
                    "mutations `{existing}` and `{}` declare identical policies",
                    mutation.id
                )));
            }
        }
        Ok(())
    }

    fn effective_mutation_targets(&self, mutation: &MutationDecl) -> Vec<String> {
        match mutation.operator {
            MutationOperator::DropRule => self
                .rules
                .iter()
                .filter(|rule| !mutation.except.contains(&rule.id))
                .map(|rule| rule.id.clone())
                .collect(),
            MutationOperator::DropCondition => self
                .rules
                .iter()
                .flat_map(|rule| {
                    rule.when.iter().enumerate().filter_map(move |(index, _)| {
                        let condition = rule.condition_id(index);
                        if mutation.excludes_condition(&rule.id, condition) {
                            return None;
                        }
                        let condition =
                            condition.map_or_else(|| format!("#{}", index + 1), str::to_string);
                        Some(format!("{}.{condition}", rule.id))
                    })
                })
                .collect(),
            MutationOperator::DropFact => self
                .facts
                .iter()
                .filter(|fact| {
                    mutation.relation.as_deref() == Some(fact.relation.as_str())
                        && !mutation.except.contains(&fact.id)
                })
                .map(|fact| fact.id.clone())
                .collect(),
        }
    }
}

fn validate_exceptions(
    mutation: &MutationDecl,
    targets: &BTreeSet<&str>,
    target_kind: &str,
) -> Result<(), ArtifactError> {
    validate_unique_exceptions(mutation)?;
    for except in &mutation.except {
        if !targets.contains(except.as_str()) {
            return Err(ArtifactError::new(format!(
                "mutation `{}` excludes unknown {target_kind} `{except}`",
                mutation.id
            )));
        }
    }
    Ok(())
}

fn validate_condition_exceptions(
    mutation: &MutationDecl,
    rules: &BTreeSet<&str>,
    rule_declarations: &[RuleDecl],
) -> Result<(), ArtifactError> {
    validate_unique_exceptions(mutation)?;
    for except in &mutation.except {
        if !rules.contains(except.as_str()) && !named_condition_exists(rule_declarations, except) {
            return Err(ArtifactError::new(format!(
                "mutation `{}` excludes unknown rule or named condition `{except}`",
                mutation.id
            )));
        }
    }
    Ok(())
}

fn validate_unique_exceptions(mutation: &MutationDecl) -> Result<(), ArtifactError> {
    let mut seen = BTreeSet::new();
    for except in &mutation.except {
        if !seen.insert(except) {
            return Err(ArtifactError::new(format!(
                "mutation `{}` excludes `{except}` more than once",
                mutation.id
            )));
        }
    }
    Ok(())
}

fn named_condition_exists(rules: &[RuleDecl], reference: &str) -> bool {
    let Some((rule_id, condition_id)) = reference.split_once('.') else {
        return false;
    };
    rules.iter().any(|rule| {
        rule.id == rule_id
            && rule
                .condition_ids
                .iter()
                .any(|candidate| candidate == condition_id)
    })
}

#[derive(Debug, Clone)]
pub(crate) struct CompiledRule {
    pub(crate) id: String,
    pub(crate) head_relation: String,
    pub(crate) source: String,
    pub(crate) clause: Clause,
}

#[derive(Debug, Clone)]
pub(crate) struct CompiledExpectation {
    pub(crate) id: String,
    pub(crate) query: String,
    pub(crate) count: usize,
    pub(crate) atom: Atom,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedArtifact {
    pub(crate) schemas: BTreeMap<String, Vec<ValueType>>,
    pub(crate) rules: Vec<CompiledRule>,
    pub(crate) expectations: Vec<CompiledExpectation>,
}

pub(crate) struct ProjectionEvaluation {
    pub(crate) artifact: Artifact,
    pub(crate) engine: Engine,
    pub(crate) status: String,
    pub(crate) expectations: Vec<WalkExpectation>,
    pub(crate) compiled_rules: Vec<CompiledRule>,
    pub(crate) compiled_expectations: Vec<CompiledExpectation>,
}

pub(crate) fn prepare_artifact(artifact: &Artifact) -> Result<PreparedArtifact, ArtifactError> {
    let schemas: BTreeMap<String, Vec<ValueType>> = artifact
        .relations
        .iter()
        .map(|relation| (relation.name.clone(), relation.args.clone()))
        .collect();

    validate_facts(&artifact.facts, &schemas)?;
    if let Some(rule) = artifact.rules.iter().find(|rule| rule.when.is_empty()) {
        return Err(ArtifactError::new(format!(
            "rule `{}` must contain at least one condition",
            rule.id
        )));
    }
    let rules = artifact
        .rules
        .iter()
        .map(|rule| compile_rule(rule, &schemas))
        .collect::<Result<Vec<_>, _>>()?;
    let derived_relations: BTreeMap<_, _> = rules
        .iter()
        .map(|rule| (rule.head_relation.as_str(), rule.id.as_str()))
        .collect();
    for fact in &artifact.facts {
        if let Some(rule_id) = derived_relations.get(fact.relation.as_str()) {
            return Err(ArtifactError::new(format!(
                "fact `{}` asserts relation `{}`, which is derived by rule `{rule_id}`",
                fact.id, fact.relation
            )));
        }
    }
    let expectations = artifact
        .expectations
        .iter()
        .map(|expectation| compile_expectation(expectation, &schemas))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(PreparedArtifact {
        schemas,
        rules,
        expectations,
    })
}

fn run_prepared_engine<'a>(
    rules: impl IntoIterator<Item = &'a CompiledRule>,
    facts: impl IntoIterator<Item = &'a FactDecl>,
    expectations: &[CompiledExpectation],
) -> Result<(Engine, Vec<WalkExpectation>), ArtifactError> {
    let mut engine = Engine::new_batch();
    for rule in rules {
        engine
            .install_clauses(vec![rule.clause.clone()], rule.source.clone())
            .map_err(|error| ArtifactError::new(format!("install rule: {error}")))?;
    }

    for fact in facts {
        let args = intern_fact_args(&mut engine, &fact.args);
        let mut provenance = fact.provenance.clone();
        provenance.push(fact.id.clone());
        engine.declare(
            &fact.relation,
            &args,
            Ann::base(fact.confidence, provenance),
        );
    }
    engine.run();

    let mut walked_expectations = Vec::with_capacity(expectations.len());
    for expectation in expectations {
        let mut found = engine.ask_atom(&expectation.atom);
        found.sort();
        let actual_count = found.len();
        walked_expectations.push(WalkExpectation {
            id: expectation.id.clone(),
            query: expectation.query.clone(),
            expected_count: expectation.count,
            actual_count,
            satisfied: actual_count == expectation.count,
            found,
        });
    }

    Ok((engine, walked_expectations))
}

pub fn walk_artifact(source: &str) -> Result<WalkReport, ArtifactError> {
    let artifact = parse_artifact(source)?;
    walk_parsed_artifact(&artifact)
}

pub fn walk_parsed_artifact(artifact: &Artifact) -> Result<WalkReport, ArtifactError> {
    let prepared = prepare_artifact(artifact)?;
    let (engine, expectations) =
        run_prepared_engine(&prepared.rules, &artifact.facts, &prepared.expectations)?;

    let mut facts = Vec::new();
    for relation in &artifact.relations {
        let Some(stored) = engine.relations.get(&relation.name) else {
            continue;
        };
        for row in &stored.rows {
            let origin = if row
                .fact
                .supports
                .iter()
                .any(|support| matches!(support, Support::Base))
            {
                "asserted"
            } else {
                "derived"
            };
            facts.push(WalkFact {
                relation: relation.name.clone(),
                args: row
                    .key
                    .iter()
                    .map(|value| engine.interner.display(value))
                    .collect(),
                origin: origin.to_string(),
                confidence: row.fact.ann.conf,
                provenance: row.fact.ann.prov.iter().cloned().collect(),
                why: engine.why(&relation.name, &row.key),
            });
        }
    }
    facts.sort_by(|left, right| {
        (&left.relation, &left.args, &left.origin).cmp(&(
            &right.relation,
            &right.args,
            &right.origin,
        ))
    });

    let asserted_count = facts
        .iter()
        .filter(|fact| fact.origin == "asserted")
        .count();
    let derived_count = facts.len() - asserted_count;
    let status = if expectations.iter().all(|expectation| expectation.satisfied) {
        "clean"
    } else {
        "incomplete"
    };

    Ok(WalkReport {
        spec: artifact.name.clone(),
        status: status.to_string(),
        asserted: asserted_count,
        derived: derived_count,
        facts,
        expectations,
    })
}

pub(crate) fn evaluate_parsed_for_projection(
    artifact: Artifact,
) -> Result<ProjectionEvaluation, ArtifactError> {
    let prepared = prepare_artifact(&artifact)?;
    let (engine, expectations) =
        run_prepared_engine(&prepared.rules, &artifact.facts, &prepared.expectations)?;
    let status = if expectations.iter().all(|expectation| expectation.satisfied) {
        "clean"
    } else {
        "incomplete"
    };
    Ok(ProjectionEvaluation {
        artifact,
        engine,
        status: status.to_string(),
        expectations,
        compiled_rules: prepared.rules,
        compiled_expectations: prepared.expectations,
    })
}

pub(crate) fn evaluate_prepared_expectations(
    artifact: &Artifact,
    prepared: &PreparedArtifact,
) -> Result<(String, Vec<WalkExpectation>), ArtifactError> {
    let (_, expectations) =
        run_prepared_engine(&prepared.rules, &artifact.facts, &prepared.expectations)?;
    let status = if expectations.iter().all(|expectation| expectation.satisfied) {
        "clean"
    } else {
        "incomplete"
    };
    Ok((status.to_string(), expectations))
}

pub(crate) fn evaluate_prepared_mutant(
    artifact: &Artifact,
    prepared: &PreparedArtifact,
    target: &MutationTarget,
) -> Result<Vec<WalkExpectation>, ArtifactError> {
    match target {
        MutationTarget::Rule { rule } => {
            let rules = prepared
                .rules
                .iter()
                .filter(|candidate| &candidate.id != rule);
            let (_, expectations) =
                run_prepared_engine(rules, &artifact.facts, &prepared.expectations)?;
            Ok(expectations)
        }
        MutationTarget::Fact { fact, .. } => {
            let facts = artifact
                .facts
                .iter()
                .filter(|candidate| &candidate.id != fact);
            let (_, expectations) =
                run_prepared_engine(&prepared.rules, facts, &prepared.expectations)?;
            Ok(expectations)
        }
        MutationTarget::Condition { rule, index, .. } => {
            let mut mutated_rule = artifact
                .rules
                .iter()
                .find(|candidate| &candidate.id == rule)
                .expect("artifact retains selected rule")
                .clone();
            mutated_rule.remove_condition(index - 1);
            if mutated_rule.when.is_empty() {
                return Err(ArtifactError::new(format!(
                    "rule `{}` must contain at least one condition",
                    mutated_rule.id
                )));
            }
            let compiled_mutated = compile_rule(&mutated_rule, &prepared.schemas)?;
            let rules: Vec<&CompiledRule> = prepared
                .rules
                .iter()
                .map(|candidate| {
                    if &candidate.id == rule {
                        &compiled_mutated
                    } else {
                        candidate
                    }
                })
                .collect();
            let (_, expectations) =
                run_prepared_engine(rules, &artifact.facts, &prepared.expectations)?;
            Ok(expectations)
        }
    }
}

fn validate_facts(
    facts: &[FactDecl],
    schemas: &BTreeMap<String, Vec<ValueType>>,
) -> Result<(), ArtifactError> {
    for fact in facts {
        let schema = schemas.get(&fact.relation).ok_or_else(|| {
            ArtifactError::new(format!(
                "fact `{}` references unknown relation `{}`",
                fact.id, fact.relation
            ))
        })?;
        if schema.len() != fact.args.len() {
            return Err(ArtifactError::new(format!(
                "relation `{}` expects {} arguments, got {} in fact `{}`",
                fact.relation,
                schema.len(),
                fact.args.len(),
                fact.id
            )));
        }
        for (position, (expected, actual)) in schema.iter().zip(&fact.args).enumerate() {
            let matches = matches!(
                (expected, actual),
                (ValueType::Symbol, FactValue::Symbol(_))
                    | (ValueType::Integer, FactValue::Integer(_))
            );
            if !matches {
                return Err(ArtifactError::new(format!(
                    "fact `{}` argument {} for relation `{}` must be {}",
                    fact.id,
                    position + 1,
                    fact.relation,
                    value_type_name(expected)
                )));
            }
        }
    }
    Ok(())
}

fn compile_rule(
    rule: &RuleDecl,
    schemas: &BTreeMap<String, Vec<ValueType>>,
) -> Result<CompiledRule, ArtifactError> {
    let source = format!("{}: {} :- {}.", rule.id, rule.derive, rule.when.join(", "));
    let mut clauses = parse_program(&source)
        .map_err(|error| ArtifactError::new(format!("rule `{}`: {error}", rule.id)))?;
    if clauses.len() != 1 || clauses[0].is_fact {
        return Err(ArtifactError::new(format!(
            "rule `{}` must compile to exactly one rule",
            rule.id
        )));
    }
    let clause = clauses.pop().expect("checked single clause");
    validate_clause(&rule.id, &clause, schemas)?;
    Ok(CompiledRule {
        id: rule.id.clone(),
        head_relation: clause.head.pred.clone(),
        source,
        clause,
    })
}

fn compile_expectation(
    expectation: &ExpectationDecl,
    schemas: &BTreeMap<String, Vec<ValueType>>,
) -> Result<CompiledExpectation, ArtifactError> {
    let mut clauses = parse_program(&format!("{}.", expectation.query))
        .map_err(|error| ArtifactError::new(format!("expect `{}`: {error}", expectation.id)))?;
    if clauses.len() != 1 || !clauses[0].is_fact {
        return Err(ArtifactError::new(format!(
            "expect `{}` query must be one atom",
            expectation.id
        )));
    }
    let clause = clauses.pop().expect("checked single fact clause");
    let mut variables = BTreeMap::new();
    validate_atom(
        &format!("expect `{}`", expectation.id),
        &clause.head,
        schemas,
        &mut variables,
    )?;
    Ok(CompiledExpectation {
        id: expectation.id.clone(),
        query: expectation.query.clone(),
        count: expectation.count,
        atom: clause.head,
    })
}

fn validate_clause(
    rule_id: &str,
    clause: &Clause,
    schemas: &BTreeMap<String, Vec<ValueType>>,
) -> Result<(), ArtifactError> {
    let owner = format!("rule `{rule_id}`");
    let mut variables = BTreeMap::new();
    for literal in &clause.body {
        match literal {
            Lit::Pos(atom) | Lit::Neg(atom) => {
                validate_atom(&owner, atom, schemas, &mut variables)?;
            }
            Lit::Now(_) => {
                return Err(ArtifactError::new(format!(
                    "{owner} uses `now/1`, which is unavailable without a deterministic artifact clock"
                )));
            }
            Lit::Cmp(..) => {}
        }
    }
    validate_atom(&owner, &clause.head, schemas, &mut variables)?;
    for literal in &clause.body {
        if let Lit::Cmp(operator, left, right) = literal {
            validate_comparison(&owner, operator, left, right, &mut variables)?;
        }
    }
    Ok(())
}

fn validate_comparison(
    owner: &str,
    operator: &CmpOp,
    left: &Term,
    right: &Expr,
    variables: &mut BTreeMap<String, ValueType>,
) -> Result<(), ArtifactError> {
    match operator {
        CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge => {
            let message = "ordering comparison requires integer operands";
            validate_term_type(owner, left, &ValueType::Integer, variables)
                .map_err(|_| ArtifactError::new(format!("{owner} {message}")))?;
            validate_integer_expression(owner, right, variables, message)
        }
        CmpOp::Eq | CmpOp::Ne => match right {
            Expr::T(right) => validate_equality_terms(owner, left, right, variables),
            Expr::Add(..) | Expr::Sub(..) => {
                let message = "arithmetic expression requires integer operands";
                validate_term_type(owner, left, &ValueType::Integer, variables)
                    .map_err(|_| ArtifactError::new(format!("{owner} {message}")))?;
                validate_integer_expression(owner, right, variables, message)
            }
        },
    }
}

fn validate_integer_expression(
    owner: &str,
    expression: &Expr,
    variables: &mut BTreeMap<String, ValueType>,
    message: &str,
) -> Result<(), ArtifactError> {
    match expression {
        Expr::T(term) => validate_term_type(owner, term, &ValueType::Integer, variables)
            .map_err(|_| ArtifactError::new(format!("{owner} {message}"))),
        Expr::Add(left, right) | Expr::Sub(left, right) => {
            validate_integer_expression(owner, left, variables, message)?;
            validate_integer_expression(owner, right, variables, message)
        }
    }
}

fn validate_equality_terms(
    owner: &str,
    left: &Term,
    right: &Term,
    variables: &mut BTreeMap<String, ValueType>,
) -> Result<(), ArtifactError> {
    match (
        known_term_type(left, variables),
        known_term_type(right, variables),
    ) {
        (Some(left_type), Some(right_type)) if left_type != right_type => {
            Err(ArtifactError::new(format!(
                "{owner} equality comparison has incompatible {} and {} operands",
                value_type_name(&left_type),
                value_type_name(&right_type)
            )))
        }
        (Some(expected), None) => validate_term_type(owner, right, &expected, variables),
        (None, Some(expected)) => validate_term_type(owner, left, &expected, variables),
        _ => Ok(()),
    }
}

fn known_term_type(term: &Term, variables: &BTreeMap<String, ValueType>) -> Option<ValueType> {
    match term {
        Term::Var(variable) => variables.get(variable).cloned(),
        Term::Sym(_) => Some(ValueType::Symbol),
        Term::Int(_) | Term::Agg(..) => Some(ValueType::Integer),
        Term::Wildcard => None,
    }
}

fn validate_atom(
    owner: &str,
    atom: &Atom,
    schemas: &BTreeMap<String, Vec<ValueType>>,
    variables: &mut BTreeMap<String, ValueType>,
) -> Result<(), ArtifactError> {
    let schema = schemas.get(&atom.pred).ok_or_else(|| {
        ArtifactError::new(format!(
            "{owner} references unknown relation `{}`",
            atom.pred
        ))
    })?;
    if schema.len() != atom.args.len() {
        return Err(ArtifactError::new(format!(
            "relation `{}` expects {} arguments, got {} in {owner}",
            atom.pred,
            schema.len(),
            atom.args.len()
        )));
    }
    for (term, expected) in atom.args.iter().zip(schema) {
        validate_term_type(owner, term, expected, variables)?;
    }
    Ok(())
}

fn validate_term_type(
    owner: &str,
    term: &Term,
    expected: &ValueType,
    variables: &mut BTreeMap<String, ValueType>,
) -> Result<(), ArtifactError> {
    match term {
        Term::Var(variable) => match variables.get(variable) {
            Some(actual) if actual != expected => Err(ArtifactError::new(format!(
                "{owner} variable `{variable}` has incompatible types {} and {}",
                value_type_name(actual),
                value_type_name(expected)
            ))),
            Some(_) => Ok(()),
            None => {
                variables.insert(variable.clone(), expected.clone());
                Ok(())
            }
        },
        Term::Sym(_) if *expected == ValueType::Symbol => Ok(()),
        Term::Int(_) if *expected == ValueType::Integer => Ok(()),
        Term::Wildcard => Ok(()),
        Term::Agg(function, inner) if *expected == ValueType::Integer => match function {
            crate::intern::AggFn::Count => Ok(()),
            crate::intern::AggFn::Min | crate::intern::AggFn::Max => {
                validate_term_type(owner, inner, &ValueType::Integer, variables).map_err(|_| {
                    ArtifactError::new(format!(
                        "{owner} aggregate `{}` requires integer input",
                        function.name()
                    ))
                })
            }
            crate::intern::AggFn::Sum => Err(ArtifactError::new(format!(
                "{owner} aggregate `sum` is unavailable until checked integer evaluation is implemented"
            ))),
        },
        _ => Err(ArtifactError::new(format!(
            "{owner} uses a {} value where {} is required",
            term_type_name(term),
            value_type_name(expected)
        ))),
    }
}

fn value_type_name(value_type: &ValueType) -> &'static str {
    match value_type {
        ValueType::Symbol => "symbol",
        ValueType::Integer => "integer",
    }
}

fn term_type_name(term: &Term) -> &'static str {
    match term {
        Term::Sym(_) => "symbol",
        Term::Int(_) | Term::Agg(_, _) => "integer",
        Term::Var(_) | Term::Wildcard => "unknown",
    }
}

fn intern_fact_args(engine: &mut Engine, args: &[FactValue]) -> Vec<Value> {
    args.iter()
        .map(|argument| match argument {
            FactValue::Symbol(value) => engine.sym(value),
            FactValue::Integer(value) => Value::Int(*value),
        })
        .collect()
}
