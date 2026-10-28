//! `codemap-lemmaspec` — projects a codemap snapshot into `LemmaSpec`
//! evidence for the checkers this crate ships.
//!
//! codemap emits verified structural facts; `LemmaSpec` evaluates rules
//! over facts and proves every finding. This crate is the consumer between
//! them. It reads the published snapshot schema through `codemap-graph`,
//! never the extractor, and writes a `spec` of facts plus the claims a
//! checker evaluates, so `stele spec check <checker> <evidence>` is a
//! proof record for one codebase at one commit.
//!
//! Three kinds of fact leave here, distinguished by provenance:
//!
//! - facts read from the snapshot (`defines`, `calls`) cite the source
//!   location they were extracted from;
//! - facts classified by a named rule in [`policy`] (`production_file`,
//!   `test_file`, default `entry_point`: Rust binary mains, Rails actions
//!   and jobs, PHP controllers, jobs, and commands) cite that rule;
//! - facts passed on the command line (`core_file`, `presentation_file`,
//!   `public_api`, explicit `entry_point`) carry no provenance, so
//!   `LemmaSpec` presents them as the review decisions they are.
//!
//! No fact is derived here. Reachability, layering, and liveness are the
//! checker's rules, evaluated by `LemmaSpec` with a witness for each
//! result.

pub mod citations;
pub mod cli;
pub mod coverage;
pub mod lemmaspec;
pub mod policy;
pub mod positive_path;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use codemap_graph::schema::{EdgeKind, Language, Node, Provenance, Snapshot};

use lemmaspec::{Basis, Expectation, Fact};
use policy::FileClass;

/// A checker this crate ships. Each is a self-testing `LemmaSpec`
/// artifact whose fixture facts `check` replaces with projected evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checker {
    /// File-level layering and conditional absence of observed callers.
    Architecture,
    /// Conditional absence of observed paths from entry points or public API.
    Symbols,
    /// Complexity at the request layer: no entry point in shipped code sits
    /// above the cyclomatic cutoff. Needs a measured snapshot.
    Hotspots,
    /// Declaration containment of reviewer-supplied rule-card citations.
    Citations,
}

impl Checker {
    pub const ALL: [Self; 4] = [
        Self::Architecture,
        Self::Symbols,
        Self::Hotspots,
        Self::Citations,
    ];

    /// The checker's spec name, also its command-line name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Architecture => "architecture",
            Self::Symbols => "symbols",
            Self::Hotspots => "hotspots",
            Self::Citations => "citations",
        }
    }

    /// The checker artifact itself.
    pub const fn source(self) -> &'static str {
        match self {
            Self::Architecture => include_str!("../checkers/architecture.stele"),
            Self::Symbols => include_str!("../checkers/symbols.stele"),
            Self::Hotspots => include_str!("../checkers/hotspots.stele"),
            Self::Citations => include_str!("../checkers/citations.stele"),
        }
    }

    /// Whether the checker reads `complexity`, and so needs a snapshot
    /// extracted with `--measures`.
    #[must_use]
    pub const fn needs_measures(self) -> bool {
        matches!(self, Self::Hotspots)
    }

    /// The relations the evidence may assert: the checker's base
    /// vocabulary, everything its rules do not derive.
    pub const fn base_relations(self) -> &'static [&'static str] {
        match self {
            Self::Citations => &[
                "snapshot_identity",
                "rule_card",
                "location_status",
                "located",
            ],
            Self::Architecture => &[
                "snapshot_identity",
                "call_census",
                "coverage_input",
                "defines",
                "calls",
                "production_file",
                "test_file",
                "presentation_file",
                "core_file",
                "entry_point",
            ],
            Self::Symbols => &[
                "snapshot_identity",
                "call_census",
                "coverage_input",
                "defines",
                "calls",
                "production_file",
                "entry_point",
                "public_api",
            ],
            Self::Hotspots => &[
                "snapshot_identity",
                "call_census",
                "coverage_input",
                "defines",
                "production_file",
                "entry_point",
                "complexity",
            ],
        }
    }

    /// Finding relations must be empty; absence checkers additionally
    /// require a positive coverage-admission result.
    pub fn expectations(self) -> Vec<Expectation> {
        let claims: &[(&'static str, &'static str)] = match self {
            Self::Architecture => &[
                (
                    "core_stays_out_of_presentation",
                    "core_depends_on_presentation(S)",
                ),
                (
                    "no_observed_test_only_production_files",
                    "test_only_production_file(F, _)",
                ),
                (
                    "no_observed_unreferenced_production_files",
                    "unreferenced_production_file(F, _)",
                ),
            ],
            Self::Symbols => &[(
                "no_observed_unreached_symbols",
                "unused_production_symbol(S, _)",
            )],
            Self::Hotspots => &[("no_hotspot_entry_points", "hotspot_entry_point(S)")],
            Self::Citations => &[
                ("no_invalid_locations", "invalid_location(R)"),
                (
                    "no_semantically_unverified_claims",
                    "semantic_support_unverified(R)",
                ),
            ],
        };
        let mut expectations: Vec<_> = claims
            .iter()
            .map(|(id, query)| Expectation {
                id,
                query,
                count: 0,
            })
            .collect();
        if matches!(self, Self::Architecture | Self::Symbols) {
            expectations.insert(
                0,
                Expectation {
                    id: "coverage_is_admitted",
                    query: "coverage_admitted(P)",
                    count: 1,
                },
            );
        }
        expectations
    }

    fn declares(self, relation: &str) -> bool {
        self.base_relations().contains(&relation)
    }
}

