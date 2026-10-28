//! Project guarded compiler evidence onto the source-addressed spelled graph.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::cpp_inputs::InputManifest;
use crate::cpp_lsp::{CppSemanticResult, CppSession};
use crate::cpp_profile::ProfileRoots;
use crate::cpp_runtime::CppRuntimeManifest;
use crate::cpp_syntax::{RawFile, ScopeIndex};
use crate::extract::{ExtractOptions, build_report, build_snapshot};
use crate::git;
use crate::lsp;
use crate::rust_syntax::RawDef;
use crate::schema::{
    CallSiteCategory, CppSemanticInputs, Edge, EdgeKind, FactSource, Language, Node, NodeKind,
    Snapshot, edge_id, node_id, node_id_disambiguated, sort_edges, sort_nodes, symbol_key,
};

/// Operator-prepared parse inputs; extraction never prepares or downloads them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CppExtractConfig {
    pub compilation_database: PathBuf,
    pub translation_units: Vec<String>,
    #[serde(default = "default_query_lanes")]
    pub query_lanes: usize,
    pub roots: ProfileRoots,
    pub inputs: InputManifest,
    pub runtime: CppRuntimeManifest,
}

const fn default_query_lanes() -> usize {
    1
}

pub fn extract_cpp(options: &ExtractOptions, config: &CppExtractConfig) -> Result<Snapshot> {
    extract_current(options, config, false)
}

pub fn extract_cpp_worktree(
    options: &ExtractOptions,
    config: &CppExtractConfig,
) -> Result<Snapshot> {
    extract_current(options, config, true)
}

fn extract_current(
    options: &ExtractOptions,
    config: &CppExtractConfig,
    dirty: bool,
) -> Result<Snapshot> {
    ensure!(
        options.language == Language::Cpp && options.calls_required,
        "a C++ profile requires --language cpp --calls required"
    );
    ensure!(
        (1..=8).contains(&config.query_lanes),
        "C++ query lanes must be between 1 and 8"
    );
    let state = if dirty {
        git::gather_worktree(&options.path, Language::Cpp)?
    } else {
        git::gather(&options.path, Language::Cpp)?
    };
    let root = std::fs::canonicalize(&state.repo_root)?;
    ensure!(
        std::fs::canonicalize(&config.roots.source)? == root,
        "C++ profile source root differs from the selected repository"
    );
    ensure!(
        config.compilation_database.is_absolute(),
        "C++ compilation database must be absolute"
    );
    let sources = if dirty {
        crate::extract::read_worktree_files(&state)?
    } else {
        crate::extract::read_snapshot_files(&state)?
    };
    let roots = crate::cpp_lsp::canonical_roots(&config.roots)?;
    let profile = crate::cpp_profile::load_profile(&config.compilation_database, &roots)?;
    let selected = validate_selection(&config.translation_units, &sources, &profile)?;
    let spelled = build_snapshot(&state, &sources, false, options.measures, options.twins)?;
    let semantic = CppSession::start_profile(
        &config.compilation_database,
        &config.roots,
        &config.inputs,
        &config.runtime,
        &sources,
    )?
    .resolve_with_query_lanes(config.query_lanes)?;
    ensure!(
        semantic.profile_sha256 == profile.fingerprint,
        "C++ profile changed after translation-unit selection"
    );
    let snapshot = project(spelled, &root, &sources, &selected, semantic)?;
    git::verify_unchanged(&state).context("repository changed during C++ extraction")?;
    Ok(snapshot)
}

