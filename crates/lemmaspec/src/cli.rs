//! The `stele spec` command line: walk, mutate, check, bind, project and
//! render specifications.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::{
    bind_artifact, check_artifact, mutate_artifact, observation_identity_mismatch,
    project_artifact, render_projection_html_with_context, render_projection_markdown_with_context,
    walk_artifact, MutationTarget, SourceContext,
};

const HELP: &str = "Usage:
  stele spec walk <path.stele> [--json]
  stele spec mutate <path.stele> [--json]
  stele spec check <checker.stele> <evidence.stele> [--json]
  stele spec bind <checker.stele> <evidence.stele> [-o <bound.stele>]
  stele spec project <path.stele> [--json]
  stele spec render <path.stele> [--format html|md] [-o <path>]
  stele spec syntax
  stele spec --version
  stele spec help [syntax]

Commands:
  walk    Parse, validate, and evaluate one self-contained specification
  mutate  Test whether declared mutations are caught by its expectations
  check   Evaluate a checker's rules over another file's facts and expectations
  bind    Write the checker bound to the evidence as one self-contained artifact
  project Emit its closed, deterministic graph projection
  render  Write an answer-first HTML or Markdown report beside the artifact
  syntax  Show the supported artifact and rule language
  help    Show this help or the syntax reference

Options:
  --json  Emit machine-readable JSON for walk, mutate, check, or project
  -o, --output <path>  Choose the render (.html/.md) or bind (.stele) output path
  --format html|md  Choose the report format (default: html)
  --source-root <path>    Resolve repository citations from this explicit directory
  --target-identity <id>  Compare observed report status against this identity
  -V, --version  Show the engine version
  -h, --help

Exit status:
  0  Command succeeded and all expectations passed
  1  Artifact valid, but an expectation or mutation policy failed
  2  Usage, read, parse, validation, or evaluation error

Example:
  stele spec walk examples/release_readiness.stele --json
  stele spec mutate examples/mutation_analysis.stele --json
  stele spec check examples/state_as_records.stele .stele/state_as_records.stele
  stele spec bind examples/state_as_records.stele .stele/state_as_records.stele
  stele spec project examples/release_readiness.stele --json
  stele spec render examples/release_readiness.stele";

const SYNTAX_HELP: &str = r#"Artifact:
  // Comments before `spec` document the artifact: the question it answers.
  spec NAME {
    notes { text: "Maintainer guidance, separate from the reader's question." }
    symbol value { label: "Human-readable value" source: "plan.md#value" }

    // A comment touching a declaration documents that declaration.
    relation NAME {
      args: [symbol, integer]
      roles: [item, score]
      reads: "{item} scored {score}"
    }

    fact ID {
      relation: NAME
      args: [value, 1]
      confidence: 95
      provenance: [source_ref]
    }

    rule ID {
      derive: "result(Item)"
      when: {
        has_input: "input(Item)"
        score_is_sufficient: "Score >= 1"
      }
    }

    expect ID { query: "result(Item)" count: 1 }

    mutation ID {
      operator: drop_condition
      except: [known_equivalent_rule, "rule_id.condition_id"]
      must_fail: EXPECTATION_ID
    }
  }

Artifact rules:
  - symbol labels describe constants used by facts, rules, or expectations;
    symbol keys may be bare identifiers or quoted strings, and source is optional
  - one declaration per symbol; display labels may repeat and never affect proofs
  - notes is an optional singleton block of maintainer guidance
  - source links allow repository-relative paths, fragments, or HTTPS URLs;
    unsafe destinations remain metadata for plain-text display
  - relation argument types are symbol or integer
  - roles is an optional identifier per argument; reads is an optional sentence
    template whose {placeholders} name a role or an argument position
  - comments separated from a declaration by a blank line are section headings
    and document nothing; a comment trailing a closing brace documents that block
  - confidence is an optional integer percentage from 0 through 100
  - provenance is an optional list of symbols or strings; it does not verify a fact
  - optional fact metadata: basis: snapshot|policy|observed|reviewer_declared,
    source: "citation", identity: "content identity" (required for snapshot/observed)
  - basis names a declared producer, not authenticated authorship; tools emit
    snapshot/policy/observed bases; LLM-created claims remain reviewer_declared
  - every predicate used by facts, rules, or expectations needs a relation
  - an asserted relation cannot also be derived by a rule
  - expectations require an exact result count
  - mutation operators are drop_rule, drop_condition, and drop_fact
  - rule when accepts an expression list or a named condition map
  - drop_condition except names rule ids or quoted rule.condition ids
  - drop_fact requires a relation; except names fact ids
  - must_fail names the exact expectation that kills a mutant; omit it for any failure