impl std::str::FromStr for Checker {
    type Err = anyhow::Error;

    fn from_str(name: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|checker| checker.name() == name)
            .with_context(|| {
                format!("unknown checker `{name}`; expected architecture, symbols, hotspots, or citations")
            })
    }
}

/// The architecture and symbols checkers conclude from absence: a file or
/// symbol nothing reaches. C++ and JS/TS have no accepted shipping and
/// completeness policy. Refuse those conclusions until their coverage
/// contracts can justify them. Positive paths are a separate consumer.
fn refuse_absence_checks_without_coverage(language: Language, checker: Checker) -> Result<()> {
    if matches!(language, Language::Cpp | Language::JsTs | Language::Python)
        && matches!(checker, Checker::Architecture | Checker::Symbols)
    {
        bail!(
            "the {} checker concludes from absence, and {} snapshots have no coverage contract yet",
            checker.name(),
            language.tag()
        );
    }
    Ok(())
}

fn refuse_dirty_snapshot(snapshot: &Snapshot, options: &Options) -> Result<()> {
    if snapshot.identity.dirty && !options.allow_dirty {
        bail!(
            "snapshot {} was extracted from a dirty worktree, so its facts do not describe commit {}; pass --allow-dirty to project it anyway",
            snapshot.identity.snapshot_id,
            snapshot.identity.commit_sha
        );
    }
    Ok(())
}

/// What to project and the review decisions to assert alongside it.
#[derive(Debug, Clone)]
pub struct Options {
    pub checker: Checker,
    /// Admission threshold over recognized call sites, from 0 through 1000.
    pub min_call_coverage_per_mille: u16,
    /// Spec name; derived from the repository identity when absent.
    pub spec_name: Option<String>,
    /// Accept a snapshot whose identity says the worktree was dirty.
    pub allow_dirty: bool,
    /// Files declared as the evaluation core (repository-relative paths).
    pub core_files: Vec<String>,
    /// Files declared as presentation (repository-relative paths).
    pub presentation_files: Vec<String>,
    /// Symbols declared as entry points, by `symbol_key`.
    pub entry_points: Vec<String>,
    /// Symbols declared as public API, by `symbol_key`.
    pub public_api: Vec<String>,
}

/// The relations `project` can emit. Tests hold this against each
/// checker's declared vocabulary so neither side drifts silently.
pub const EMITTED_RELATIONS: [&str; 15] = [
    "rule_card",
    "location_status",
    "located",
    "snapshot_identity",
    "call_census",
    "coverage_input",
    "defines",
    "calls",
    "production_file",
    "test_file",
    "entry_point",
    "core_file",
    "presentation_file",
    "public_api",
    "complexity",
];

/// Project a snapshot into evidence for the chosen checker.
pub fn project(snapshot: &Snapshot, options: &Options) -> Result<String> {
    Ok(project_output(snapshot, options, false)?.evidence)
}

