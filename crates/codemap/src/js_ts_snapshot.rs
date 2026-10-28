//! Bind guarded compiler evidence and independently admitted calls to source snapshots.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::extract::{ExtractOptions, build_report, build_snapshot};
use crate::git;
use crate::js_ts_admission;
use crate::js_ts_inputs::{InputManifest, InputRoots};
use crate::js_ts_runtime::JsTsRuntimeManifest;
use crate::js_ts_semantic::{CapturedEvidence, HelperConfig};
use crate::js_ts_syntax::{self, RawFile};
use crate::schema::{
    CallSiteCategory, Edge, EdgeKind, FactSource, Language, Snapshot, edge_id, sort_edges,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsTsExtractConfig {
    pub roots: InputRoots,
    pub inputs: InputManifest,
    pub runtime: JsTsRuntimeManifest,
    pub bundle: PathBuf,
}

pub fn extract_js_ts(
    options: &ExtractOptions,
    project: &str,
    config: &JsTsExtractConfig,
) -> Result<Snapshot> {
    extract_current(options, project, config, false)
}

pub fn extract_js_ts_worktree(
    options: &ExtractOptions,
    project: &str,
    config: &JsTsExtractConfig,
) -> Result<Snapshot> {
    extract_current(options, project, config, true)
}

fn extract_current(
    options: &ExtractOptions,
    project: &str,
    config: &JsTsExtractConfig,
    dirty: bool,
) -> Result<Snapshot> {
    ensure!(
        options.language == Language::JsTs && options.calls_required,
        "a JS/TS project requires --language js-ts --calls required"
    );
    let state = if dirty {
        git::gather_worktree(&options.path, Language::JsTs)?
    } else {
        git::gather(&options.path, Language::JsTs)?
    };
    let root = std::fs::canonicalize(&state.repo_root)?;
    ensure!(
        config.roots.source == root,
        "JS/TS source root differs from the selected repository"
    );
    let source_files = if dirty {
        crate::extract::read_worktree_files(&state)?
    } else {
        crate::extract::read_snapshot_files(&state)?
    };
    for (path, bytes) in &source_files {
        ensure!(
            config.inputs.files.get(&format!("{{source}}/{path}"))
                == Some(&crate::schema::sha256_hex(bytes)),
            "inventoried JS/TS source is missing or stale in prepared inputs: {path}"
        );
    }
    let spelled = build_snapshot(
        &state,
        &source_files,
        false,
        options.measures,
        options.twins,
    )?;
    let captured = crate::js_ts_semantic::resolve(&HelperConfig {
        roots: config.roots.clone(),
        inputs: config.inputs.clone(),
        runtime: config.runtime.clone(),
        bundle: config.bundle.clone(),
        project: project.to_owned(),
    })?;
    if !dirty {
        let config_bytes = git::read_file_at(&root, &state.commit_sha, project)
            .context("selected JS/TS project must belong to the committed source tree")?;
        ensure!(
            captured.read_source(project)?.as_bytes() == config_bytes,
            "JS/TS project bytes differ from the selected commit"
        );
    }
    let snapshot = project_snapshot(spelled, &source_files, &captured)?;
    captured.verify_unchanged()?;
    git::verify_unchanged(&state).context("repository changed during JS/TS extraction")?;
    Ok(snapshot)
}

fn project_snapshot(
    mut snapshot: Snapshot,
    source_files: &[(String, Vec<u8>)],
    captured: &CapturedEvidence,
) -> Result<Snapshot> {
    let evidence = &captured.evidence;
    let program: BTreeSet<_> = evidence
        .program_files
        .iter()
        .filter_map(|file| file.path.strip_prefix("{source}/").map(str::to_owned))
        .collect();
    let sources: BTreeMap<_, _> = source_files.iter().cloned().collect();
    let mut parsed = BTreeMap::new();
    for (path, bytes) in &sources {
        ensure!(
            captured.read_source(path)?.as_bytes() == bytes,
            "captured source bytes differ from snapshot source: {path}"
        );
        parsed.insert(path.clone(), js_ts_syntax::extract_file(path, bytes)?);
    }
    let admission = js_ts_admission::admit(evidence, &sources, &parsed)?;
    let mut resolved = project_calls(&mut snapshot, &parsed, &program, admission.calls)?;
    let mut unanalysed = 0;
    let mut withheld = 0;
    for site in &mut snapshot.call_sites {
        let key = (
            site.rel_path.clone(),
            site.span.start_byte,
            site.span.end_byte,
        );
        site.category = if resolved.remove(&key) {
            CallSiteCategory::Resolved
        } else if !program.contains(&site.rel_path) {
            unanalysed += 1;
            CallSiteCategory::Unprofiled
        } else {
            withheld += 1;
            if admission
                .reasons
                .get(&key)
                .is_some_and(|reason| reason == "external")
            {
                CallSiteCategory::ExternalCrate
            } else {
                CallSiteCategory::Unsupported
            }
        };
    }
    ensure!(
        resolved.is_empty(),
        "admitted JS/TS calls lack exact source observations"
    );
    snapshot.identity = snapshot
        .identity
        .with_js_ts_semantic(captured.identity.clone());
    let mut observations = snapshot.report.observations;
    let source_sites: BTreeSet<_> = snapshot
        .call_sites
        .iter()
        .map(|site| {
            (
                site.rel_path.clone(),
                site.span.start_byte,
                site.span.end_byte,
            )
        })
        .collect();
    let unmatched = admission
        .reasons
        .keys()
        .filter(|key| !source_sites.contains(key))
        .count();
    for (name, count) in [
        ("js_ts_program_files", program.len()),
        ("js_ts_unanalysed_calls", unanalysed),
        ("js_ts_withheld_calls", withheld),
        ("js_ts_unmatched_compiler_sites", unmatched),
    ] {
        if count > 0 {
            observations.insert(name.to_owned(), count);
        }
    }
    sort_edges(&mut snapshot.edges);
    snapshot.report = build_report(
        &snapshot.files,
        &snapshot.nodes,
        &snapshot.edges,
        &snapshot.call_sites,
        snapshot.report.files_skipped,
        snapshot.report.parse_errors,
        observations,
    );
    crate::validate::validate(&snapshot)?;
    Ok(snapshot)
}

fn project_calls(
    snapshot: &mut Snapshot,
    parsed: &BTreeMap<String, RawFile>,
    program: &BTreeSet<String>,
    calls: Vec<js_ts_admission::AdmittedCall>,
) -> Result<BTreeSet<(String, usize, usize)>> {
    let mut symbols: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, node) in snapshot.nodes.iter().enumerate() {
        symbols
            .entry(node.symbol_key.clone())
            .or_default()
            .push(index);
    }
    let mut resolved = BTreeSet::new();
    for call in calls {
        ensure!(
            program.contains(&call.path) && program.contains(&call.target_path),
            "admitted body is outside the semantic program"
        );
        let caller = definition(
            snapshot,
            &symbols,
            parsed,
            &call.path,
            call.caller_definition,
        )?;
        let target = definition(
            snapshot,
            &symbols,
            parsed,
            &call.target_path,
            call.target_definition,
        )?;
        let mut provenance = caller.provenance.clone();
        provenance.span = call.span;
        provenance.fact_source = FactSource::TypeScript;
        let edge = Edge {
            id: edge_id(
                EdgeKind::Calls,
                &caller.id,
                &target.id,
                call.span.start_byte,
            ),
            kind: EdgeKind::Calls,
            from: caller.id.clone(),
            to: target.id.clone(),
            provenance,
        };
        ensure!(
            resolved.insert((call.path, call.span.start_byte, call.span.end_byte)),
            "duplicate admitted JS/TS site"
        );
        snapshot.edges.push(edge);
    }
    Ok(resolved)
}

fn definition<'a>(
    snapshot: &'a Snapshot,
    symbols: &BTreeMap<String, Vec<usize>>,
    parsed: &BTreeMap<String, RawFile>,
    path: &str,
    index: usize,
) -> Result<&'a crate::schema::Node> {
    let definition = parsed
        .get(path)
        .and_then(|raw| raw.defs.get(index))
        .context("admitted definition missing from source model")?;
    let key = crate::schema::symbol_key(Language::JsTs, path, &definition.scope, &definition.name);
    let matches: Vec<_> = symbols
        .get(&key)
        .into_iter()
        .flatten()
        .map(|index| &snapshot.nodes[*index])
        .filter(|node| {
            node.provenance.rel_path == path
                && node.kind == definition.kind
                && node.provenance.span == definition.span
        })
        .collect();
    ensure!(
        matches.len() == 1,
        "admitted definition lacks one exact snapshot node"
    );
    Ok(matches[0])
}