Logic expressions:
  - variables start with uppercase; `_` is a wildcard
  - symbols may be bare identifiers or quoted strings; integers may be negative
  - bodies support atoms, `!atom(...)`, and <, =<, >, >=, =, \=
  - the right side of a comparison supports integer + and - expressions
  - rule heads support count(X), min(N), and max(N)
  - sum and now are unavailable in self-contained .stele artifacts

Run `stele spec walk FILE --json` to inspect facts and expectations,
`stele spec mutate FILE --json` to test the declared mutation policies, then
`stele spec project FILE --json` to emit the closed graph."#;

/// Run the `stele spec` command line on `args`, the words after `stele spec`,
/// reporting an error as `stele spec: …` with exit status 2.
pub fn dispatch(args: Vec<String>) -> ExitCode {
    match run(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("stele spec: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<ExitCode, String> {
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("lemmaspec {}", crate::VERSION);
        return Ok(ExitCode::SUCCESS);
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{HELP}");
        return Ok(ExitCode::SUCCESS);
    }
    if args.is_empty() {
        eprintln!("{HELP}");
        return Ok(ExitCode::from(2));
    }
    match args[0].as_str() {
        "walk" => run_walk(&args[1..]),
        "mutate" => run_mutate(&args[1..]),
        "check" => run_check(&args[1..]),
        "bind" => run_bind(&args[1..]),
        "project" => run_project(&args[1..]),
        "render" => run_render(&args[1..]),
        "syntax" => print_syntax(&args[1..]),
        "help" => print_help(&args[1..]),
        command => Err(format!("unknown command `{command}`\n\n{HELP}")),
    }
}

fn print_help(args: &[String]) -> Result<ExitCode, String> {
    match args {
        [] => {
            println!("{HELP}");
            Ok(ExitCode::SUCCESS)
        }
        [topic] if topic == "syntax" => print_syntax(&[]),
        [topic] => Err(format!("unknown help topic `{topic}`\n\n{HELP}")),
        _ => Err(format!("help accepts at most one topic\n\n{HELP}")),
    }
}

fn print_syntax(args: &[String]) -> Result<ExitCode, String> {
    if let Some(unexpected) = args.first() {
        return Err(format!("unexpected argument `{unexpected}` for syntax"));
    }
    println!("{SYNTAX_HELP}");
    Ok(ExitCode::SUCCESS)
}

/// Whether `path` names a specification: `.stele`, or `.lemmaspec` from before
/// the engine joined Stele.
fn is_spec(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("stele" | "lemmaspec")
    )
}

fn artifact_path<'a>(args: &'a [String], command: &str) -> Result<&'a str, String> {
    let Some(path) = args.first() else {
        return Err(format!("{command} requires a .stele path\n\n{HELP}"));
    };
    if let Some(unexpected) = args[1..].iter().find(|arg| arg.as_str() != "--json") {
        return Err(format!("unexpected argument `{unexpected}`"));
    }
    if !is_spec(Path::new(path)) {
        return Err(format!("{command} expects a .stele file, got `{path}`"));
    }
    Ok(path)
}

fn run_walk(args: &[String]) -> Result<ExitCode, String> {
    let path = artifact_path(args, "walk")?;

    let source =
        std::fs::read_to_string(path).map_err(|error| format!("read `{path}`: {error}"))?;
    let report = walk_artifact(&source).map_err(|error| format!("walk `{path}`: {error}"))?;
    report_walk(&report, args.iter().any(|arg| arg == "--json"))
}

fn run_check(args: &[String]) -> Result<ExitCode, String> {
    let (paths, flags): (Vec<&String>, Vec<&String>) =
        args.iter().partition(|arg| !arg.starts_with("--"));
    let [checker, evidence] = paths[..] else {
        return Err(format!(
            "check requires a checker and an evidence .stele path\n\n{HELP}"
        ));
    };
    if let Some(unexpected) = flags.iter().find(|flag| flag.as_str() != "--json") {
        return Err(format!("unexpected argument `{unexpected}`"));
    }
    for path in [checker, evidence] {
        if !is_spec(Path::new(path)) {
            return Err(format!("check expects .stele files, got `{path}`"));
        }
    }

    let read = |path: &String| {
        std::fs::read_to_string(path).map_err(|error| format!("read `{path}`: {error}"))
    };
    let report = check_artifact(&read(checker)?, &read(evidence)?)
        .map_err(|error| format!("check `{checker}` over `{evidence}`: {error}"))?;
    report_walk(&report, !flags.is_empty())
}

