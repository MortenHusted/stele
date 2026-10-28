//! The human guide: one artifact read as an argument, from observations to
//! conclusions, so a reader can follow, challenge, and decide.

mod index;

use std::collections::BTreeMap;

use crate::artifact::MutationOperator;
use crate::html::html_escape;
use crate::narrative::read_with;
use crate::projection::{GraphNode, GraphNodeData, GraphProjection};

pub use index::observation_identity_mismatch;
pub(crate) use index::{
    basis_label, brief, brief_from_index, display_labels_from_index, label, split_atom,
    standing_text, Brief, BriefFact, Index, Relation, Standing,
};

impl Index<'_> {
    fn sentence(&self, node: &GraphNode) -> String {
        html_escape(&self.sentence_text(node))
    }

    fn atom_markup(&self, relation: &str, args: &[String]) -> String {
        let roles = self
            .relations
            .get(relation)
            .map(|info| info.roles)
            .unwrap_or_default();
        let inner = args
            .iter()
            .enumerate()
            .map(|(position, value)| match roles.get(position) {
                Some(role) => format!("<i>{}</i> {}", html_escape(role), html_escape(value)),
                None => html_escape(value),
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("<code>{}({inner})</code>", html_escape(relation))
    }

    /// A rule atom read with its variables in place, e.g. `Mutation is
    /// governed by Policy`. Comparisons and anything unparsable stay verbatim.
    fn condition_markup(&self, text: &str) -> String {
        match split_atom(text) {
            Some((negated, predicate, args)) => {
                let roles = self
                    .relations
                    .get(predicate)
                    .map(|info| info.roles)
                    .unwrap_or_default();
                let args: Vec<String> = args
                    .into_iter()
                    .enumerate()
                    .map(
                        |(position, arg)| match (arg.as_str(), roles.get(position)) {
                            ("_", Some(role)) => format!("any {role}"),
                            ("_", None) => "anything".to_string(),
                            _ => self.symbol_text(arg.trim_matches('"')),
                        },
                    )
                    .collect();
                let body = match self.relations.get(predicate) {
                    Some(Relation {
                        reads: Some(template),
                        roles,
                        ..
                    }) => html_escape(&read_with(template, roles, &args)),
                    _ => self.atom_markup(predicate, &args),
                };
                if negated {
                    format!("<span class=\"not\">not</span> {body}")
                } else {
                    body
                }
            }
            None => format!("<code>{}</code>", html_escape(text)),
        }
    }

    fn link(&self, id: &str) -> String {
        let Some(node) = self.nodes.get(id) else {
            return html_escape(id);
        };
        format!(
            "<a class=\"ref\" href=\"#{}\">{}</a>",
            anchor(id),
            match &node.data {
                GraphNodeData::Fact { .. } => self.sentence(node),
                _ => html_escape(&label(node)),
            }
        )
    }

    fn because(&self, fact_id: &str, depth: usize, path: &mut Vec<String>) -> String {
        let Some(supports) = self.supports.get(fact_id) else {
            return String::new();
        };
        path.push(fact_id.to_string());
        let mut html = String::from("<ul class=\"because\">");
        for support in supports {
            let rule_link = self
                .rules_by_name
                .get(support.rule)
                .map(|id| self.link(id))
                .unwrap_or_else(|| html_escape(support.rule));
            html.push_str(&format!(
                "<li><span class=\"via\">via rule {rule_link}</span><ul>"
            ));
            for body in &support.body {
                let Some(node) = self.nodes.get(body) else {
                    continue;
                };
                let standing = self.standing(node);
                html.push_str(&format!(
                    "<li><span class=\"chip {}\">{}</span> {}",
                    standing.tone(),
                    standing_text(node),
                    self.link(body)
                ));
                if standing == Standing::Conclusion
                    && depth < 4
                    && !path.iter().any(|seen| seen == body)
                {
                    let nested = self.because(body, depth + 1, path);
                    if !nested.is_empty() {
                        html.push_str(&format!(
                            "<details><summary>why</summary>{nested}</details>"
                        ));
                    }
                }
                html.push_str("</li>");
            }
            html.push_str("</ul></li>");
        }
        html.push_str("</ul>");
        path.pop();
        html
    }
}

/// Stable element id for a projection node, shared with the graph script.
pub(crate) fn anchor(id: &str) -> String {
    let mut anchor = String::with_capacity(id.len() + 2);
    anchor.push_str("n-");
    anchor.extend(id.chars().map(|character| {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            character
        } else {
            '-'
        }
    }));
    anchor
}

