//! `stele intro` and `stele agent install`: what an agent needs from the
//! binary itself. An orientation it can read before starting, and the skill
//! this binary was built with, installed next to the project it works in so
//! skill and CLI never drift.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// A skill this binary was built with.
struct Skill {
    /// The skill's directory name.
    name: &'static str,
    /// Each file by its path inside the skill directory.
    files: &'static [(&'static str, &'static str)],
    /// Codex's display metadata, written to `agents/openai.yaml`.
    openai: &'static str,
}

const SKILLS: [Skill; 2] = [
    Skill {
        name: "stele",
        files: &[
            (
                "SKILL.md",
                include_str!("../../../plugins/stele/skills/stele/SKILL.md"),
            ),
            (
                "references/catalog.md",
                include_str!("../../../plugins/stele/skills/stele/references/catalog.md"),
            ),
            (
                "references/checks.md",
                include_str!("../../../plugins/stele/skills/stele/references/checks.md"),
            ),
            (
                "references/code.md",
                include_str!("../../../plugins/stele/skills/stele/references/code.md"),
            ),
            (
                "references/plans.md",
                include_str!("../../../plugins/stele/skills/stele/references/plans.md"),
            ),
            (
                "references/specs.md",
                include_str!("../../../plugins/stele/skills/stele/references/specs.md"),
            ),
        ],
        openai: include_str!("../../../plugins/stele/skills/stele/agents/openai.yaml"),
    },
    Skill {
        name: "stele-onboard",
        files: &[(
            "SKILL.md",
            include_str!("../../../plugins/stele/skills/stele-onboard/SKILL.md"),
        )],
        openai: include_str!("../../../plugins/stele/skills/stele-onboard/agents/openai.yaml"),
    },
];

const INTRO: &str = r#"Stele, for an agent

What it is
  Stele extracts deterministic facts from source code and evaluates typed
  specifications against them. The same commit and the same .stele file
  always give the same facts, the same verdict, and the same graph.

Reach for it when
  - a question about code can be settled from its facts: what calls what,
    what a change reaches, which symbols nothing reaches, where complexity
    sits in the request layer;
  - a claim about code, a plan, or a system must survive you: written as
    facts, rules, and exact-count expectations that another agent or a
    human can audit, challenge, and re-run;
  - a change should be measured rather than judged: did it make the code
    harder to maintain.
  Do not reach for it for mutable runtime state, persistence, or clocks.

Check code against a shipped checker
  stele check architecture|symbols|hotspots|citations --path DIR
  stele check symbols --snapshot FILE   reuse a snapshot, or check C++,
                                        JS/TS, Python, or a dirty tree
  Architecture and symbols need verified calls; without the language's
  resolver they exit 2 rather than guess.

Ask the code
  stele code extract --path DIR --out snapshot.json
  stele code context|impact|trace|search|structure ... --file snapshot.json
  stele code diff BASE HEAD             what changed structurally
  stele slop diff --base A --head B     what became harder to maintain
  hotspots, refactor, clusters, and processes are signals, not findings.

Plan a change
  intent/catalog.stele                what the system is for: intents,
                                      definitions, exceptions, channels
  plans/NAME.stele                    decisions and ordered changes that
                                      serve them, anchored in the code
  stele plan check plans/NAME.stele   broken links and missing code, then
                                      which changes are ready

Back the catalog with evidence
  stele intent record                  run the tagged examples: red on a
                                       deliberate break, then green
  stele intent approve IDS --by NAME   record a human's approval
  stele intent report                  each statement's standing, what
                                       awaits approval, and the code map
  stele intent impact BASE             the statements a change since BASE
                                       touches, and their examples
  New to a repository? The stele-onboard skill backfills the catalog for
  a human to approve.

Write a specification
  1. stele spec syntax                the grammar; never guess it
  2. author the artifact              one question per file, from verified
                                      observations only
  3. stele spec walk FILE --json      0 clean, 1 an expectation is open,
                                      2 the file itself is wrong
  4. stele spec mutate FILE --json    when policies are declared; a survivor
                                      is a specification gap
  5. stele spec project FILE --json   the closed graph for other tools
  6. stele spec render FILE           the answer-first page a human reads;
                                      --format md writes Markdown

Evidence discipline
  - Facts may declare basis: snapshot, policy, observed, or reviewer_declared.
    Each basis requires source; snapshot and observed also require identity.
    These fields describe a producer's claim, not authenticated authorship.
  - Untyped facts remain reviewer-declared assumptions, even with provenance
    or full confidence. Neither citations nor confidence upgrade a claim.
    LLM-authored claims stay reviewer_declared; generated observations must
    come from the named deterministic tool.
  - Never promote a guess to a fact to make an expectation pass.
  - An open expectation is a finding. Report it; do not change the count.

Write for the human who will read the render
  - Comments before `spec` state the question. A comment directly above a
    relation, fact, rule, expectation, or mutation explains it.
    Keep maintainer guidance in notes { text: "..." }, outside the question.
  - Give relations `roles` and a `reads` template so facts and rule
    conditions render as sentences: "{item} depends on {dependency}".
    Give symbols labels and safe source links so readers can follow them.
  - Hand over the answers first, with failed expectations and their numbered
    witnesses. The graph starts on the selected answer's witness; the full
    graph and walkthrough are opt-in. Press ? for the guide.

Install the skill next to the project
  stele agent install                .claude/skills and .codex/skills,
                                     matching this binary
  stele agent install --marketplace  through the agents' own plugin
                                     marketplaces instead"#;

