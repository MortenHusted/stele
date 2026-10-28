//! Self-contained human view of one deterministic graph projection.

use std::collections::BTreeMap;

use crate::artifact::ValueType;
use crate::guide::{prepare_html_guide, render_brief, GuideSections, PreparedHtmlGuide};
use crate::projection::{GraphNodeData, GraphProjection};

const TEMPLATE: &str = include_str!("html_template.html");

pub fn render_projection_html(source: &str, projection: &GraphProjection) -> String {
    render_projection_html_with_target(source, projection, None)
}

pub fn render_projection_html_with_target(
    source: &str,
    projection: &GraphProjection,
    target: Option<&str>,
) -> String {
    render_projection_html_with_context(source, projection, target, None)
}

/// Render with an explicit source root and report directory for citations.
/// Without a context, authored destinations are preserved verbatim.
pub fn render_projection_html_with_context(
    source: &str,
    projection: &GraphProjection,
    target: Option<&str>,
    context: Option<&crate::SourceContext>,
) -> String {
    let title = humanize(&projection.spec);
    let (expectation_count, failed) = projection
        .nodes
        .iter()
        .filter_map(|node| match &node.data {
            GraphNodeData::Expectation { satisfied, .. } => Some(*satisfied),
            _ => None,
        })
        .fold((0, 0), |(total, failed), satisfied| {
            (total + 1, failed + usize::from(!satisfied))
        });
    let (thesis, thesis_detail) = expectation_summary(expectation_count, failed);
    let summary = render_summary(projection, failed);
    let PreparedHtmlGuide {
        sections:
            GuideSections {
                question,
                observations,
                assumptions,
                reasoning,
                conclusions,
                claims,
                stress_tests,
                reference,
            },
        brief,
        labels,
    } = prepare_html_guide(projection, context);
    let relations = render_relations(projection);
    let facts = render_facts(projection, &labels);
    let graph_json = escape_json_for_script(
        &serde_json::to_string(projection).expect("graph projection is serializable"),
    );
    let source = html_escape(source);
    let status_label = if failed == 0 { "clean" } else { "incomplete" };

    render_template(&BTreeMap::from([
        ("FRESHNESS", target.filter(|target| crate::guide::observation_identity_mismatch(projection, target)).map(|target| format!("<div class=\"freshness\" role=\"status\">Not current for target {}. Observed status was recorded against a different identity.</div>", html_escape(target))).unwrap_or_default()),
        ("BRIEF", render_brief(&brief, context)),
        ("DISPLAY_LABELS", escape_json_for_script(&serde_json::to_string(&labels).expect("labels serialize"))),
        ("ANSWER_CLOSURES", escape_json_for_script(&serde_json::to_string(&brief.answers.iter().map(|answer| (&answer.id, &answer.closure)).collect::<BTreeMap<_, _>>()).expect("closures serialize"))),
        ("ASSUMPTIONS", assumptions),
        ("CLAIMS", claims),
        ("CONCLUSIONS", conclusions),
        ("FACTS", facts),
        ("GRAPH_JSON", graph_json),
        ("OBSERVATIONS", observations),
        ("QUESTION", question),
        ("REASONING", reasoning),
        ("REFERENCE", reference),
        ("RELATIONS", relations),
        ("STRESS_TESTS", stress_tests),
        ("SOURCE", source),
        ("SPEC_NAME", html_escape(&projection.spec)),
        ("STATUS_LABEL", html_escape(status_label)),
        ("SUMMARY", summary),
        ("THESIS", html_escape(&thesis)),
        ("THESIS_DETAIL", html_escape(&thesis_detail)),
        ("TITLE", html_escape(&title)),
    ]))
}

fn expectation_summary(total: usize, failed: usize) -> (String, String) {
    if total == 0 {
        return (
            "No expectations are declared".to_string(),
            "The facts and rules are valid, but this artifact has no executable acceptance claim."
                .to_string(),
        );
    }
    if failed == 0 {
        return (
            format!(
                "All {total} {} {} satisfied",
                plural(total, "expectation", "expectations"),
                if total == 1 { "is" } else { "are" }
            ),
            "Every declared exact-count claim matches the deterministic walk.".to_string(),
        );
    }
    (
        format!(
            "{failed} {} needs attention",
            plural(failed, "expectation", "expectations")
        ),
        format!(
            "{} of {total} executable constraints matched; failed expectations remain visible as unresolved work.",
            total - failed
        ),
    )
}

fn plural<'a>(count: usize, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 {
        singular
    } else {
        plural
    }
}