/// Project the same facts with explicit evidence bases and presentation metadata
/// for `LemmaSpec` 0.4. Legacy `project` output remains unchanged.
pub fn project_readable(snapshot: &Snapshot, options: &Options) -> Result<String> {
    Ok(project_output(snapshot, options, true)?.evidence)
}

/// Evidence plus the actual expanded symbol selections from the same projection.
pub struct Projection {
    pub evidence: String,
    pub selection: Option<serde_json::Value>,
}

pub fn project_output(
    snapshot: &Snapshot,
    options: &Options,
    readable: bool,
) -> Result<Projection> {
    if options.checker == Checker::Citations {
        bail!("citations requires rule cards; use citations::project or --rule-cards");
    }
    codemap_graph::validate(snapshot).context("snapshot failed validation")?;
    refuse_absence_checks_without_coverage(snapshot.identity.language, options.checker)?;
    refuse_dirty_snapshot(snapshot, options)?;
    if options.min_call_coverage_per_mille > 1000 {
        bail!("--min-call-coverage-per-mille must be between 0 and 1000");
    }
    let checker = options.checker;
    refuse_undeclared_decisions(checker, options)?;
    if checker.needs_measures() && snapshot.identity.measures_policy.is_none() {
        bail!(
            "the {} checker reads complexity, and snapshot {} carries no measures; extract it with `stele code extract --measures`",
            checker.name(),
            snapshot.identity.snapshot_id
        );
    }

    let language = snapshot.identity.language;
    let shipped: BTreeMap<&str, &Node> = snapshot
        .nodes
        .iter()
        .filter(|node| policy::is_shipped_definition(language, node))
        .map(|node| (node.id.as_str(), node))
        .collect();
    let files: BTreeSet<&str> = shipped
        .values()
        .map(|node| node.provenance.rel_path.as_str())
        .collect();

    let mut facts = definition_facts(&shipped);
    facts.push(Fact {
        basis: Basis::Snapshot,
        relation: "snapshot_identity",
        args: vec![
            snapshot.identity.snapshot_id.clone(),
            snapshot.identity.repository.id.clone(),
            snapshot.identity.commit_sha.clone(),
            snapshot.identity.tree_sha.clone(),
            policy::provenance(policy::ENTRY_POINT, language),
        ],
        provenance: Some(format!("snapshot:{}", snapshot.identity.snapshot_id)),
    });
    facts.extend(layout_facts(language, &files));
    facts.extend(call_facts(snapshot, &shipped));
    facts.extend(entry_point_facts(language, &shipped, options)?);
    facts.extend(complexity_facts(snapshot, &shipped));
    facts.extend(decision_facts(&shipped, &files, options)?);
    facts.extend(coverage_facts(snapshot, options));
    facts.sort();
    facts.dedup();
    // The header describes the whole projection; the file asserts only
    // what the checker declares.
    let doc = header(snapshot, options, &facts);
    facts.retain(|fact| checker.declares(fact.relation));

    let spec = options
        .spec_name
        .clone()
        .unwrap_or_else(|| lemmaspec::spec_name(&snapshot.identity.repository.id));
    let selection = (checker == Checker::Symbols).then(|| {
        let selected = |relation| {
            facts
                .iter()
                .filter(|fact| fact.relation == relation)
                .map(|fact| fact.args[0].clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        };
        serde_json::json!({
            "entry_policy": policy::provenance(policy::ENTRY_POINT, language),
            "coverage_policy": coverage::POLICY,
            "min_call_coverage_per_mille": options.min_call_coverage_per_mille,
            "entry_points": selected("entry_point"),
            "public_api": selected("public_api"),
        })
    });
    let evidence = if readable {
        let labels = labels(snapshot, &facts);
        let notes = doc.join("\n");
        lemmaspec::render_with_metadata(
            &[question(checker).into()],
            &spec,
            &facts,
            &checker.expectations(),
            Some(&lemmaspec::Readable {
                snapshot_id: &snapshot.identity.snapshot_id,
                notes: &notes,
                labels: &labels,
            }),
        )
    } else {
        lemmaspec::render(&doc, &spec, &facts, &checker.expectations())
    };
    Ok(Projection {
        evidence,
        selection,
    })
}

/// `defines(symbol, file)` for every shipped definition.
fn definition_facts(shipped: &BTreeMap<&str, &Node>) -> Vec<Fact> {
    shipped
        .values()
        .map(|node| Fact {
            basis: Basis::Snapshot,
            relation: "defines",
            args: vec![node.id.clone(), node.provenance.rel_path.clone()],
            provenance: Some(location(&node.provenance)),
        })
        .collect()
}

/// `complexity(symbol, cc)` for every shipped definition the snapshot
/// measured, cited at the callable. Nothing leaves for an unmeasured
/// snapshot; the checkers that read it refuse one first.
fn complexity_facts(snapshot: &Snapshot, shipped: &BTreeMap<&str, &Node>) -> Vec<Fact> {
    snapshot
        .measures
        .iter()
        .filter_map(|measure| {
            shipped
                .get(measure.node_id.as_str())
                .map(|node| (measure, node))
        })
        .map(|(measure, node)| Fact {
            basis: Basis::Snapshot,
            relation: "complexity",
            args: vec![node.id.clone(), measure.cc.to_string()],
            provenance: Some(location(&node.provenance)),
        })
        .collect()
}

/// `production_file(file)` or `test_file(file)` for every file that holds
/// a shipped definition and sits under a marker directory.
fn layout_facts(language: Language, files: &BTreeSet<&str>) -> Vec<Fact> {
    files
        .iter()
        .filter_map(|file| {
            let relation = match policy::classify_file(language, file) {
                FileClass::Production => "production_file",
                FileClass::Test => "test_file",
                FileClass::Unclassified => return None,
            };
            Some(Fact {
                basis: Basis::Policy,
                relation,
                args: vec![(*file).to_string()],
                provenance: Some(policy::provenance(policy::FILE_LAYOUT, language)),
            })
        })
        .collect()
}

/// One `calls(caller, callee)` per pair, cited at its first call site.
/// Both ends must ship: a call from an inline test module is not a
/// production reach.
fn call_facts(snapshot: &Snapshot, shipped: &BTreeMap<&str, &Node>) -> Vec<Fact> {
    let mut first_site: BTreeMap<(&str, &str), &Provenance> = BTreeMap::new();
    for edge in snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
    {
        if !shipped.contains_key(edge.from.as_str()) || !shipped.contains_key(edge.to.as_str()) {
            continue;
        }
        let key = (edge.from.as_str(), edge.to.as_str());
        let known_earlier = first_site
            .get(&key)
            .is_some_and(|known| known.span.start_line <= edge.provenance.span.start_line);
        if !known_earlier {
            first_site.insert(key, &edge.provenance);
        }
    }
    first_site
        .into_iter()
        .map(|((from, to), provenance)| Fact {
            basis: Basis::Snapshot,
            relation: "calls",
            args: vec![from.to_string(), to.to_string()],
            provenance: Some(location(provenance)),
        })
        .collect()
}

/// `entry_point(symbol)`: toolchain-convention defaults cite the policy;
/// symbols named on the command line are review decisions. A symbol that
/// is both keeps the policy citation.
fn entry_point_facts(
    language: Language,
    shipped: &BTreeMap<&str, &Node>,
    options: &Options,
) -> Result<Vec<Fact>> {
    let mut entries: BTreeMap<&str, (Option<String>, Basis)> = shipped
        .values()
        .filter(|node| policy::is_default_entry_point(language, node))
        .map(|node| {
            (
                node.id.as_str(),
                (
                    Some(policy::provenance(policy::ENTRY_POINT, language)),
                    Basis::Policy,
                ),
            )
        })
        .collect();
    for id in resolve_symbols(shipped, &options.entry_points, "--entry")? {
        entries.entry(id).or_insert((None, Basis::ReviewerDeclared));
    }
    Ok(entries
        .into_iter()
        .map(|(id, (provenance, basis))| Fact {
            basis,
            relation: "entry_point",
            args: vec![id.to_string()],
            provenance,
        })
        .collect())
}

/// The layout and API decisions passed on the command line, without
/// provenance. A path or symbol the snapshot does not know is a typo.
fn decision_facts(
    shipped: &BTreeMap<&str, &Node>,
    files: &BTreeSet<&str>,
    options: &Options,
) -> Result<Vec<Fact>> {
    let mut facts = Vec::new();
    for (relation, paths) in [
        ("core_file", &options.core_files),
        ("presentation_file", &options.presentation_files),
    ] {
        for path in paths {
            if !files.contains(path.as_str()) {
                bail!(
                    "{relation}: `{path}` holds no shipped definition in this snapshot; paths are repository-relative"
                );
            }
            facts.push(Fact {
                basis: Basis::ReviewerDeclared,
                relation,
                args: vec![path.clone()],
                provenance: None,
            });
        }
    }
    for id in resolve_symbols(shipped, &options.public_api, "--public-api")? {
        facts.push(Fact {
            basis: Basis::ReviewerDeclared,
            relation: "public_api",
            args: vec![id.to_string()],
            provenance: None,
        });
    }
    Ok(facts)
}

fn header(snapshot: &Snapshot, options: &Options, facts: &[Fact]) -> Vec<String> {
    let checker = options.checker;
    let identity = &snapshot.identity;
    let count = |relation: &str| {
        facts
            .iter()
            .filter(|fact| fact.relation == relation)
            .count()
    };
    let mut lines = vec![
        format!(
            "Evidence for the `{}` checker, projected from codemap snapshot {}.",
            checker.name(),
            identity.snapshot_id
        ),
        format!(
            "repository {}, commit {}, language {}, extractor {}, dirty: {}",
            identity.repository.id,
            identity.commit_sha,
            identity.language.tag(),
            identity.extractor_version,
            identity.dirty
        ),
        String::new(),
        format!(
            "{} shipped callables (policy:{}/{}), {} verified calls, {} production files, {} test files.",
            count("defines"),
            policy::SHIPPED_DEFINITION,
            identity.language.tag(),
            count("calls"),
            count("production_file"),
            count("test_file")
        ),
        "A fact citing a source location was read from the snapshot: every call".into(),
        "was resolved by a language server to a unique in-repository definition.".into(),
        "A fact citing a policy was classified by a named rule in codemap-lemmaspec.".into(),
        "A fact without provenance is a review decision passed on the command line.".into(),
    ];
    let coverage = coverage::Coverage::from_snapshot(snapshot);
    lines.extend([
        format!("schema {}, tree {}", identity.schema_version, identity.tree_sha),
        format!("Entry-point policy: {}; explicit entry and public API selections are the facts below.", policy::provenance(policy::ENTRY_POINT, identity.language)),
        format!("Extraction scope: {} recorded {} source files; skipped files: {}; parse errors: {}; semantic: {}.", snapshot.files.len(), identity.language.tag(), coverage.files_skipped, coverage.parse_errors, coverage.semantic),
        "Scope includes only snapshot file records; excluded-source inventory and reasons: unknown.".into(),
        "Unsupported observations: unknown (snapshot schema has no unsupported-observation accounting).".into(),
        format!("Coverage policy: {}; exact in-repository Resolved sites / all recognized sites: {}/{}; minimum: {} per mille; admitted: {}.", coverage::POLICY, coverage.resolved, coverage.recognized, options.min_call_coverage_per_mille, coverage.admitted(options.min_call_coverage_per_mille)),
        "An empty census is inadmissible. Missing admission is inconclusive evidence, even with zero findings.".into(),
        "Admission is a threshold policy, never a proof of extraction completeness.".into(),
        "Absence means no path in observed graph under entry-point policy named above, within this extraction scope.".into(),
    ]);
    let mut paths: Vec<_> = snapshot
        .files
        .iter()
        .map(|file| file.rel_path.as_str())
        .collect();
    paths.sort_unstable();
    for path in paths {
        lines.push(format!("Scope file: {}", lemmaspec::term(path)));
    }
    if let Some(measures_policy) = &identity.measures_policy {
        if checker.needs_measures() {
            lines.push(format!(
                "{} complexity facts counted under `{measures_policy}`; the cutoff is the checker's.",
                count("complexity")
            ));
        }
    }
    lines
}

fn refuse_undeclared_decisions(checker: Checker, options: &Options) -> Result<()> {
    for (flag, relation, given) in [
        ("--core", "core_file", !options.core_files.is_empty()),
        (
            "--presentation",
            "presentation_file",
            !options.presentation_files.is_empty(),
        ),
        ("--public-api", "public_api", !options.public_api.is_empty()),
    ] {
        if given && !checker.declares(relation) {
            bail!(
                "{flag} asserts `{relation}`, which the {} checker does not declare",
                checker.name()
            );
        }
    }
    Ok(())
}

fn location(provenance: &Provenance) -> String {
    format!("{}:{}", provenance.rel_path, provenance.span.start_line)
}

/// Resolve review-supplied symbol keys to every shipped node carrying that
/// key. A key nothing matches is a typo, not an empty fact.
fn resolve_symbols<'a>(
    shipped: &BTreeMap<&'a str, &'a Node>,
    keys: &[String],
    flag: &str,
) -> Result<Vec<&'a str>> {
    let mut ids = Vec::new();
    for key in keys {
        let matches: Vec<&str> = shipped
            .values()
            .filter(|node| node.symbol_key == *key)
            .map(|node| node.id.as_str())
            .collect();
        if matches.is_empty() {
            let example = shipped
                .values()
                .next()
                .map_or("rust:src/main.rs#main", |node| node.symbol_key.as_str());
            bail!(
                "{flag} {key}: no shipped definition has that symbol key (keys look like `{example}`)"
            );
        }
        ids.extend(matches);
    }
    Ok(ids)
}

