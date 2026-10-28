//! Format-agnostic graph index and answer-first brief construction.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::artifact::{EvidenceBasis, EvidenceKind, FactValue};
use crate::narrative::{read_with, value_text};
use crate::projection::{GraphNode, GraphNodeData, GraphProjection};

/// Every fact stands somewhere on the ladder from evidence to conclusion.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Standing {
    /// Asserted and backed by provenance.
    Observation,
    /// Asserted without provenance, or below full confidence: a decision point.
    Assumption,
    /// Derived by a rule.
    Conclusion,
}

impl Standing {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Standing::Observation => "observation",
            Standing::Assumption => "reviewer-declared",
            Standing::Conclusion => "conclusion",
        }
    }

    pub(crate) fn tone(self) -> &'static str {
        match self {
            Standing::Observation => "c-stable",
            Standing::Assumption => "c-attention",
            Standing::Conclusion => "c-additive",
        }
    }
}

pub(crate) struct Relation<'a> {
    pub(crate) roles: &'a [String],
    pub(crate) reads: Option<&'a str>,
    pub(crate) doc: Option<&'a str>,
}

/// One producing rule and the facts its witness needed.
pub(crate) struct Support<'a> {
    pub(crate) rule: &'a str,
    pub(crate) body: Vec<&'a str>,
}

pub(crate) struct Index<'a> {
    pub(crate) projection: &'a GraphProjection,
    pub(crate) source_context: Option<&'a crate::SourceContext>,
    pub(crate) nodes: BTreeMap<&'a str, &'a GraphNode>,
    pub(crate) relations: BTreeMap<&'a str, Relation<'a>>,
    pub(crate) symbols: BTreeMap<&'a str, Option<&'a str>>,
    pub(crate) symbol_references: BTreeMap<&'a str, Vec<&'a str>>,
    pub(crate) instance_of: BTreeMap<&'a str, Vec<&'a str>>,
    pub(crate) expects: BTreeMap<&'a str, Vec<&'a str>>,
    pub(crate) proof_rules: BTreeMap<&'a str, Vec<&'a str>>,
    pub(crate) rules_by_name: BTreeMap<&'a str, &'a str>,
    pub(crate) expectations_by_name: BTreeMap<&'a str, &'a str>,
    pub(crate) supports: BTreeMap<&'a str, Vec<Support<'a>>>,
    pub(crate) dependents: BTreeMap<&'a str, Vec<&'a str>>,
    pub(crate) proves: BTreeMap<&'a str, Vec<&'a str>>,
    pub(crate) proven_by: BTreeMap<&'a str, Vec<&'a str>>,
    pub(crate) matched_by: BTreeMap<&'a str, Vec<&'a str>>,
    pub(crate) rule_yield: BTreeMap<&'a str, usize>,
    pub(crate) derived_by: BTreeMap<&'a str, Vec<&'a str>>,
    pub(crate) declaration_ids: BTreeSet<&'a str>,
}

impl<'a> Index<'a> {
    pub(crate) fn build(projection: &'a GraphProjection) -> Self {
        let nodes: BTreeMap<&str, &GraphNode> = projection
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let mut relations = BTreeMap::new();
        let mut symbols = BTreeMap::new();
        let mut rules_by_name = BTreeMap::new();
        let mut expectations_by_name = BTreeMap::new();
        let mut declaration_ids = BTreeSet::new();
        for node in &projection.nodes {
            match &node.data {
                GraphNodeData::Fact { declarations, .. } => {
                    declaration_ids.extend(declarations.iter().map(String::as_str));
                }
                GraphNodeData::Relation {
                    name,
                    roles,
                    reads,
                    doc,
                    ..
                } => {
                    relations.insert(
                        name.as_str(),
                        Relation {
                            roles,
                            reads: reads.as_deref(),
                            doc: doc.as_deref(),
                        },
                    );
                }
                GraphNodeData::Symbol { value, label, .. } => {
                    symbols.entry(value.as_str()).or_insert(label.as_deref());
                }
                GraphNodeData::Rule { name, .. } => {
                    rules_by_name.insert(name.as_str(), node.id.as_str());
                }
                GraphNodeData::Expectation { name, .. } => {
                    expectations_by_name.insert(name.as_str(), node.id.as_str());
                }
                _ => {}
            }
        }

        let mut supports: BTreeMap<&str, Vec<Support>> = BTreeMap::new();
        let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut proves: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut proven_by: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut matched_by: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut rule_yield: BTreeMap<&str, usize> = BTreeMap::new();
        let mut derived_by: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut symbol_references: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut instance_of: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut expects: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut proof_rules: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for edge in &projection.edges {
            let (from, to) = (edge.from.as_str(), edge.to.as_str());
            let node_type = |id: &str| nodes.get(id).map(|node| node.node_type());
            match (edge.rel.as_str(), edge.basis.as_deref()) {
                ("depends_on", Some("proof_witness")) => {
                    let Some(rule) = edge.witness.as_deref() else {
                        continue;
                    };
                    let entry = supports.entry(from).or_default();
                    match entry.iter_mut().find(|support| support.rule == rule) {
                        Some(support) => support.body.push(to),
                        None => entry.push(Support {
                            rule,
                            body: vec![to],
                        }),
                    }
                    dependents.entry(to).or_default().push(from);
                }
                ("proves", _) => {
                    proves.entry(from).or_default().push(to);
                    proven_by.entry(to).or_default().push(from);
                }
                ("matches", _) => {
                    matched_by.entry(to).or_default().push(from);
                }
                ("derives", Some("proof_witness")) => {
                    *rule_yield.entry(from).or_default() += 1;
                    proof_rules.entry(to).or_default().push(from);
                }
                ("references_symbol", _) => {
                    symbol_references.entry(from).or_default().push(to);
                }
                ("instance_of", _) => {
                    instance_of.entry(from).or_default().push(to);
                }
                ("expects", _) => {
                    expects.entry(from).or_default().push(to);
                }
                ("derives", Some("declaration")) if node_type(to) == Some("relation") => {
                    derived_by.entry(to).or_default().push(from);
                }
                _ => {}
            }
        }

        Self {
            projection,
            source_context: None,
            nodes,
            relations,
            symbols,
            symbol_references,
            instance_of,
            expects,
            proof_rules,
            rules_by_name,
            expectations_by_name,
            supports,
            dependents,
            proves,
            proven_by,
            matched_by,
            rule_yield,
            derived_by,
            declaration_ids,
        }
    }