fn paragraphs(doc: &str) -> String {
    doc.split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .map(|paragraph| format!("<p>{}</p>", html_escape(paragraph).replace('\n', " ")))
        .collect()
}

fn doc_markup(doc: Option<&str>) -> String {
    doc.map(|doc| format!("<div class=\"doc\">{}</div>", paragraphs(doc)))
        .unwrap_or_default()
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// One stop on the journey. Its cards double as the set of nodes the graph
/// emphasises while the reader is on this step.
fn step(key: &str, title: &str, intro: &str, empty: &str, body: String) -> String {
    let content = if body.is_empty() {
        format!("<p class=\"empty\">{}</p>", html_escape(empty))
    } else {
        body
    };
    format!(
        "<section class=\"step\" data-step=\"{key}\" hidden><header class=\"step-head\"><h2>{}</h2><p class=\"intro\">{}</p></header><div class=\"step-body\">{content}</div></section>",
        html_escape(title),
        html_escape(intro)
    )
}

struct Card<'a> {
    id: &'a str,
    class: &'a str,
    attrs: String,
    tone: &'a str,
    chip: &'a str,
    summary: String,
    hint: String,
    detail: String,
}

/// A card is one line until selected; selecting it opens the detail and
/// lights the same node in the graph.
fn card(card: Card) -> String {
    let hint = if card.hint.is_empty() {
        String::new()
    } else {
        format!("<span class=\"hint\">{}</span>", card.hint)
    };
    format!(
        "<article class=\"card {}\" id=\"{}\" data-node=\"{}\"{}><button type=\"button\" class=\"card-summary\"><span class=\"chip {}\">{}</span><span class=\"sentence\">{}</span>{hint}</button><div class=\"card-detail\" hidden>{}</div></article>",
        card.class,
        anchor(card.id),
        html_escape(card.id),
        card.attrs,
        card.tone,
        card.chip,
        card.summary,
        card.detail
    )
}

