//! Deterministic projection of one evaluated artifact into graph entries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::artifact::{
    evaluate_parsed_for_projection, parse_artifact, Artifact, ArtifactError, EvidenceBasis,
    EvidenceKind, FactValue, MutationOperator, ValueType,
};
use crate::ast::{Atom, Expr, Lit};
use crate::eval::Support;
use crate::narrative::read_fact;
use crate::{Interner, Term, Value};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphProjection {
    pub spec: String,
    pub status: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphNode {
    pub kind: String,
    pub id: String,
    pub spec: String,
    #[serde(flatten)]
    pub data: GraphNodeData,
}

impl GraphNode {
    pub fn node_type(&self) -> &'static str {
        match self.data {
            GraphNodeData::Spec { .. } => "spec",
            GraphNodeData::Relation { .. } => "relation",
            GraphNodeData::Fact { .. } => "fact",
            GraphNodeData::Rule { .. } => "rule",
            GraphNodeData::Mutation { .. } => "mutation",
            GraphNodeData::Expectation { .. } => "expectation",
            GraphNodeData::Symbol { .. } => "symbol",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GraphNodeData {
    Spec {
        name: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        doc: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
    },
    Relation {
        name: String,
        args: Vec<ValueType>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        roles: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reads: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        doc: Option<String>,
    },
    Fact {
        relation: String,
        args: Vec<FactValue>,
        origin: String,
        confidence: f64,
        provenance: Vec<String>,
        declarations: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        bases: Vec<EvidenceBasis>,
        /// The fact read through its relation's `reads` template.
        #[serde(skip_serializing_if = "Option::is_none")]
        reading: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        doc: Option<String>,
    },
    Rule {
        name: String,
        derive: String,
        when: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        condition_ids: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        doc: Option<String>,
    },
    Mutation {
        name: String,
        operator: MutationOperator,
        relation: Option<String>,
        except: Vec<String>,
        must_fail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        doc: Option<String>,
    },
    Expectation {
        name: String,
        query: String,
        expected_count: usize,
        actual_count: usize,
        satisfied: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        doc: Option<String>,
    },
    Symbol {
        value: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        doc: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphEdge {
    pub kind: String,
    pub id: String,
    pub spec: String,
    pub from: String,
    pub to: String,
    pub rel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub witness: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionError {
    message: String,
}

impl ProjectionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProjectionError {}

impl GraphProjection {
    /// Enforce the local projection boundary: every record belongs to this
    /// spec, every id is unique and namespaced, and every edge resolves to two
    /// nodes emitted by this projection.
    pub fn validate_closed(&self) -> Result<(), ProjectionError> {
        let prefix = format!("lemmaspec:{}:", self.spec);
        let mut node_ids = BTreeSet::new();
        let mut node_types = BTreeMap::new();
        for node in &self.nodes {
            if node.kind != "node" {
                return Err(ProjectionError::new(format!(
                    "graph node `{}` has kind `{}`",
                    node.id, node.kind
                )));
            }
            if node.spec != self.spec || !node.id.starts_with(&prefix) {
                return Err(ProjectionError::new(format!(
                    "graph node `{}` escapes spec `{}`",
                    node.id, self.spec
                )));
            }
            if !node_ids.insert(node.id.as_str()) {
                return Err(ProjectionError::new(format!(
                    "duplicate graph node id `{}`",
                    node.id
                )));
            }
            node_types.insert(node.id.as_str(), node.node_type());
        }

        let mut edge_ids = BTreeSet::new();
        for edge in &self.edges {
            if edge.kind != "edge" {
                return Err(ProjectionError::new(format!(
                    "graph edge `{}` has kind `{}`",
                    edge.id, edge.kind
                )));
            }
            if edge.spec != self.spec || !edge.id.starts_with(&prefix) {
                return Err(ProjectionError::new(format!(
                    "graph edge `{}` escapes spec `{}`",
                    edge.id, self.spec
                )));
            }
            if !edge_ids.insert(edge.id.as_str()) {
                return Err(ProjectionError::new(format!(
                    "duplicate graph edge id `{}`",
                    edge.id
                )));
            }
            if !node_ids.contains(edge.from.as_str()) {
                return Err(ProjectionError::new(format!(
                    "graph edge `{}` references missing source node `{}`",
                    edge.id, edge.from
                )));
            }
            if !node_ids.contains(edge.to.as_str()) {
                return Err(ProjectionError::new(format!(
                    "graph edge `{}` references missing target node `{}`",
                    edge.id, edge.to
                )));
            }
            let from_type = node_types[edge.from.as_str()];
            let to_type = node_types[edge.to.as_str()];
            if !valid_edge_signature(&edge.rel, from_type, to_type) {
                return Err(ProjectionError::new(format!(
                    "invalid `{}` edge from {from_type} to {to_type}",
                    edge.rel
                )));
            }
        }
        Ok(())
    }
}

fn valid_edge_signature(relation: &str, from: &str, to: &str) -> bool {
    match relation {
        "asserts" => from == "spec" && to == "fact",
        "derives" => from == "rule" && matches!(to, "relation" | "fact"),
        "depends_on" => (from == "rule" && to == "relation") || (from == "fact" && to == "fact"),
        "proves" | "matches" => from == "fact" && to == "expectation",
        "expects" => from == "expectation" && to == "relation",
        "targets" => from == "mutation" && matches!(to, "rule" | "relation"),
        "must_fail" => from == "mutation" && to == "expectation",
        "references_symbol" => matches!(from, "fact" | "rule" | "expectation") && to == "symbol",
        _ => false,
    }
}

struct ProjectionBuilder {
    spec: String,
    nodes: BTreeMap<String, GraphNode>,
    edges: BTreeMap<String, GraphEdge>,
}

#[derive(Default)]
struct EdgeMetadata<'a> {
    basis: Option<&'a str>,
    position: Option<usize>,
    negated: Option<bool>,
    witness: Option<&'a str>,
    evidence: Vec<String>,
}

impl ProjectionBuilder {
    fn new(spec: &str) -> Self {
        Self {
            spec: spec.to_string(),
            nodes: BTreeMap::new(),
            edges: BTreeMap::new(),
        }
    }

    fn node(&mut self, id: String, data: GraphNodeData) -> String {
        self.nodes.entry(id.clone()).or_insert_with(|| GraphNode {
            kind: "node".to_string(),
            id: id.clone(),
            spec: self.spec.clone(),
            data,
        });
        id
    }

    fn edge(&mut self, from: &str, to: &str, rel: &str, metadata: EdgeMetadata<'_>) {
        let EdgeMetadata {
            basis,
            position,
            negated,
            witness,
            evidence,
        } = metadata;
        let digest = stable_digest(&(rel, from, to, basis, position, negated, witness));
        let id = format!("lemmaspec:{}:edge:{digest}", self.spec);
        self.edges.entry(id.clone()).or_insert(GraphEdge {
            kind: "edge".to_string(),
            id,
            spec: self.spec.clone(),
            from: from.to_string(),
            to: to.to_string(),
            rel: rel.to_string(),
            basis: basis.map(str::to_string),
            position,
            negated,
            witness: witness.map(str::to_string),
            evidence,
        });
    }

    fn finish(self, status: String) -> GraphProjection {
        GraphProjection {
            spec: self.spec,
            status,
            nodes: self.nodes.into_values().collect(),
            edges: self.edges.into_values().collect(),
        }
    }
}

pub fn project_artifact(source: &str) -> Result<GraphProjection, ArtifactError> {
    let artifact = parse_artifact(source)?;
    project_parsed_artifact(&artifact)
}

pub fn project_parsed_artifact(artifact: &Artifact) -> Result<GraphProjection, ArtifactError> {
    let evaluation = evaluate_parsed_for_projection(artifact.clone())?;
    let artifact = evaluation.artifact;
    let engine = evaluation.engine;
    let status = evaluation.status;
    let expectations = evaluation.expectations;
    let compiled_rules = evaluation.compiled_rules;
    let compiled_expectations = evaluation.compiled_expectations;
    let mut builder = ProjectionBuilder::new(&artifact.name);
    let declarations_by_fact = artifact.facts.iter().fold(
        BTreeMap::<(String, Vec<FactValue>), Vec<String>>::new(),
        |mut declarations, fact| {
            declarations
                .entry((fact.relation.clone(), fact.args.clone()))
                .or_default()
                .push(fact.id.clone());
            declarations
        },
    );
    let facts_by_declaration: BTreeMap<_, _> = artifact
        .facts
        .iter()
        .map(|fact| (fact.id.as_str(), fact))
        .collect();
    let docs_by_declaration: BTreeMap<&str, &str> = artifact
        .facts
        .iter()
        .filter_map(|fact| Some((fact.id.as_str(), fact.doc.as_deref()?)))
        .collect();
    let walked_expectations: BTreeMap<_, _> = expectations
        .iter()
        .map(|expectation| (expectation.id.as_str(), expectation))
        .collect();

    let spec_id = builder.node(
        readable_id(&artifact.name, "spec", &artifact.name),
        GraphNodeData::Spec {
            name: artifact.name.clone(),
            status: status.clone(),
            doc: artifact.doc.clone(),
            notes: artifact.notes.clone(),
        },
    );

    let mut relation_ids = BTreeMap::new();
    for relation in &artifact.relations {
        let id = builder.node(
            readable_id(&artifact.name, "relation", &relation.name),
            GraphNodeData::Relation {
                name: relation.name.clone(),
                args: relation.args.clone(),
                roles: relation.roles.clone(),
                reads: relation.reads.clone(),
                doc: relation.doc.clone(),
            },
        );
        relation_ids.insert(relation.name.as_str(), id);
    }

    let mut rule_ids = BTreeMap::new();
    for rule in &artifact.rules {
        let id = builder.node(
            readable_id(&artifact.name, "rule", &rule.id),
            GraphNodeData::Rule {
                name: rule.id.clone(),
                derive: rule.derive.clone(),
                when: rule.when.clone(),
                condition_ids: rule.condition_ids.clone(),
                doc: rule.doc.clone(),
            },
        );
        rule_ids.insert(rule.id.as_str(), id);
    }

    let mut expectation_ids = BTreeMap::new();
    for expectation in &artifact.expectations {
        let walked = walked_expectations[expectation.id.as_str()];
        let id = builder.node(
            readable_id(&artifact.name, "expectation", &expectation.id),
            GraphNodeData::Expectation {
                name: expectation.id.clone(),
                query: expectation.query.clone(),
                expected_count: expectation.count,
                actual_count: walked.actual_count,
                satisfied: walked.satisfied,
                doc: expectation.doc.clone(),
            },
        );
        expectation_ids.insert(expectation.id.as_str(), id);
    }

    let mut mutation_ids = BTreeMap::new();
    for mutation in &artifact.mutations {
        let id = builder.node(
            readable_id(&artifact.name, "mutation", &mutation.id),
            GraphNodeData::Mutation {
                name: mutation.id.clone(),
                operator: mutation.operator,
                relation: mutation.relation.clone(),
                except: mutation.except.clone(),
                must_fail: mutation.must_fail.clone(),
                doc: mutation.doc.clone(),
            },
        );
        mutation_ids.insert(mutation.id.as_str(), id);
    }

    for mutation in &artifact.mutations {
        let mutation_id = &mutation_ids[mutation.id.as_str()];
        match mutation.operator {
            MutationOperator::DropRule | MutationOperator::DropCondition => {
                for rule in artifact
                    .rules
                    .iter()
                    .filter(|rule| !mutation.except.contains(&rule.id))
                {
                    builder.edge(
                        mutation_id,
                        &rule_ids[rule.id.as_str()],
                        "targets",
                        EdgeMetadata {
                            basis: Some(mutation.operator.as_str()),
                            ..EdgeMetadata::default()
                        },
                    );
                }
            }
            MutationOperator::DropFact => {
                let relation = mutation
                    .relation
                    .as_deref()
                    .expect("validated drop_fact mutation has a relation");
                builder.edge(
                    mutation_id,
                    &relation_ids[relation],
                    "targets",
                    EdgeMetadata {
                        basis: Some("drop_fact"),
                        ..EdgeMetadata::default()
                    },
                );
            }
        }
        if let Some(expectation) = mutation.must_fail.as_deref() {
            builder.edge(
                mutation_id,
                &expectation_ids[expectation],
                "must_fail",
                EdgeMetadata {
                    basis: Some("mutation_oracle"),
                    ..EdgeMetadata::default()
                },
            );
        }
    }

    let mut fact_ids: BTreeMap<(String, Vec<Value>), String> = BTreeMap::new();
    let mut fact_supports = Vec::new();
    for relation in &artifact.relations {
        let Some(stored) = engine.relations.get(&relation.name) else {
            continue;
        };
        for row in &stored.rows {
            let args: Vec<FactValue> = row
                .key
                .iter()
                .map(|value| fact_value(value, &engine.interner))
                .collect();
            let declarations = declarations_by_fact
                .get(&(relation.name.clone(), args.clone()))
                .cloned()
                .unwrap_or_default();
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
            let reading = relation
                .reads
                .as_deref()
                .map(|template| read_fact(template, &relation.roles, &args));
            let doc = declarations
                .iter()
                .filter_map(|declaration| docs_by_declaration.get(declaration.as_str()))
                .map(|doc| doc.to_string())
                .collect::<Vec<_>>();
            let id = builder.node(
                digest_id(&artifact.name, "fact", &(&relation.name, &args)),
                GraphNodeData::Fact {
                    relation: relation.name.clone(),
                    args: args.clone(),
                    origin: origin.to_string(),
                    confidence: row.fact.ann.conf,
                    provenance: row.fact.ann.prov.iter().cloned().collect(),
                    declarations: declarations.clone(),
                    bases: {
                        let asserted: Vec<_> = declarations
                            .iter()
                            .filter_map(|declaration| {
                                facts_by_declaration.get(declaration.as_str())
                            })
                            .collect();
                        if asserted.iter().any(|fact| fact.basis.is_some()) {
                            asserted
                                .into_iter()
                                .map(|fact| {
                                    fact.basis.clone().unwrap_or_else(|| EvidenceBasis {
                                        kind: EvidenceKind::ReviewerDeclared,
                                        source: fact.id.clone(),
                                        identity: None,
                                    })
                                })
                                .collect::<BTreeSet<_>>()
                                .into_iter()
                                .collect()
                        } else {
                            Vec::new()
                        }
                    },
                    reading,
                    doc: (!doc.is_empty()).then(|| doc.join("\n\n")),
                },
            );
            fact_ids.insert((relation.name.clone(), row.key.clone()), id.clone());
            fact_supports.push((
                (relation.name.clone(), row.key.clone()),
                row.fact.supports.clone(),
            ));

            if origin == "asserted" {
                builder.edge(
                    &spec_id,
                    &id,
                    "asserts",
                    EdgeMetadata {
                        basis: Some("declaration"),
                        evidence: declarations,
                        ..EdgeMetadata::default()
                    },
                );
            }
            add_fact_symbol_references(&mut builder, &id, &args);
        }
    }

    for rule in &compiled_rules {
        let clause = &rule.clause;
        let rule_id = &rule_ids[rule.id.as_str()];
        let relation_id = &relation_ids[clause.head.pred.as_str()];
        builder.edge(
            rule_id,
            relation_id,
            "derives",
            EdgeMetadata {
                basis: Some("declaration"),
                ..EdgeMetadata::default()
            },
        );
        add_term_symbol_references(&mut builder, rule_id, &clause.head.args, "rule_head", 0);

        let mut term_position = clause.head.args.len();
        for (position, literal) in clause.body.iter().enumerate() {
            match literal {
                Lit::Pos(atom) | Lit::Neg(atom) => {
                    builder.edge(
                        rule_id,
                        &relation_ids[atom.pred.as_str()],
                        "depends_on",
                        EdgeMetadata {
                            basis: Some("rule_body"),
                            position: Some(position),
                            negated: Some(matches!(literal, Lit::Neg(_))),
                            ..EdgeMetadata::default()
                        },
                    );
                    add_term_symbol_references(
                        &mut builder,
                        rule_id,
                        &atom.args,
                        "rule_body",
                        term_position,
                    );
                    term_position += atom.args.len();
                }
                Lit::Cmp(_, left, right) => {
                    add_term_symbol_reference(
                        &mut builder,
                        rule_id,
                        left,
                        "rule_comparison",
                        term_position,
                    );
                    term_position += 1;
                    add_expr_symbol_references(
                        &mut builder,
                        rule_id,
                        right,
                        "rule_comparison",
                        &mut term_position,
                    );
                }
                Lit::Now(_) => {}
            }
        }
    }

    for (key, supports) in fact_supports {
        let fact_id = &fact_ids[&key];
        let mut projected_rules = BTreeSet::new();
        for support in &supports {
            let Support::Rule { rule, body } = support else {
                continue;
            };
            if !projected_rules.insert(rule.as_str()) {
                continue;
            };
            let Some(rule_id) = rule_ids.get(rule.as_str()) else {
                continue;
            };
            builder.edge(
                rule_id,
                fact_id,
                "derives",
                EdgeMetadata {
                    basis: Some("proof_witness"),
                    witness: Some(rule),
                    ..EdgeMetadata::default()
                },
            );
            for (position, body_key) in body.iter().enumerate() {
                let mut projected_body_facts = BTreeSet::new();
                collect_projected_support_facts(
                    &engine,
                    &fact_ids,
                    body_key,
                    &mut BTreeSet::new(),
                    &mut projected_body_facts,
                );
                for projected_body_key in projected_body_facts {
                    let body_fact_id = &fact_ids[&projected_body_key];
                    builder.edge(
                        fact_id,
                        body_fact_id,
                        "depends_on",
                        EdgeMetadata {
                            basis: Some("proof_witness"),
                            position: Some(position),
                            witness: Some(rule),
                            ..EdgeMetadata::default()
                        },
                    );
                }
            }
        }
    }

    for expectation in &compiled_expectations {
        let atom = &expectation.atom;
        let expectation_id = &expectation_ids[expectation.id.as_str()];
        builder.edge(
            expectation_id,
            &relation_ids[atom.pred.as_str()],
            "expects",
            EdgeMetadata {
                basis: Some("query"),
                ..EdgeMetadata::default()
            },
        );
        add_term_symbol_references(
            &mut builder,
            expectation_id,
            &atom.args,
            "expectation_query",
            0,
        );

        add_expectation_proof_edges(
            &mut builder,
            &fact_ids,
            &engine,
            atom,
            expectation_id,
            walked_expectations[expectation.id.as_str()].satisfied,
            walked_expectations[expectation.id.as_str()].actual_count,
        );
    }

    for symbol in &artifact.symbols {
        if let Some(node) =
            builder
                .nodes
                .get_mut(&digest_id(&artifact.name, "symbol", &symbol.value))
        {
            if let GraphNodeData::Symbol {
                label, source, doc, ..
            } = &mut node.data
            {
                *label = Some(symbol.label.clone());
                *source = symbol.source.clone();
                *doc = symbol.doc.clone();
            }
        }
    }
    let projection = builder.finish(status);
    projection
        .validate_closed()
        .map_err(|error| ArtifactError::new(format!("project graph: {error}")))?;
    Ok(projection)
}

fn add_fact_symbol_references(builder: &mut ProjectionBuilder, from: &str, args: &[FactValue]) {
    for (position, argument) in args.iter().enumerate() {
        if let FactValue::Symbol(value) = argument {
            let symbol_id = symbol_node(builder, value);
            builder.edge(
                from,
                &symbol_id,
                "references_symbol",
                EdgeMetadata {
                    basis: Some("fact_argument"),
                    position: Some(position),
                    ..EdgeMetadata::default()
                },
            );
        }
    }
}

fn add_term_symbol_references(
    builder: &mut ProjectionBuilder,
    from: &str,
    terms: &[Term],
    basis: &str,
    position_offset: usize,
) {
    for (position, term) in terms.iter().enumerate() {
        add_term_symbol_reference(builder, from, term, basis, position_offset + position);
    }
}

fn add_term_symbol_reference(
    builder: &mut ProjectionBuilder,
    from: &str,
    term: &Term,
    basis: &str,
    position: usize,
) {
    match term {
        Term::Sym(value) => {
            let symbol_id = symbol_node(builder, value);
            builder.edge(
                from,
                &symbol_id,
                "references_symbol",
                EdgeMetadata {
                    basis: Some(basis),
                    position: Some(position),
                    ..EdgeMetadata::default()
                },
            );
        }
        Term::Agg(_, inner) => {
            add_term_symbol_reference(builder, from, inner, basis, position);
        }
        Term::Var(_) | Term::Int(_) | Term::Wildcard => {}
    }
}

fn collect_projected_support_facts(
    engine: &crate::Engine,
    fact_ids: &BTreeMap<(String, Vec<Value>), String>,
    key: &(String, Vec<Value>),
    visited: &mut BTreeSet<(String, Vec<Value>)>,
    projected: &mut BTreeSet<(String, Vec<Value>)>,
) {
    if fact_ids.contains_key(key) {
        projected.insert(key.clone());
        return;
    }
    if !visited.insert(key.clone()) {
        return;
    }
    let Some(fact) = engine.fact(&key.0, &key.1) else {
        return;
    };
    for support in fact.supports {
        if let Support::Rule { body, .. } = support {
            for body_key in body {
                collect_projected_support_facts(engine, fact_ids, &body_key, visited, projected);
            }
        }
    }
}

fn add_expr_symbol_references(
    builder: &mut ProjectionBuilder,
    from: &str,
    expression: &Expr,
    basis: &str,
    position: &mut usize,
) {
    match expression {
        Expr::T(term) => {
            add_term_symbol_reference(builder, from, term, basis, *position);
            *position += 1;
        }
        Expr::Add(left, right) | Expr::Sub(left, right) => {
            add_expr_symbol_references(builder, from, left, basis, position);
            add_expr_symbol_references(builder, from, right, basis, position);
        }
    }
}

fn symbol_node(builder: &mut ProjectionBuilder, value: &str) -> String {
    builder.node(
        digest_id(&builder.spec, "symbol", &value),
        GraphNodeData::Symbol {
            value: value.to_string(),
            label: None,
            source: None,
            doc: None,
        },
    )
}

fn add_expectation_proof_edges(
    builder: &mut ProjectionBuilder,
    fact_ids: &BTreeMap<(String, Vec<Value>), String>,
    engine: &crate::Engine,
    atom: &Atom,
    expectation_id: &str,
    satisfied: bool,
    actual_count: usize,
) {
    if actual_count == 0 {
        return;
    }
    let Some(relation) = engine.relations.get(&atom.pred) else {
        return;
    };
    // Matching facts prove a satisfied claim. For an open claim they are the
    // facts a reader has to confront: what was found instead of the count.
    let (rel, basis) = if satisfied {
        ("proves", "satisfied_query")
    } else {
        ("matches", "unsatisfied_query")
    };
    for row in &relation.rows {
        if !atom_matches(atom, &row.key, &engine.interner) {
            continue;
        }
        let Some(fact_id) = fact_ids.get(&(atom.pred.clone(), row.key.clone())) else {
            continue;
        };
        builder.edge(
            fact_id,
            expectation_id,
            rel,
            EdgeMetadata {
                basis: Some(basis),
                ..EdgeMetadata::default()
            },
        );
    }
}

fn atom_matches(atom: &Atom, row: &[Value], interner: &Interner) -> bool {
    let mut variables = BTreeMap::new();
    atom.args.iter().zip(row).all(|(term, value)| match term {
        Term::Var(variable) => match variables.get(variable) {
            Some(bound) => bound == value,
            None => {
                variables.insert(variable, *value);
                true
            }
        },
        Term::Sym(symbol) => {
            matches!(value, Value::Sym(value) if interner.resolve(*value) == symbol)
        }
        Term::Int(integer) => matches!(value, Value::Int(value) if value == integer),
        Term::Wildcard => true,
        Term::Agg(..) => false,
    })
}

fn fact_value(value: &Value, interner: &Interner) -> FactValue {
    match value {
        Value::Sym(symbol) => FactValue::Symbol(interner.resolve(*symbol).to_string()),
        Value::Int(integer) => FactValue::Integer(*integer),
    }
}

fn readable_id(spec: &str, kind: &str, name: &str) -> String {
    format!("lemmaspec:{spec}:{kind}:{name}")
}

fn digest_id<T: Serialize>(spec: &str, kind: &str, value: &T) -> String {
    format!("lemmaspec:{spec}:{kind}:{}", stable_digest(value))
}

fn stable_digest<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).expect("projection identity values are serializable");
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}