    /// Provenance the author attached. The engine also seeds every asserted
    /// fact's provenance with its own declaration id and unions those into
    /// derived facts; declaration ids are bookkeeping, not evidence.
    pub(crate) fn evidence(&self, node: &'a GraphNode) -> Vec<&'a str> {
        match &node.data {
            GraphNodeData::Fact { provenance, .. } => provenance
                .iter()
                .map(String::as_str)
                .filter(|source| !self.declaration_ids.contains(source))
                .collect(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn facts(&self) -> impl Iterator<Item = &'a GraphNode> + '_ {
        self.projection
            .nodes
            .iter()
            .filter(|node| matches!(node.data, GraphNodeData::Fact { .. }))
    }

    pub(crate) fn standing(&self, node: &'a GraphNode) -> Standing {
        match &node.data {
            GraphNodeData::Fact { origin, bases, .. } => {
                if origin == "derived" {
                    Standing::Conclusion
                } else if !bases.is_empty()
                    && bases
                        .iter()
                        .all(|basis| basis.kind != EvidenceKind::ReviewerDeclared)
                {
                    Standing::Observation
                } else {
                    Standing::Assumption
                }
            }
            _ => Standing::Conclusion,
        }
    }

    /// A fact as a sentence: the relation's template when it has one,
    /// otherwise the atom with role names alongside the values.
    pub(crate) fn symbol_text(&self, value: &str) -> String {
        match self.symbols.get(value) {
            Some(Some(label)) => (*label).to_string(),
            Some(None) => format!("{value} (unlabelled)"),
            None => value.to_string(),
        }
    }

    pub(crate) fn sentence_text(&self, node: &GraphNode) -> String {
        let GraphNodeData::Fact { relation, args, .. } = &node.data else {
            return label(node);
        };
        let texts: Vec<String> = args
            .iter()
            .map(|value| match value {
                FactValue::Symbol(value) => self.symbol_text(value),
                FactValue::Integer(value) => value.to_string(),
            })
            .collect();
        match self.relations.get(relation.as_str()) {
            Some(Relation {
                reads: Some(template),
                roles,
                ..
            }) => read_with(template, roles, &texts),
            _ => format!("{}: {}", relation.replace('_', " "), texts.join(", ")),
        }
    }

    /// Everything that would fall if this fact were withdrawn.
    pub(crate) fn blast_radius(&self, fact_id: &str) -> (usize, usize) {
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::from([fact_id]);
        let mut claims = BTreeSet::new();
        while let Some(current) = queue.pop_front() {
            if let Some(expectations) = self.proves.get(current) {
                claims.extend(expectations.iter().copied());
            }
            for dependent in self.dependents.get(current).into_iter().flatten() {
                if seen.insert(*dependent) {
                    queue.push_back(dependent);
                }
            }
        }
        (seen.len(), claims.len())
    }

    pub(crate) fn brief_fact(&self, node: &GraphNode) -> BriefFact {
        let mut sources = BTreeSet::new();
        for symbol in self
            .symbol_references
            .get(node.id.as_str())
            .into_iter()
            .flatten()
        {
            if let Some(GraphNode {
                data:
                    GraphNodeData::Symbol {
                        source: Some(source),
                        ..
                    },
                ..
            }) = self.nodes.get(symbol)
            {
                sources.insert(source.clone());
            }
        }
        BriefFact {
            sentence: self.sentence_text(node),
            bases: match &node.data {
                GraphNodeData::Fact { bases, .. } => bases.clone(),
                _ => Vec::new(),
            },
            standing: standing_text(node),
            sources: sources.into_iter().collect(),
            provenance: self
                .evidence(node)
                .into_iter()
                .map(str::to_string)
                .collect(),
            rules: self
                .proof_rules
                .get(node.id.as_str())
                .into_iter()
                .flatten()
                .filter_map(|rule| self.nodes.get(rule))
                .filter_map(|node| match &node.data {
                    GraphNodeData::Rule {
                        name, when, doc, ..
                    } => Some(doc.clone().unwrap_or_else(|| {
                        format!(
                            "{name}: {}",
                            when.iter()
                                .map(|condition| self.query_text(condition))
                                .collect::<Vec<_>>()
                                .join(" and ")
                        )
                    })),
                    _ => None,
                })
                .collect(),
        }
    }

    pub(crate) fn witness_order(&'a self, roots: &[&'a str]) -> Vec<&'a str> {
        let mut seen = BTreeSet::new();
        let mut result = Vec::new();
        let mut stack: Vec<(&str, bool)> = roots.iter().rev().map(|id| (*id, false)).collect();
        while let Some((id, expanded)) = stack.pop() {
            if expanded {
                result.push(id);
                continue;
            }
            if !seen.insert(id) {
                continue;
            }
            stack.push((id, true));
            for support in self.supports.get(id).into_iter().flatten().rev() {
                stack.extend(support.body.iter().rev().map(|id| (*id, false)));
            }
        }
        result
    }

    pub(crate) fn query_text(&self, query: &str) -> String {
        let Some((negated, relation, args)) = split_atom(query) else {
            return query.to_string();
        };
        let args: Vec<_> = args
            .iter()
            .map(|arg| self.symbol_text(arg.trim_matches('"')))
            .collect();
        let text = match self.relations.get(relation) {
            Some(Relation {
                reads: Some(template),
                roles,
                ..
            }) => read_with(template, roles, &args),
            _ => format!("{}: {}", relation.replace('_', " "), args.join(", ")),
        };
        if negated {
            format!("not {text}")
        } else {
            text
        }
    }
}

pub(crate) fn label(node: &GraphNode) -> String {
    match &node.data {
        GraphNodeData::Fact { relation, args, .. } => format!(
            "{relation}({})",
            args.iter().map(value_text).collect::<Vec<_>>().join(", ")
        ),
        GraphNodeData::Symbol { value, label, .. } => label
            .clone()
            .unwrap_or_else(|| format!("{value} (unlabelled)")),
        GraphNodeData::Spec { name, .. }
        | GraphNodeData::Relation { name, .. }
        | GraphNodeData::Rule { name, .. }
        | GraphNodeData::Mutation { name, .. }
        | GraphNodeData::Expectation { name, .. } => name.clone(),
    }
}

/// `!governed_by(M, P)` → (true, "governed_by", ["M", "P"]).
pub(crate) fn split_atom(text: &str) -> Option<(bool, &str, Vec<String>)> {
    let trimmed = text.trim();
    let (negated, rest) = match trimmed.strip_prefix('!') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, trimmed),
    };
    let open = rest.find('(')?;
    let predicate = rest[..open].trim();
    if predicate.is_empty()
        || !predicate
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        || !rest.ends_with(')')
    {
        return None;
    }
    let inner = &rest[open + 1..rest.len() - 1];
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut current = String::new();
    for character in inner.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                current.push(character);
            }
            '(' if !quoted => {
                depth += 1;
                current.push(character);
            }
            ')' if !quoted => {
                depth = depth.saturating_sub(1);
                current.push(character);
            }
            ',' if !quoted && depth == 0 => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(character),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }
    Some((negated, predicate, args))
}