fn render_summary(projection: &GraphProjection, failed: usize) -> String {
    let mut counts = BTreeMap::<&str, usize>::new();
    let mut asserted = 0;
    let mut derived = 0;
    for node in &projection.nodes {
        *counts.entry(node.node_type()).or_default() += 1;
        if let GraphNodeData::Fact { origin, .. } = &node.data {
            if origin == "asserted" {
                asserted += 1;
            } else {
                derived += 1;
            }
        }
    }
    let mut cards = vec![
        ("relations", counts.get("relation").copied().unwrap_or(0)),
        ("asserted", asserted),
        ("derived", derived),
        ("rules", counts.get("rule").copied().unwrap_or(0)),
        (
            "expectations",
            counts.get("expectation").copied().unwrap_or(0),
        ),
        ("failed", failed),
        ("nodes", projection.nodes.len()),
        ("edges", projection.edges.len()),
    ];
    if let Some(mutations) = counts.get("mutation").copied().filter(|count| *count > 0) {
        cards.insert(4, ("mutations", mutations));
    }
    cards
        .into_iter()
        .map(|(label, value)| {
            format!(r#"<div class="stat"><strong>{value}</strong><span>{label}</span></div>"#)
        })
        .collect()
}

fn render_relations(projection: &GraphProjection) -> String {
    let mut rows = String::new();
    for node in &projection.nodes {
        if let GraphNodeData::Relation { name, args, .. } = &node.data {
            let signature = args.iter().map(value_type).collect::<Vec<_>>().join(", ");
            rows.push_str(&format!(
                "<tr><td><code>{}</code></td><td><code>({})</code></td></tr>",
                html_escape(name),
                html_escape(&signature)
            ));
        }
    }
    if rows.is_empty() {
        "<p class=\"empty\">No relations declared.</p>".to_string()
    } else {
        format!(
            "<table><thead><tr><th>Relation</th><th>Argument types</th></tr></thead><tbody>{rows}</tbody></table>"
        )
    }
}

fn render_facts(projection: &GraphProjection, labels: &BTreeMap<String, String>) -> String {
    let mut rows = String::new();
    for node in &projection.nodes {
        if let GraphNodeData::Fact {
            origin,
            confidence,
            provenance,
            declarations,
            ..
        } = &node.data
        {
            let tone = if origin == "asserted" {
                "c-stable"
            } else {
                "c-additive"
            };
            let evidence = if provenance.is_empty() {
                "—".to_string()
            } else {
                provenance.join(", ")
            };
            let declarations = if declarations.is_empty() {
                "—".to_string()
            } else {
                declarations.join(", ")
            };
            rows.push_str(&format!(
                r#"<tr><td><span class="chip {tone}">{}</span></td><td><span>{}</span></td><td>{:.0}%</td><td>{}</td><td>{}</td></tr>"#,
                html_escape(origin),
                html_escape(&labels[&node.id]),
                confidence * 100.0,
                html_escape(&evidence),
                html_escape(&declarations)
            ));
        }
    }
    if rows.is_empty() {
        "<p class=\"empty\">No materialized facts.</p>".to_string()
    } else {
        format!(
            "<div class=\"table-scroll\"><table><thead><tr><th>Origin</th><th>Fact</th><th>Confidence</th><th>Provenance</th><th>Declarations</th></tr></thead><tbody>{rows}</tbody></table></div>"
        )
    }
}

fn value_type(value: &ValueType) -> &'static str {
    match value {
        ValueType::Symbol => "symbol",
        ValueType::Integer => "integer",
    }
}

fn humanize(value: &str) -> String {
    let mut result = value.replace(['_', '-'], " ");
    if let Some(first) = result.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    result
}

pub(crate) fn html_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn escape_json_for_script(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("\\u0026"),
            '<' => escaped.push_str("\\u003c"),
            '>' => escaped.push_str("\\u003e"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn render_template(values: &BTreeMap<&str, String>) -> String {
    let mut rendered = String::with_capacity(TEMPLATE.len() + 4096);
    let mut remaining = TEMPLATE;
    while let Some(start) = remaining.find("@@") {
        rendered.push_str(&remaining[..start]);
        let token_start = start + 2;
        let Some(relative_end) = remaining[token_start..].find("@@") else {
            rendered.push_str(&remaining[start..]);
            return rendered;
        };
        let token_end = token_start + relative_end;
        let token = &remaining[token_start..token_end];
        rendered.push_str(
            values
                .get(token)
                .unwrap_or_else(|| panic!("missing HTML template value `{token}`")),
        );
        remaining = &remaining[token_end + 2..];
    }
    rendered.push_str(remaining);
    rendered
}