const fn question(checker: Checker) -> &'static str {
    match checker {
        Checker::Architecture => {
            "Does the observed graph cross declared layers or leave production files without observed callers?"
        }
        Checker::Symbols => "Which shipped symbols have no path in the observed call graph?",
        Checker::Hotspots => "Which declared entry points exceed the complexity cutoff?",
        Checker::Citations => {
            "Do rule-card citations locate recorded declarations, and what remains semantically unverified?"
        }
    }
}

fn labels(snapshot: &Snapshot, facts: &[Fact]) -> Vec<lemmaspec::SymbolLabel> {
    let used: BTreeSet<&str> = facts
        .iter()
        .flat_map(|fact| fact.args.iter().map(String::as_str))
        .collect();
    let mut labels: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|node| used.contains(node.id.as_str()))
        .map(|node| lemmaspec::SymbolLabel {
            symbol: node.id.clone(),
            label: format!(
                "{} ({}:{})",
                node.name, node.provenance.rel_path, node.provenance.span.start_line
            ),
            source: source_link(&node.provenance.rel_path, node.provenance.span.start_line),
        })
        .collect();
    for file in &snapshot.files {
        if used.contains(file.rel_path.as_str()) {
            labels.push(lemmaspec::SymbolLabel {
                symbol: file.rel_path.clone(),
                label: file.rel_path.clone(),
                source: source_link(&file.rel_path, 1),
            });
        }
    }
    labels.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    labels.dedup_by(|a, b| a.symbol == b.symbol);
    labels
}