fn run_bind(args: &[String]) -> Result<ExitCode, String> {
    let [checker, evidence, options @ ..] = args else {
        return Err(format!(
            "bind requires a checker and an evidence .stele path\n\n{HELP}"
        ));
    };
    for path in [checker, evidence] {
        if !is_spec(Path::new(path)) {
            return Err(format!("bind expects .stele files, got `{path}`"));
        }
    }
    let mut output = None;
    let mut index = 0;
    while index < options.len() {
        match options[index].as_str() {
            "-o" | "--output" => {
                if output.is_some() {
                    return Err("bind accepts only one output path".to_string());
                }
                let Some(value) = options.get(index + 1) else {
                    return Err(format!("{} requires a path", options[index]));
                };
                output = Some(PathBuf::from(value));
                index += 2;
            }
            unexpected => return Err(format!("unexpected argument `{unexpected}`")),
        }
    }
    let output = output.unwrap_or_else(|| Path::new(evidence).with_extension("bound.stele"));
    if !is_spec(&output) {
        return Err(format!(
            "bind output must be a .stele file, got `{}`",
            output.display()
        ));
    }
    if [checker, evidence]
        .iter()
        .any(|path| same_file(Path::new(path), &output))
    {
        return Err(format!(
            "bind refuses to overwrite its input `{}`",
            output.display()
        ));
    }

    let read = |path: &String| {
        std::fs::read_to_string(path).map_err(|error| format!("read `{path}`: {error}"))
    };
    let bound = bind_artifact(&read(checker)?, &read(evidence)?)
        .map_err(|error| format!("bind `{checker}` over `{evidence}`: {error}"))?;
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create output directory `{}`: {error}", parent.display()))?;
    }
    std::fs::write(&output, bound)
        .map_err(|error| format!("write `{}`: {error}", output.display()))?;
    println!("wrote {}", output.display());
    Ok(ExitCode::SUCCESS)
}