fn fact_meta(index: &Index, node: &GraphNode) -> String {
    let GraphNodeData::Fact {
        relation,
        confidence,
        ..
    } = &node.data
    else {
        return String::new();
    };
    let sources = index.evidence(node);
    let mut meta = vec![format!("<code>{}</code>", html_escape(relation))];
    if let GraphNodeData::Fact { bases, .. } = &node.data {
        for basis in bases {
            meta.push(format!(
                "{}: {}{}",
                basis_label(&basis.kind),
                source_html(&basis.source, index.source_context),
                basis
                    .identity
                    .as_ref()
                    .map(|identity| format!(" as of {}", html_escape(identity)))
                    .unwrap_or_default()
            ));
        }
    }
    if !sources.is_empty() {
        meta.push(format!(
            "evidence: {}",
            sources
                .iter()
                .map(|source| html_escape(source))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if *confidence < 1.0 {
        meta.push(format!("{:.0}% confidence", confidence * 100.0));
    }
    format!("<div class=\"meta\">{}</div>", meta.join(" · "))
}

fn fact_card(index: &Index, node: &GraphNode, hint: String, extra: &str) -> String {
    let GraphNodeData::Fact { doc, .. } = &node.data else {
        return String::new();
    };
    let standing = index.standing(node);
    card(Card {
        id: &node.id,
        class: "fact",
        attrs: format!(" data-standing=\"{}\"", standing.label()),
        tone: standing.tone(),
        chip: &standing_text(node),
        summary: index.sentence(node),
        hint,
        detail: format!(
            "{}{}{extra}",
            fact_meta(index, node),
            doc_markup(doc.as_deref())
        ),
    })
}

/// Facts under the concept they describe; long groups start folded.
fn grouped_facts(
    index: &Index,
    facts: &[&GraphNode],
    noun: (&str, &str),
    detail: impl Fn(&GraphNode) -> String,
) -> String {
    let mut groups: BTreeMap<&str, Vec<&GraphNode>> = BTreeMap::new();
    for node in facts {
        if let GraphNodeData::Fact { relation, .. } = &node.data {
            groups.entry(relation.as_str()).or_default().push(node);
        }
    }
    groups
        .into_iter()
        .map(|(relation, nodes)| {
            let doc = index
                .relations
                .get(relation)
                .and_then(|info| info.doc)
                .map(|doc| format!("<p class=\"group-doc\">{}</p>", html_escape(doc)))
                .unwrap_or_default();
            let cards: String = nodes
                .iter()
                .map(|node| fact_card(index, node, String::new(), &detail(node)))
                .collect();
            format!(
                "<details class=\"group\"{}><summary><strong>{}</strong><span class=\"count\">{}</span></summary>{doc}{cards}</details>",
                if nodes.len() <= 8 { " open" } else { "" },
                html_escape(relation),
                plural(nodes.len(), noun.0, noun.1)
            )
        })
        .collect()
}

pub struct GuideSections {
    pub question: String,
    pub observations: String,
    pub assumptions: String,
    pub reasoning: String,
    pub conclusions: String,
    pub claims: String,
    pub stress_tests: String,
    pub reference: String,
}

pub(crate) struct PreparedHtmlGuide {
    pub sections: GuideSections,
    pub brief: Brief,
    pub labels: BTreeMap<String, String>,
}

/// Build the index once and derive all HTML guide views from it.
pub(crate) fn prepare_html_guide(
    projection: &GraphProjection,
    context: Option<&crate::SourceContext>,
) -> PreparedHtmlGuide {
    let mut index = Index::build(projection);
    index.source_context = context;
    let sections = GuideSections {
        question: render_question(&index),
        observations: render_observations(&index),
        assumptions: render_assumptions(&index),
        reasoning: render_reasoning(&index),
        conclusions: render_conclusions(&index),
        claims: render_claims(&index),
        stress_tests: render_stress_tests(&index),
        reference: render_reference(&index),
    };
    let brief = brief_from_index(&index);
    let labels = display_labels_from_index(&index);
    PreparedHtmlGuide {
        sections,
        brief,
        labels,
    }
}

fn render_question(index: &Index) -> String {
    index
        .projection
        .nodes
        .iter()
        .find_map(|node| match &node.data {
            GraphNodeData::Spec { doc: Some(doc), .. } => {
                Some(format!("<div class=\"question\">{}</div>", paragraphs(doc)))
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            "<p class=\"empty\">This artifact states no question. A comment above `spec` becomes one.</p>".to_string()
        })
}

fn render_observations(index: &Index) -> String {
    let facts: Vec<&GraphNode> = index
        .facts()
        .filter(|node| index.standing(node) == Standing::Observation)
        .collect();
    step(
        "observations",
        "Observations",
        "Facts with an explicitly declared observed standing.",
        "No typed observations are declared. Freeform provenance alone does not establish an observation.",
        grouped_facts(index, &facts, ("fact", "facts"), |_| String::new()),
    )
}

fn render_assumptions(index: &Index) -> String {
    let mut facts: Vec<(&GraphNode, (usize, usize))> = index
        .facts()
        .filter(|node| index.standing(node) == Standing::Assumption)
        .map(|node| (node, index.blast_radius(&node.id)))
        .collect();
    facts.sort_by(|left, right| {
        right
            .1
             .1
            .cmp(&left.1 .1)
            .then(right.1 .0.cmp(&left.1 .0))
            .then_with(|| left.0.id.cmp(&right.0.id))
    });
    let cards: String = facts
        .iter()
        .map(|(node, (conclusions, claims))| {
            let (hint, radius) = if *conclusions == 0 && *claims == 0 {
                (
                    String::new(),
                    "<div class=\"radius quiet\">Nothing derived rests on this yet.</div>"
                        .to_string(),
                )
            } else {
                (
                    format!("{conclusions} · {claims}"),
                    format!(
                        "<div class=\"radius\">If this is wrong, {} and {} fall with it.</div>",
                        plural(*conclusions, "conclusion", "conclusions"),
                        plural(*claims, "claim", "claims")
                    ),
                )
            };
            fact_card(index, node, hint, &radius)
        })
        .collect();
    step(
        "assumptions",
        "Assumptions",
        "Reviewer-declared facts. Citations identify their sources but do not verify the declarations.",
        "No reviewer-declared facts.",
        cards,
    )
}

fn render_reasoning(index: &Index) -> String {
    let cards: String = index
        .projection
        .nodes
        .iter()
        .filter_map(|node| {
            let GraphNodeData::Rule {
                name,
                derive,
                when,
                condition_ids,
                doc,
            } = &node.data
            else {
                return None;
            };
            let conditions: String = when
                .iter()
                .enumerate()
                .map(|(position, condition)| {
                    let id = condition_ids
                        .get(position)
                        .map(|id| format!("<span class=\"cond\">{}</span>", html_escape(id)))
                        .unwrap_or_default();
                    format!("<li>{id}{}</li>", index.condition_markup(condition))
                })
                .collect();
            let yielded = index
                .rule_yield
                .get(node.id.as_str())
                .map(|count| plural(*count, "conclusion", "conclusions"))
                .unwrap_or_else(|| "nothing yet".to_string());
            Some(card(Card {
                id: &node.id,
                class: "rule",
                attrs: String::new(),
                tone: "c-attention",
                chip: "rule",
                summary: html_escape(name),
                hint: yielded,
                detail: format!(
                    "<p class=\"lead\">Concludes <span class=\"sentence\">{}</span> when</p><ol class=\"conditions\">{conditions}</ol>{}",
                    index.condition_markup(derive),
                    doc_markup(doc.as_deref())
                ),
            }))
        })
        .collect();
    step(
        "reasoning",
        "Reasoning",
        "The rules that turn observations and assumptions into conclusions. Read each as: this follows whenever all of these hold.",
        "No rules. Everything here is asserted directly.",
        cards,
    )
}

fn render_conclusions(index: &Index) -> String {
    let facts: Vec<&GraphNode> = index
        .facts()
        .filter(|node| index.standing(node) == Standing::Conclusion)
        .collect();
    let body = grouped_facts(index, &facts, ("conclusion", "conclusions"), |node| {
        let because = index.because(&node.id, 0, &mut Vec::new());
        if because.is_empty() {
            String::new()
        } else {
            format!("<div class=\"why\"><span class=\"why-label\">because</span>{because}</div>")
        }
    });
    step(
        "conclusions",
        "Conclusions",
        "What follows. Select one to see the rule and the facts that produced it, back to the observations.",
        "Nothing is derived. The rules found no matching facts.",
        body,
    )
}

fn render_claims(index: &Index) -> String {
    let mut expectations: Vec<&GraphNode> = index
        .projection
        .nodes
        .iter()
        .filter(|node| matches!(node.data, GraphNodeData::Expectation { .. }))
        .collect();
    expectations.sort_by_key(|node| match &node.data {
        GraphNodeData::Expectation { satisfied, .. } => *satisfied,
        _ => true,
    });
    let cards: String = expectations
        .iter()
        .filter_map(|node| {
            let GraphNodeData::Expectation {
                name,
                query,
                expected_count,
                actual_count,
                satisfied,
                doc,
            } = &node.data
            else {
                return None;
            };
            let verdict = if *satisfied {
                format!("Found {actual_count}. Confirmed.")
            } else {
                format!(
                    "Found {actual_count}. This claim is open until the facts or the claim change."
                )
            };
            let (evidence, label) = if *satisfied {
                (index.proven_by.get(node.id.as_str()), "supported by")
            } else {
                (index.matched_by.get(node.id.as_str()), "found instead")
            };
            let proof = evidence
                .map(|facts| {
                    let shown: Vec<String> = facts
                        .iter()
                        .take(8)
                        .map(|fact| format!("<li>{}</li>", index.link(fact)))
                        .collect();
                    let more = if facts.len() > 8 {
                        format!("<li class=\"more\">and {} more</li>", facts.len() - 8)
                    } else {
                        String::new()
                    };
                    format!(
                        "<div class=\"why\"><span class=\"why-label\">{label}</span><ul class=\"because\">{}{more}</ul></div>",
                        shown.join("")
                    )
                })
                .unwrap_or_default();
            Some(card(Card {
                id: &node.id,
                class: "claim",
                attrs: format!(
                    " data-status=\"{}\"",
                    if *satisfied { "passed" } else { "failed" }
                ),
                tone: if *satisfied { "c-stable" } else { "c-critical" },
                chip: if *satisfied { "confirmed" } else { "open" },
                summary: html_escape(name),
                hint: format!("{actual_count} of {expected_count}"),
                detail: format!(
                    "<p class=\"lead\">There must be exactly {} where <span class=\"sentence\">{}</span>.</p><p class=\"verdict\">{verdict}</p>{}{proof}",
                    plural(*expected_count, "result", "results"),
                    index.condition_markup(query),
                    doc_markup(doc.as_deref())
                ),
            }))
        })
        .collect();
    step(
        "claims",
        "Claims",
        "The acceptance criteria this spec commits to, checked against the walk. Open claims come first.",
        "No claims declared. The artifact evaluates but commits to nothing.",
        cards,
    )
}

fn render_stress_tests(index: &Index) -> String {
    let cards: String = index
        .projection
        .nodes
        .iter()
        .filter_map(|node| {
            let GraphNodeData::Mutation {
                name,
                operator,
                relation,
                except,
                must_fail,
                doc,
            } = &node.data
            else {
                return None;
            };
            let target = match operator {
                MutationOperator::DropRule => "Dropping any rule".to_string(),
                MutationOperator::DropCondition => "Dropping any single rule condition".to_string(),
                MutationOperator::DropFact => format!(
                    "Dropping any <code>{}</code> fact",
                    html_escape(relation.as_deref().unwrap_or("?"))
                ),
            };
            let exceptions = if except.is_empty() {
                String::new()
            } else {
                format!(
                    " except {}",
                    except
                        .iter()
                        .map(|item| format!("<code>{}</code>", html_escape(item)))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let oracle = match must_fail {
                Some(expectation) => format!(
                    "must break the claim {}",
                    index
                        .expectations_by_name
                        .get(expectation.as_str())
                        .map(|id| index.link(id))
                        .unwrap_or_else(|| html_escape(expectation))
                ),
                None => "must break at least one claim".to_string(),
            };
            Some(card(Card {
                id: &node.id,
                class: "policy",
                attrs: String::new(),
                tone: "c-attention",
                chip: "stress test",
                summary: html_escape(name),
                hint: html_escape(operator.as_str()),
                detail: format!(
                    "<p class=\"lead\">{target}{exceptions} {oracle}.</p>{}",
                    doc_markup(doc.as_deref())
                ),
            }))
        })
        .collect();
    step(
        "stress-tests",
        "Stress tests",
        "Mutation policies: which parts of the reasoning must be load-bearing. Run stele spec mutate to see which mutants survive.",
        "No mutation policies. Nothing checks that the reasoning is load-bearing.",
        cards,
    )
}

/// Concepts, then the vocabulary they share, grouped by role.
fn render_reference(index: &Index) -> String {
    let mut asserted: BTreeMap<&str, usize> = BTreeMap::new();
    let mut derived: BTreeMap<&str, usize> = BTreeMap::new();
    for node in index.facts() {
        if let GraphNodeData::Fact {
            relation, origin, ..
        } = &node.data
        {
            let counts = if origin == "asserted" {
                &mut asserted
            } else {
                &mut derived
            };
            *counts.entry(relation.as_str()).or_default() += 1;
        }
    }

    let mut concepts = String::new();
    for node in &index.projection.nodes {
        let GraphNodeData::Relation {
            name,
            args,
            roles,
            reads,
            doc,
        } = &node.data
        else {
            continue;
        };
        let signature = if roles.is_empty() {
            format!(
                "{} {}",
                args.len(),
                if args.len() == 1 {
                    "argument"
                } else {
                    "arguments"
                }
            )
        } else {
            roles
                .iter()
                .map(|role| format!("<i>{}</i>", html_escape(role)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut facts = Vec::new();
        if let Some(count) = asserted.get(name.as_str()) {
            facts.push(format!("{count} asserted"));
        }
        if let Some(count) = derived.get(name.as_str()) {
            facts.push(format!("{count} derived"));
        }
        let producers = index
            .derived_by
            .get(node.id.as_str())
            .map(|rules| {
                format!(
                    "concluded by {}",
                    rules
                        .iter()
                        .map(|rule| index.link(rule))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .unwrap_or_else(|| "asserted directly".to_string());
        let reads = reads
            .as_deref()
            .map(|template| {
                format!(
                    "<div class=\"reads\">reads as “{}”</div>",
                    html_escape(template)
                )
            })
            .unwrap_or_default();
        concepts.push_str(&card(Card {
            id: &node.id,
            class: "concept",
            attrs: String::new(),
            tone: "c-neutral",
            chip: "concept",
            summary: html_escape(name),
            hint: facts.join(", "),
            detail: format!(
                "<div class=\"meta\">{signature} · {producers}</div>{reads}{}",
                doc_markup(doc.as_deref())
            ),
        }));
    }

    let notes = index
        .projection
        .nodes
        .iter()
        .find_map(|node| match &node.data {
            GraphNodeData::Spec {
                notes: Some(notes), ..
            } => Some(format!(
                "<details><summary>Maintainer notes</summary>{}</details>",
                paragraphs(notes)
            )),
            _ => None,
        })
        .unwrap_or_default();
    format!(
        "{notes}<h3>Concepts</h3>{concepts}{}",
        render_vocabulary(index)
    )
}

/// Symbols grouped by the role they play. A role name shared by several
/// relations unifies the vocabulary across them.
fn render_vocabulary(index: &Index) -> String {
    let mut by_role: BTreeMap<String, BTreeMap<&str, (usize, &str)>> = BTreeMap::new();
    for edge in &index.projection.edges {
        if edge.rel != "references_symbol" || edge.basis.as_deref() != Some("fact_argument") {
            continue;
        }
        let (Some(fact), Some(symbol), Some(position)) = (
            index.nodes.get(edge.from.as_str()),
            index.nodes.get(edge.to.as_str()),
            edge.position,
        ) else {
            continue;
        };
        let (GraphNodeData::Fact { relation, .. }, GraphNodeData::Symbol { value, .. }) =
            (&fact.data, &symbol.data)
        else {
            continue;
        };
        let role = index
            .relations
            .get(relation.as_str())
            .and_then(|info| info.roles.get(position))
            .cloned()
            .unwrap_or_else(|| format!("{relation} · argument {}", position + 1));
        let entry = by_role
            .entry(role)
            .or_default()
            .entry(value.as_str())
            .or_insert((0, symbol.id.as_str()));
        entry.0 += 1;
    }
    if by_role.is_empty() {
        return String::new();
    }
    let groups: String = by_role
        .into_iter()
        .map(|(role, symbols)| {
            let items: String = symbols
                .iter()
                .map(|(symbol, (uses, id))| {
                    format!(
                        "<li><a class=\"ref\" href=\"#\" data-node=\"{}\"><code>{}</code></a><span class=\"count\">{}</span>{}</li>",
                        html_escape(id),
                        html_escape(&index.symbol_text(symbol)),
                        plural(*uses, "use", "uses"),
                        index.nodes.get(id).and_then(|node| match &node.data {
                            GraphNodeData::Symbol { source: Some(source), .. } => Some(format!(" <span class=\"citations\">{}</span>", source_html(source, index.source_context))),
                            _ => None,
                        }).unwrap_or_default()
                    )
                })
                .collect();
            format!(
                "<details class=\"vocab\"{}><summary><i>{}</i><span class=\"count\">{}</span></summary><ul>{items}</ul></details>",
                if symbols.len() <= 8 { " open" } else { "" },
                html_escape(&role),
                plural(symbols.len(), "value", "values")
            )
        })
        .collect();
    format!("<h3>Vocabulary</h3>{groups}")
}

pub(crate) fn source_html(source: &str, context: Option<&crate::SourceContext>) -> String {
    let escaped = html_escape(source);
    if let Some(destination) = crate::source::source_destination(source, context) {
        format!("<a href=\"{}\">{escaped}</a>", html_escape(&destination))
    } else {
        format!("<span class=\"unsafe-source\">{escaped}</span>")
    }
}

fn brief_fact_html(fact: &BriefFact, context: Option<&crate::SourceContext>) -> String {
    let mut detail = format!(
        "<span class=\"chip\">{}</span> {}",
        html_escape(&fact.standing),
        html_escape(&fact.sentence)
    );
    if !fact.rules.is_empty() {
        detail.push_str(&format!(
            " <span class=\"meta\">Rule: {}</span>",
            fact.rules
                .iter()
                .map(|rule| html_escape(rule))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !fact.sources.is_empty() {
        detail.push_str(&format!(
            " <span class=\"citations\">[{}]</span>",
            fact.sources
                .iter()
                .map(|source| source_html(source, context))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if !fact.provenance.is_empty() {
        detail.push_str(&format!(
            " <span class=\"citations\">Provenance: {}</span>",
            fact.provenance
                .iter()
                .map(|value| html_escape(value))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    for basis in &fact.bases {
        detail.push_str(&format!(
            " <span class=\"basis\">{}: {}{}</span>",
            basis_label(&basis.kind),
            source_html(&basis.source, context),
            basis
                .identity
                .as_ref()
                .map(|identity| format!(" as of {}", html_escape(identity)))
                .unwrap_or_default()
        ));
    }
    detail
}

pub(crate) fn render_brief(brief: &Brief, context: Option<&crate::SourceContext>) -> String {
    let mut html =
        String::from("<section class=\"brief\" id=\"brief\"><header><h2>Answers</h2></header>");
    if let Some(question) = &brief.question {
        html.push_str(&format!(
            "<div class=\"question\">{}</div>",
            paragraphs(question)
        ));
    }
    if brief.answers.is_empty() {
        html.push_str(
            "<p>No expectations are declared. This artifact makes no acceptance claim.</p>",
        );
    }
    for (position, answer) in brief.answers.iter().enumerate() {
        html.push_str(&format!("<article class=\"answer\" data-answer=\"{}\" data-status=\"{}\"><button type=\"button\" class=\"answer-select\" aria-pressed=\"{}\"><span class=\"chip {}\">{}</span> {}</button><p class=\"meta\">{} <span>Acceptance criterion: reviewer-declared.</span></p>", html_escape(&answer.id), if answer.satisfied { "passed" } else { "failed" }, position == 0, if answer.satisfied { "c-stable" } else { "c-critical" }, if answer.satisfied { "confirmed" } else { "failed" }, html_escape(&answer.sentence), html_escape(&answer.counts)));
        html.push_str(&doc_markup(answer.explanation.as_deref()));
        html.push_str("<details class=\"witness\" open><summary>Why this answer holds</summary>");
        if answer.steps.is_empty() {
            html.push_str("<p>No matching witness was produced in this evaluated artifact. A zero result describes this model only.</p>");
        } else {
            html.push_str("<ol>");
            for fact in answer.steps.iter().take(8) {
                html.push_str(&format!("<li>{}</li>", brief_fact_html(fact, context)));
            }
            html.push_str("</ol>");
            if answer.steps.len() > 8 {
                html.push_str(&format!(
                    "<details><summary>{} more steps</summary><ol start=\"9\">",
                    answer.steps.len() - 8
                ));
                for fact in answer.steps.iter().skip(8) {
                    html.push_str(&format!("<li>{}</li>", brief_fact_html(fact, context)));
                }
                html.push_str("</ol></details>");
            }
        }
        html.push_str("</details><details class=\"premises\"><summary>Observations and assumptions</summary><ul>");
        for fact in &answer.premises {
            html.push_str(&format!("<li>{}</li>", brief_fact_html(fact, context)));
        }
        html.push_str("</ul></details></article>");
    }
    html.push_str("</section>");
    html
}