/// The answer and its finite proof closure, shared by both human report formats.
pub(crate) struct Brief {
    pub question: Option<String>,
    pub answers: Vec<Answer>,
}

pub(crate) struct Answer {
    pub id: String,
    pub sentence: String,
    pub explanation: Option<String>,
    pub satisfied: bool,
    pub counts: String,
    pub steps: Vec<BriefFact>,
    pub premises: Vec<BriefFact>,
    pub closure: Vec<String>,
}

pub(crate) struct BriefFact {
    pub sentence: String,
    pub bases: Vec<EvidenceBasis>,
    pub standing: String,
    pub sources: Vec<String>,
    pub provenance: Vec<String>,
    pub rules: Vec<String>,
}

pub(crate) fn brief(projection: &GraphProjection) -> Brief {
    let index = Index::build(projection);
    brief_from_index(&index)
}

pub(crate) fn brief_from_index(index: &Index<'_>) -> Brief {
    let projection = index.projection;
    let mut expectations: Vec<_> = projection
        .nodes
        .iter()
        .filter(|node| matches!(node.data, GraphNodeData::Expectation { .. }))
        .collect();
    expectations.sort_by_key(|node| match node.data {
        GraphNodeData::Expectation { satisfied, .. } => satisfied,
        _ => true,
    });
    let answers = expectations
        .into_iter()
        .map(|node| {
            let GraphNodeData::Expectation {
                query,
                expected_count,
                actual_count,
                satisfied,
                doc,
                ..
            } = &node.data
            else {
                unreachable!()
            };
            let roots: Vec<&str> = index
                .proven_by
                .get(node.id.as_str())
                .into_iter()
                .flatten()
                .chain(index.matched_by.get(node.id.as_str()).into_iter().flatten())
                .copied()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let order = index.witness_order(&roots);
            let steps: Vec<_> = order
                .iter()
                .filter_map(|id| index.nodes.get(id))
                .map(|node| index.brief_fact(node))
                .collect();
            let premises = order
                .iter()
                .filter_map(|id| index.nodes.get(id))
                .filter(|node| index.standing(node) != Standing::Conclusion)
                .map(|node| index.brief_fact(node))
                .collect();
            let sentence = if roots.is_empty() {
                format!(
                    "{}: {}.",
                    if *actual_count == 0 {
                        "No matching result"
                    } else {
                        "Results found"
                    },
                    index.query_text(query)
                )
            } else {
                roots
                    .iter()
                    .filter_map(|id| index.nodes.get(id))
                    .map(|node| index.sentence_text(node))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            let mut closure: BTreeSet<String> = order.iter().map(|id| (*id).to_string()).collect();
            closure.insert(node.id.clone());
            for expected in index.expects.get(node.id.as_str()).into_iter().flatten() {
                closure.insert((*expected).to_string());
            }
            for &witness_id in &order {
                for rule_id in index.proof_rules.get(witness_id).into_iter().flatten() {
                    closure.insert((*rule_id).to_string());
                }
                for sym_id in index
                    .symbol_references
                    .get(witness_id)
                    .into_iter()
                    .flatten()
                {
                    closure.insert((*sym_id).to_string());
                }
                for inst_id in index.instance_of.get(witness_id).into_iter().flatten() {
                    closure.insert((*inst_id).to_string());
                }
            }
            Answer {
                id: node.id.clone(),
                sentence,
                explanation: doc.clone(),
                satisfied: *satisfied,
                counts: format!("Expected {expected_count}; found {actual_count}."),
                steps,
                premises,
                closure: closure.into_iter().collect(),
            }
        })
        .collect();
    Brief {
        question: projection.nodes.iter().find_map(|node| match &node.data {
            GraphNodeData::Spec { doc, .. } => doc.clone(),
            _ => None,
        }),
        answers,
    }
}

pub(crate) fn display_labels_from_index(index: &Index<'_>) -> BTreeMap<String, String> {
    index
        .projection
        .nodes
        .iter()
        .map(|node| {
            (
                node.id.clone(),
                match node.data {
                    GraphNodeData::Fact { .. } => index.sentence_text(node),
                    _ => label(node),
                },
            )
        })
        .collect()
}

pub(crate) fn basis_label(kind: &EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::Snapshot => "snapshot fact",
        EvidenceKind::Policy => "policy fact",
        EvidenceKind::Observed => "observed status",
        EvidenceKind::ReviewerDeclared => "reviewer-declared",
    }
}

pub(crate) fn standing_text(node: &GraphNode) -> String {
    match &node.data {
        GraphNodeData::Fact { origin, .. } if origin == "derived" => "conclusion".to_string(),
        GraphNodeData::Fact { bases, .. } if !bases.is_empty() => bases
            .iter()
            .map(|basis| basis_label(&basis.kind))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(" / "),
        _ => "reviewer-declared".to_string(),
    }
}

/// Freshness is meaningful only against an operator-supplied identity.
pub fn observation_identity_mismatch(projection: &GraphProjection, target: &str) -> bool {
    projection.nodes.iter().any(|node| match &node.data {
        GraphNodeData::Fact { bases, .. } => bases.iter().any(|basis| {
            basis.kind == EvidenceKind::Observed && basis.identity.as_deref() != Some(target)
        }),
        _ => false,
    })
}