fn validate_selection(
    units: &[String],
    sources: &[(String, Vec<u8>)],
    profile: &crate::cpp_profile::CompilationProfile,
) -> Result<BTreeSet<String>> {
    ensure!(
        !units.is_empty(),
        "C++ translation-unit selection must not be empty"
    );
    let inventory: BTreeSet<_> = sources.iter().map(|(path, _)| path.as_str()).collect();
    let mut selected = BTreeSet::new();
    for path in units {
        ensure!(
            inventory.contains(path.as_str()) && !git::is_cpp_header(path),
            "selected C++ translation unit is not an inventoried implementation source: {path}"
        );
        ensure!(
            selected.insert(path.clone()),
            "duplicate selected C++ translation unit: {path}"
        );
    }
    let commands: BTreeSet<_> = profile
        .application_units
        .keys()
        .filter(|path| git::is_source_path(Language::Cpp, path) && !git::is_cpp_header(path))
        .cloned()
        .collect();
    ensure!(
        selected == commands,
        "C++ compilation commands differ from the explicitly selected translation units"
    );
    Ok(selected)
}

fn project(
    mut snapshot: Snapshot,
    root: &Path,
    sources: &[(String, Vec<u8>)],
    selected: &BTreeSet<String>,
    semantic: CppSemanticResult,
) -> Result<Snapshot> {
    snapshot.identity = snapshot.identity.with_cpp_semantic(CppSemanticInputs {
        profile_sha256: semantic.profile_sha256,
        inputs_sha256: semantic.inputs_sha256,
        runtime_sha256: semantic.runtime_sha256,
        clangd_sha256: semantic.clangd_sha256,
        policy: semantic.policy.to_owned(),
    });
    let paths: BTreeMap<String, &str> = sources
        .iter()
        .map(|(path, _)| (lsp::uri_from_path(&root.join(path)), path.as_str()))
        .collect();
    let index = ScopeIndex::build(sources)?;
    let parsed = sources
        .iter()
        .map(|(path, bytes)| {
            Ok((
                path.as_str(),
                crate::cpp_syntax::extract_file(path, bytes, &index)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let (body_ids, recovered) = project_bodies(&mut snapshot, &parsed, &paths, &semantic.bodies)?;
    let mut resolved = project_calls(&mut snapshot, &body_ids, &semantic.calls)?;
    let mut observations = BTreeMap::new();
    for observation in semantic.observations {
        let path = *paths
            .get(&observation.uri)
            .context("compiler call observation is outside snapshot")?;
        ensure!(
            observations
                .insert(
                    (
                        path.to_owned(),
                        observation.span.start_byte,
                        observation.span.end_byte
                    ),
                    observation.category
                )
                .is_none(),
            "duplicate compiler call observation"
        );
    }
    let mut unprofiled = 0;
    for site in &mut snapshot.call_sites {
        let key = (
            site.rel_path.clone(),
            site.span.start_byte,
            site.span.end_byte,
        );
        site.category = if let Some(category) = observations.remove(&key) {
            category
        } else if git::is_cpp_header(&site.rel_path) {
            CallSiteCategory::Unsupported
        } else {
            ensure!(
                !selected.contains(&site.rel_path),
                "selected translation unit lacks a compiler call observation"
            );
            unprofiled += 1;
            CallSiteCategory::Unprofiled
        };
        ensure!(
            (site.category == CallSiteCategory::Resolved) == resolved.remove(&key),
            "compiler call category contradicts admitted edges"
        );
    }
    ensure!(
        observations.is_empty() && resolved.is_empty(),
        "compiler call facts lack exact spelled source sites"
    );
    let mut census = snapshot.report.observations;
    for (name, count) in [
        ("cpp_compiled_bodies", semantic.bodies.len()),
        ("cpp_recovered_bodies", recovered),
        ("cpp_withheld_bodies", semantic.withheld_bodies),
        ("cpp_withheld_calls", semantic.withheld_calls),
        ("cpp_unprofiled_calls", unprofiled),
    ] {
        if count > 0 {
            census.insert(name.to_owned(), count);
        }
    }
    sort_nodes(&mut snapshot.nodes);
    sort_edges(&mut snapshot.edges);
    snapshot.report = build_report(
        &snapshot.files,
        &snapshot.nodes,
        &snapshot.edges,
        &snapshot.call_sites,
        snapshot.report.files_skipped,
        snapshot.report.parse_errors,
        census,
    );
    crate::validate::validate(&snapshot)?;
    Ok(snapshot)
}

fn project_calls(
    snapshot: &mut Snapshot,
    body_ids: &[String],
    calls: &[crate::cpp_admission::AdmittedCall],
) -> Result<BTreeSet<(String, usize, usize)>> {
    let nodes: BTreeMap<_, _> = snapshot
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut resolved = BTreeSet::new();
    for call in calls {
        let caller = body_ids
            .get(call.caller)
            .context("admitted caller is absent")?;
        let target = body_ids
            .get(call.target)
            .context("admitted target is absent")?;
        let mut provenance = nodes
            .get(caller.as_str())
            .context("caller node is absent")?
            .provenance
            .clone();
        provenance.span = call.span;
        provenance.fact_source = FactSource::Clangd;
        ensure!(
            resolved.insert((
                provenance.rel_path.clone(),
                call.span.start_byte,
                call.span.end_byte
            )),
            "duplicate admitted call site"
        );
        snapshot.edges.push(Edge {
            id: edge_id(EdgeKind::Calls, caller, target, call.span.start_byte),
            kind: EdgeKind::Calls,
            from: caller.clone(),
            to: target.clone(),
            provenance,
        });
    }
    Ok(resolved)
}

#[allow(clippy::too_many_lines)] // Keep exact identity, lexical-parent and provenance admission together.
fn project_bodies(
    snapshot: &mut Snapshot,
    parsed: &BTreeMap<&str, RawFile>,
    paths: &BTreeMap<String, &str>,
    bodies: &[crate::cpp_admission::CompiledBody],
) -> Result<(Vec<String>, usize)> {
    let mut definitions: BTreeMap<(String, String, usize, usize), Vec<usize>> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut files = BTreeMap::new();
    for (index, node) in snapshot.nodes.iter().enumerate() {
        definitions
            .entry((
                node.provenance.rel_path.clone(),
                node.symbol_key.clone(),
                node.provenance.span.start_byte,
                node.provenance.span.end_byte,
            ))
            .or_default()
            .push(index);
        ids.insert(node.id.clone());
        if node.kind == NodeKind::File {
            files.insert(node.provenance.rel_path.clone(), index);
        }
    }
    let mut body_ids = Vec::new();
    let mut recovered = 0;
    for body in bodies {
        let path = *paths
            .get(&body.uri)
            .context("compiled body is outside snapshot source inventory")?;
        if let Some(id) = exact_definition(&snapshot.nodes, &definitions, path, &body.definition)? {
            body_ids.push(id);
            continue;
        }
        let raw: &RawFile = &parsed[path];
        let parent = if let Some(parent) = body.definition.parent {
            let definition = raw
                .defs
                .get(parent)
                .context("compiled body has an invalid lexical parent")?;
            exact_definition(&snapshot.nodes, &definitions, path, definition)?
                .context("compiled lexical parent is absent")?
        } else {
            snapshot.nodes[*files.get(path).context("source file node is absent")?]
                .id
                .clone()
        };
        let mut id = node_id(
            Language::Cpp,
            body.definition.kind,
            path,
            &body.definition.scope,
            &body.definition.name,
        );
        if ids.contains(&id) {
            id = node_id_disambiguated(
                Language::Cpp,
                body.definition.kind,
                path,
                &body.definition.scope,
                &body.definition.name,
                body.definition.span.start_byte,
            );
        }
        ensure!(!ids.contains(&id), "compiled body ID collision");
        let mut provenance = snapshot.nodes[*files.get(path).context("source file is absent")?]
            .provenance
            .clone();
        provenance.span = body.definition.span;
        provenance.fact_source = FactSource::Clangd;
        ids.insert(id.clone());
        definitions
            .entry((
                path.to_owned(),
                symbol_key(
                    Language::Cpp,
                    path,
                    &body.definition.scope,
                    &body.definition.name,
                ),
                body.definition.span.start_byte,
                body.definition.span.end_byte,
            ))
            .or_default()
            .push(snapshot.nodes.len());
        snapshot.nodes.push(Node {
            python: None,
            id: id.clone(),
            kind: body.definition.kind,
            name: body.definition.name.clone(),
            symbol_key: symbol_key(
                Language::Cpp,
                path,
                &body.definition.scope,
                &body.definition.name,
            ),
            provenance: provenance.clone(),
        });
        snapshot.edges.push(Edge {
            id: edge_id(EdgeKind::Contains, &parent, &id, provenance.span.start_byte),
            kind: EdgeKind::Contains,
            from: parent,
            to: id.clone(),
            provenance,
        });
        body_ids.push(id);
        recovered += 1;
    }
    Ok((body_ids, recovered))
}

fn exact_definition(
    nodes: &[Node],
    definitions: &BTreeMap<(String, String, usize, usize), Vec<usize>>,
    path: &str,
    definition: &RawDef,
) -> Result<Option<String>> {
    let key = symbol_key(Language::Cpp, path, &definition.scope, &definition.name);
    let mut found = definitions
        .get(&(
            path.to_owned(),
            key.clone(),
            definition.span.start_byte,
            definition.span.end_byte,
        ))
        .into_iter()
        .flatten()
        .map(|index| &nodes[*index])
        .filter(|node| {
            node.kind == definition.kind
                && node.symbol_key == key
                && node.provenance.rel_path == path
                && node.provenance.span == definition.span
        });
    let id = found.next().map(|node| node.id.clone());
    ensure!(
        found.next().is_none(),
        "compiled definition matches multiple source nodes"
    );
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp_profile::{CompilationCommand, CompilationProfile};

    #[test]
    fn invalid_query_lanes_fail_before_repository_or_runtime_access() {
        let mut config: CppExtractConfig = serde_json::from_value(serde_json::json!({
            "compilation_database": "/absent/compile_commands.json",
            "translation_units": ["unit.cpp"],
            "roots": {"source": "/absent", "build": null, "sysroot": null, "dependencies": []},
            "inputs": {"files": {}},
            "runtime": {"files": {}, "cache_family": {"directory": "/absent", "prefix": "cache"}}
        }))
        .unwrap();
        assert_eq!(config.query_lanes, 1);
        let options = ExtractOptions {
            path: "/absent".into(),
            language: Language::Cpp,
            calls_required: true,
            ..ExtractOptions::default()
        };
        for lanes in [0, 9, usize::MAX] {
            config.query_lanes = lanes;
            assert!(
                extract_cpp(&options, &config)
                    .unwrap_err()
                    .to_string()
                    .contains("query lanes must be between 1 and 8")
            );
        }
    }

    #[test]
    fn translation_unit_selection_requires_exact_inventory_and_commands() {
        let sources = vec![
            ("a.cpp".into(), vec![]),
            ("b.C".into(), vec![]),
            ("api.HPP".into(), vec![]),
        ];
        let command = CompilationCommand {
            directory: "{source}".into(),
            arguments: vec![],
        };
        let profile = CompilationProfile {
            application_units: BTreeMap::from([
                ("a.cpp".into(), command.clone()),
                ("api.HPP".into(), command),
            ]),
            fingerprint: String::new(),
        };
        assert_eq!(
            validate_selection(&["a.cpp".into()], &sources, &profile).unwrap(),
            BTreeSet::from(["a.cpp".into()])
        );
        for units in [
            vec![],
            vec!["a.cpp".into(), "a.cpp".into()],
            vec!["b.C".into()],
            vec!["unknown.cpp".into()],
            vec!["api.HPP".into()],
            vec!["a.cpp".into(), "b.C".into()],
        ] {
            assert!(
                validate_selection(&units, &sources, &profile).is_err(),
                "{units:?}"
            );
        }
        assert!(git::is_cpp_header("api.HPP"));
        assert!(git::is_cpp_header("api.H"));
        assert!(!git::is_cpp_header("b.C"));
    }
}