const MARKETPLACE: &str = "MortenHusted/stele";
const PLUGIN: &str = "stele@stele";

#[derive(clap::Subcommand, Debug)]
pub enum AgentCommand {
    /// Write this binary's skills into a project's .claude and .codex skill
    /// directories, or install it through the agents' plugin marketplaces.
    Install(InstallArgs),
}

#[derive(clap::Args, Debug)]
pub struct InstallArgs {
    /// Install for Claude Code (default: Claude Code and Codex).
    #[arg(long)]
    claude: bool,
    /// Install for Codex (default: Claude Code and Codex).
    #[arg(long)]
    codex: bool,
    /// Project to install into.
    #[arg(long, default_value = ".")]
    dir: PathBuf,
    /// Run the agents' plugin marketplace commands instead.
    #[arg(long)]
    marketplace: bool,
}

/// Print the orientation an agent reads before starting.
pub fn print_intro() -> ExitCode {
    println!("{INTRO}");
    ExitCode::SUCCESS
}

/// Run one agent command, reporting an error as `stele agent: …` with exit
/// status 2.
pub fn dispatch(command: AgentCommand) -> ExitCode {
    let AgentCommand::Install(args) = command;
    match install(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("stele agent: {error}");
            ExitCode::from(2)
        }
    }
}

fn install(args: &InstallArgs) -> Result<(), String> {
    let mut agents = Vec::new();
    if args.claude {
        agents.push(Agent::Claude);
    }
    if args.codex {
        agents.push(Agent::Codex);
    }
    if agents.is_empty() {
        agents.extend(Agent::ALL);
    }
    if args.marketplace {
        for line in install_marketplace(&agents)? {
            println!("{line}");
        }
        return Ok(());
    }
    if !args.dir.is_dir() {
        return Err(format!("`{}` is not a directory", args.dir.display()));
    }
    for installed in install_project_skill(&args.dir, &agents)? {
        for (file, change) in installed.files {
            println!("{}: {change} {}", installed.agent, file.display());
        }
    }
    println!("skill version matches stele {}", env!("CARGO_PKG_VERSION"));
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    const fn skill_root(self) -> &'static str {
        match self {
            Self::Claude => ".claude/skills",
            Self::Codex => ".codex/skills",
        }
    }

    const fn cli(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// The commands the README documents for a marketplace install.
    fn marketplace_steps(self) -> [Vec<&'static str>; 2] {
        match self {
            Self::Claude => [
                vec!["plugin", "marketplace", "add", MARKETPLACE],
                vec!["plugin", "install", PLUGIN],
            ],
            Self::Codex => [
                vec!["plugin", "marketplace", "add", MARKETPLACE],
                vec!["plugin", "add", PLUGIN],
            ],
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Installed {
    pub agent: Agent,
    pub files: Vec<(PathBuf, Change)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Created,
    Updated,
    Unchanged,
}

impl fmt::Display for Change {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Created => "wrote",
            Self::Updated => "updated",
            Self::Unchanged => "unchanged",
        })
    }
}

/// Write the skills this binary was built with into a project's own skill
/// directories. Repeat runs rewrite the same bytes, so the result is stable.
pub fn install_project_skill(project: &Path, agents: &[Agent]) -> Result<Vec<Installed>, String> {
    let mut installed = Vec::new();
    for agent in agents {
        let mut files = Vec::new();
        for skill in &SKILLS {
            let root = project.join(agent.skill_root()).join(skill.name);
            for (path, content) in skill.files {
                files.push(write(&root.join(path), content)?);
            }
            if *agent == Agent::Codex {
                files.push(write(&root.join("agents/openai.yaml"), skill.openai)?);
            }
        }
        installed.push(Installed {
            agent: *agent,
            files,
        });
    }
    Ok(installed)
}

fn write(path: &Path, content: &str) -> Result<(PathBuf, Change), String> {
    let change = match fs::read_to_string(path) {
        Ok(existing) if existing == content => return Ok((path.to_path_buf(), Change::Unchanged)),
        Ok(_) => Change::Updated,
        Err(_) => Change::Created,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    fs::write(path, content).map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok((path.to_path_buf(), change))
}

/// Install through the agents' plugin marketplaces by running their CLIs.
/// Returns one report line per agent; a missing CLI is reported, not fatal,
/// unless that agent was the only one asked for.
pub fn install_marketplace(agents: &[Agent]) -> Result<Vec<String>, String> {
    let mut report = Vec::new();
    let mut succeeded = 0;
    for agent in agents {
        let cli = agent.cli();
        let available = Command::new(cli)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success());
        if !available {
            report.push(format!("{agent}: `{cli}` is not on PATH, skipped"));
            continue;
        }
        let mut failed = None;
        for step in agent.marketplace_steps() {
            let status = Command::new(cli)
                .args(&step)
                .status()
                .map_err(|error| format!("run `{cli} {}`: {error}", step.join(" ")))?;
            if !status.success() {
                failed = Some(step.join(" "));
                break;
            }
        }
        if let Some(step) = failed {
            report.push(format!("{agent}: `{cli} {step}` failed"));
        } else {
            succeeded += 1;
            report.push(format!("{agent}: plugin {PLUGIN} installed"));
        }
    }
    if succeeded == 0 {
        return Err(format!(
            "no agent CLI completed the marketplace install\n{}",
            report.join("\n")
        ));
    }
    Ok(report)
}