/// Whether two paths name the same file, by canonical path when both exist
/// and by lexical comparison otherwise.
fn same_file(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// Print a walk or check report, as text or JSON, and return its exit status:
/// 0 when it is clean, 1 when an expectation is open.
pub fn report_walk(report: &crate::WalkReport, json: bool) -> Result<ExitCode, String> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(report)
                .map_err(|error| format!("serialize walk report: {error}"))?
        );
    } else {
        println!(
            "{}: {} ({} asserted, {} derived)",
            report.spec, report.status, report.asserted, report.derived
        );
        for expectation in &report.expectations {
            let mark = if expectation.satisfied { "ok" } else { "FAIL" };
            println!(
                "  [{mark}] {}: {} (expected {}, found {})",
                expectation.id,
                expectation.query,
                expectation.expected_count,
                expectation.actual_count
            );
            if !expectation.satisfied {
                for fact in &expectation.found {
                    println!("         {fact}");
                }
            }
        }
    }

    Ok(if report.status == "clean" {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn run_mutate(args: &[String]) -> Result<ExitCode, String> {
    let path = artifact_path(args, "mutate")?;
    let source =
        std::fs::read_to_string(path).map_err(|error| format!("read `{path}`: {error}"))?;
    let report = mutate_artifact(&source).map_err(|error| format!("mutate `{path}`: {error}"))?;

    if args.iter().any(|arg| arg == "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("serialize mutation report: {error}"))?
        );
    } else {
        println!(
            "{}: {} ({} killed, {} survived, {} rejected, {} excluded)",
            report.spec,
            report.status,
            report.summary.killed,
            report.summary.survived,
            report.summary.rejected,
            report.summary.excluded
        );
        for failure in &report.baseline_failures {
            println!(
                "  [baseline FAIL] {}: {} (expected {}, found {})",
                failure.id, failure.query, failure.expected_count, failure.actual_count
            );
        }
        for policy in &report.policies {
            println!(
                "  [policy {:<8}] {} ({} executed, {} rejected, {} excluded)",
                policy.status,
                policy.id,
                policy.summary.executed,
                policy.summary.rejected,
                policy.summary.excluded
            );
        }
        for mutation in &report.mutations {
            let target = match &mutation.target {
                MutationTarget::Rule { rule } => format!("rule {rule}"),
                MutationTarget::Condition {
                    rule,
                    condition,
                    index,
                    expression,
                } => condition.as_deref().map_or_else(
                    || format!("condition {index} of rule {rule}: {expression}"),
                    |condition| format!("condition {condition} of rule {rule}: {expression}"),
                ),
                MutationTarget::Fact { fact, relation } => {
                    format!("fact {fact} of relation {relation}")
                }
            };
            let failures = mutation
                .failed_expectations
                .iter()
                .map(|expectation| expectation.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let detail = if let Some(diagnostic) = &mutation.diagnostic {
                format!(" ({diagnostic})")
            } else if failures.is_empty() {
                String::new()
            } else {
                format!(" (failed: {failures})")
            };
            println!(
                "  [{:<8}] {} -> {}{}",
                mutation.status.as_str(),
                mutation.policy,
                target,
                detail
            );
        }
    }

    Ok(if report.status == "clean" {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn run_project(args: &[String]) -> Result<ExitCode, String> {
    let path = artifact_path(args, "project")?;
    let source =
        std::fs::read_to_string(path).map_err(|error| format!("read `{path}`: {error}"))?;
    let projection =
        project_artifact(&source).map_err(|error| format!("project `{path}`: {error}"))?;

    if args.iter().any(|arg| arg == "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&projection)
                .map_err(|error| format!("serialize graph projection: {error}"))?
        );
    } else {
        println!(
            "{}: {} ({} nodes, {} edges)",
            projection.spec,
            projection.status,
            projection.nodes.len(),
            projection.edges.len()
        );
    }

    Ok(if projection.status == "clean" {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn run_render(args: &[String]) -> Result<ExitCode, String> {
    let RenderPaths {
        path,
        output: output_path,
        format,
        target,
        source_root,
    } = render_paths(args)?;
    let source =
        std::fs::read_to_string(path).map_err(|error| format!("read `{path}`: {error}"))?;
    let projection =
        project_artifact(&source).map_err(|error| format!("render `{path}`: {error}"))?;
    let output_directory = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(output_directory).map_err(|error| {
        format!(
            "create output directory `{}`: {error}",
            output_directory.display()
        )
    })?;
    let context = source_root
        .map(|root| SourceContext::new(Path::new(root), output_directory))
        .transpose()?;
    let html = if format == "md" {
        render_projection_markdown_with_context(&projection, target, context.as_ref())
    } else {
        render_projection_html_with_context(&source, &projection, target, context.as_ref())
    };

    std::fs::write(&output_path, html)
        .map_err(|error| format!("write `{}`: {error}", output_path.display()))?;
    println!(
        "wrote {} ({}; {} nodes, {} edges)",
        output_path.display(),
        projection.status,
        projection.nodes.len(),
        projection.edges.len()
    );

    Ok(
        if projection.status == "clean"
            && !target.is_some_and(|target| observation_identity_mismatch(&projection, target))
        {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        },
    )
}

struct RenderPaths<'a> {
    path: &'a str,
    output: PathBuf,
    format: &'a str,
    target: Option<&'a str>,
    source_root: Option<&'a str>,
}

fn render_paths(args: &[String]) -> Result<RenderPaths<'_>, String> {
    let Some(path) = args.first() else {
        return Err(format!("render requires a .stele path\n\n{HELP}"));
    };
    if !is_spec(Path::new(path)) {
        return Err(format!("render expects a .stele file, got `{path}`"));
    }

    let mut output = None;
    let mut format = "html";
    let mut target = None;
    let mut source_root = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source-root" => {
                if source_root.is_some() {
                    return Err("render accepts only one source root".to_string());
                }
                let Some(value) = args.get(index + 1) else {
                    return Err("--source-root requires a path".to_string());
                };
                source_root = Some(value.as_str());
                index += 2;
            }
            "--target-identity" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("--target-identity requires an identity".to_string());
                };
                target = Some(value.as_str());
                index += 2;
            }
            "--format" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("--format requires html or md".to_string());
                };
                if !matches!(value.as_str(), "html" | "md") {
                    return Err(format!("unsupported render format `{value}`"));
                }
                format = value;
                index += 2;
            }
            "-o" | "--output" => {
                if output.is_some() {
                    return Err("render accepts only one output path".to_string());
                }
                let Some(value) = args.get(index + 1) else {
                    return Err(format!("{} requires a path", args[index]));
                };
                output = Some(PathBuf::from(value));
                index += 2;
            }
            unexpected => return Err(format!("unexpected argument `{unexpected}`")),
        }
    }
    let output = output.unwrap_or_else(|| Path::new(path).with_extension(format));
    if output.extension().and_then(|value| value.to_str()) != Some(format) {
        return Err(format!(
            "render output must be an .{format} file, got `{}`",
            output.display()
        ));
    }
    Ok(RenderPaths {
        path,
        output,
        format,
        target,
        source_root,
    })
}
