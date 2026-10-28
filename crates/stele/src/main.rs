//! The `stele` command line: code facts and the specifications checked
//! against them, one group per part.
//!
//! Each group hands its words to the crate that owns them and keeps that
//! crate's output and exit status: `code` to codemap and its evidence
//! projection, `spec` to the `LemmaSpec` engine, `slop` and `topology` to
//! their consumers, `plan` to the plan checker, `intent` to the catalog's
//! approvals, runs and report. `check` runs extraction, projection and the
//! engine in one step. `intro` and `agent` give an agent its orientation and
//! skill.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod agent;
mod check;
mod intent;
mod plan;

/// Deterministic code facts and the specifications checked against them.
#[derive(Parser, Debug)]
#[command(name = "stele", version, about)]
struct Cli {
    #[command(subcommand)]
    group: Group,
}

#[derive(Subcommand, Debug)]
enum Group {
    /// Check code against a shipped checker: extract its facts, project them
    /// as evidence and check them, in one step.
    Check(check::CheckArgs),
    /// Extract and query code facts, and project them as spec evidence.
    #[command(subcommand)]
    Code(CodeCommand),
    /// Walk, mutate, check, bind, project and render specifications
    /// (`stele spec --help` lists them).
    #[command(disable_help_flag = true)]
    Spec {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Check a plan against its catalog of intents and the code, and show
    /// which of its changes are ready.
    #[command(subcommand)]
    Plan(plan::PlanCommand),
    /// Check a catalog of intents, approve its statements and examples as
    /// they read, record the examples' runs, and report every statement's
    /// standing.
    #[command(subcommand)]
    Intent(intent::IntentCommand),
    /// Slop metrics over measured snapshots.
    #[command(subcommand)]
    Slop(codemap_slop::cli::Command),
    /// Render a validated snapshot as an offline topology.
    Topology(codemap_topology::cli::Args),
    /// Orient an agent: what Stele is for and how to work with it.
    Intro,
    /// Install this binary's agent skills.
    #[command(subcommand)]
    Agent(agent::AgentCommand),
}

#[derive(Subcommand, Debug)]
enum CodeCommand {
    #[command(flatten)]
    Facts(codemap::cli::Command),
    #[command(flatten)]
    Evidence(codemap_lemmaspec::cli::Command),
}

fn main() -> ExitCode {
    match Cli::parse().group {
        Group::Check(args) => check::dispatch(args),
        Group::Code(CodeCommand::Facts(command)) => codemap::cli::dispatch(command),
        Group::Code(CodeCommand::Evidence(command)) => codemap_lemmaspec::cli::dispatch(command),
        Group::Spec { args } => lemmaspec::cli::dispatch(args),
        Group::Plan(command) => plan::dispatch(command),
        Group::Intent(command) => intent::dispatch(command),
        Group::Slop(command) => codemap_slop::cli::dispatch(command),
        Group::Topology(args) => codemap_topology::cli::dispatch(args),
        Group::Intro => agent::print_intro(),
        Group::Agent(command) => agent::dispatch(command),
    }
}