fn source_link(path: &str, line_number: u32) -> Option<String> {
    use std::fmt::Write as _;
    format!("{path}:{line_number}")
        .parse::<codemap_graph::SourceLocation>()
        .ok()?;
    let mut link = String::from("./");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            link.push(char::from(byte));
        } else {
            let _ = write!(link, "%{byte:02X}");
        }
    }
    let _ = write!(link, "#L{line_number}");
    Some(link)
}

fn coverage_facts(snapshot: &Snapshot, options: &Options) -> Vec<Fact> {
    let mut facts = Vec::new();
    let language = snapshot.identity.language;
    let coverage = coverage::Coverage::from_snapshot(snapshot);
    let entry_policy = policy::provenance(policy::ENTRY_POINT, language);
    for category in coverage::categories(language) {
        facts.push(Fact {
            basis: Basis::Snapshot,
            relation: "call_census",
            args: vec![
                category.report_key().into(),
                snapshot
                    .report
                    .call_sites_by_category
                    .get(category.report_key())
                    .copied()
                    .unwrap_or(0)
                    .to_string(),
            ],
            provenance: Some(format!("snapshot:{}", snapshot.identity.snapshot_id)),
        });
    }
    facts.push(Fact {
        basis: Basis::Policy,
        relation: "coverage_input",
        args: vec![
            entry_policy,
            coverage.recognized.to_string(),
            coverage.per_mille.unwrap_or(0).to_string(),
            options.min_call_coverage_per_mille.to_string(),
            u8::from(coverage.semantic).to_string(),
            coverage.parse_errors.to_string(),
            coverage.files_skipped.to_string(),
        ],
        provenance: Some(format!("policy:{}", coverage::POLICY)),
    });
    facts
}
