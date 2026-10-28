//! Deterministic answer-first reading for terminals, reviews and diffs.

use crate::guide::{basis_label, brief, observation_identity_mismatch, BriefFact};
use crate::GraphProjection;

pub fn render_projection_markdown(projection: &GraphProjection) -> String {
    render_projection_markdown_with_target(projection, None)
}

pub fn render_projection_markdown_with_target(
    projection: &GraphProjection,
    target: Option<&str>,
) -> String {
    render_projection_markdown_with_context(projection, target, None)
}

/// Render with an explicit source root and report directory for citations.
/// Without a context, authored destinations are preserved verbatim.
pub fn render_projection_markdown_with_context(
    projection: &GraphProjection,
    target: Option<&str>,
    context: Option<&crate::SourceContext>,
) -> String {
    let brief = brief(projection);
    let mut out = format!(
        "# {}\n\nStatus: {}\n\n",
        escape(&projection.spec),
        escape(&projection.status)
    );
    if let Some(target) = target.filter(|target| observation_identity_mismatch(projection, target))
    {
        out = format!("> **Not current for target {}.** Observed status was recorded against a different identity.\n\n{out}", escape(target));
    }
    if let Some(question) = brief.question {
        out.push_str(&format!("{}\n\n", escape(&question)));
    }
    out.push_str("## Answers\n\n");
    if brief.answers.is_empty() {
        out.push_str("No expectations are declared. This artifact makes no acceptance claim.\n");
    }
    for answer in brief.answers {
        out.push_str(&format!(
            "### {} — {}\n\n{} Acceptance criterion: reviewer-declared.\n\n",
            if answer.satisfied {
                "Confirmed"
            } else {
                "Failed"
            },
            escape(&answer.sentence),
            answer.counts
        ));
        if let Some(explanation) = answer.explanation {
            out.push_str(&format!("{}\n\n", escape(&explanation)));
        }
        out.push_str("<details open>\n<summary>Why this answer holds</summary>\n\n");
        if answer.steps.is_empty() {
            out.push_str("No matching witness was produced in this evaluated artifact. A zero result describes this model only.\n\n");
        }
        for (index, fact) in answer.steps.iter().take(8).enumerate() {
            out.push_str(&format!("{}. {}\n", index + 1, fact_text(fact, context)));
        }
        if answer.steps.len() > 8 {
            out.push_str(&format!(
                "\n<details>\n<summary>{} more steps</summary>\n\n",
                answer.steps.len() - 8
            ));
            for (index, fact) in answer.steps.iter().enumerate().skip(8) {
                out.push_str(&format!("{}. {}\n", index + 1, fact_text(fact, context)));
            }
            out.push_str("\n</details>\n");
        }
        out.push_str("\n</details>\n\n#### Observations and assumptions\n\n");
        if answer.premises.is_empty() {
            out.push_str("No positive premises in this witness.\n");
        }
        for fact in &answer.premises {
            out.push_str(&format!("- {}\n", fact_text(fact, context)));
        }
        out.push('\n');
    }
    out
}

fn fact_text(fact: &BriefFact, context: Option<&crate::SourceContext>) -> String {
    let mut text = format!("**{}:** {}", fact.standing, escape(&fact.sentence));
    if !fact.rules.is_empty() {
        text.push_str(&format!(
            " (rule: {})",
            fact.rules
                .iter()
                .map(|rule| escape(rule))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for source in &fact.sources {
        text.push(' ');
        text.push_str(&citation(source, context));
    }
    for provenance in &fact.provenance {
        text.push_str(&format!(" Provenance: {}", literal_citation(provenance)));
    }
    for basis in &fact.bases {
        text.push_str(&format!(
            " — {}: {}",
            basis_label(&basis.kind),
            citation(&basis.source, context)
        ));
        if let Some(identity) = &basis.identity {
            text.push_str(&format!(" as of {}", escape(identity)));
        }
    }
    text
}

fn citation(source: &str, context: Option<&crate::SourceContext>) -> String {
    if let Some(resolved) = crate::source::source_destination(source, context) {
        format!("[{}](<{}>)", escape(source), destination(&resolved))
    } else {
        literal_citation(source)
    }
}

fn literal_citation(source: &str) -> String {
    // A code span also prevents Markdown's automatic bare-URL linking.
    let delimiter = "`".repeat(source.split(|c| c != '`').map(str::len).max().unwrap_or(0) + 1);
    format!(
        "{delimiter} {} {delimiter}",
        source.replace(['\n', '\r'], " ")
    )
}

fn destination(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "%3C")
        .replace('>', "%3E")
        .replace('"', "%22")
        .replace(' ', "%20")
}

fn escape(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.'
            | '!' | '|' => {
                out.push('\\');
                out.push(c);
            }
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}
