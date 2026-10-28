//! Python structure from the owned, captured `CPython` parser; syntax never admits calls.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path};

use anyhow::{Context, Result, ensure};

use crate::extract::{ExtractOptions, build_report};
use crate::git::{self, GitState};
use crate::python_parser::{
    ByteSpan, Definition, ParsedFile, ParserConfig, SourceExpression, line_starts, parse_sources,
};
use crate::python_resolve::ResolvedCall;
use crate::schema::{
    CallSite, CallSiteCategory, Edge, EdgeKind, FactSource, FileRecord, Language, Node, NodeKind,
    PYTHON_PARSER_POLICY, Provenance, PythonDefinition, PythonExpression, PythonInputs, Snapshot,
    SnapshotIdentity, Span, ToolVersions, TypeKind, edge_id, node_id, node_id_disambiguated,
    sort_edges, sort_files, sort_nodes, symbol_key,
};

pub fn extract_python(options: &ExtractOptions, config: &ParserConfig) -> Result<Snapshot> {
    extract_current(options, config, false)
}

pub fn extract_python_worktree(
    options: &ExtractOptions,
    config: &ParserConfig,
) -> Result<Snapshot> {
    extract_current(options, config, true)
}

fn extract_current(
    options: &ExtractOptions,
    config: &ParserConfig,
    worktree: bool,
) -> Result<Snapshot> {
    ensure!(
        options.language == Language::Python,
        "Python parser requires --language python"
    );
    ensure!(
        !options.calls_required,
        "Python calls require an explicit captured native profile"
    );
    ensure!(
        !options.measures && !options.twins,
        "Python measures, clones and twins have no accepted policy"
    );
    let state = if worktree {
        git::gather_worktree(&options.path, Language::Python)?
    } else {
        git::gather(&options.path, Language::Python)?
    };
    let snapshot = from_state(&state, config)?;
    git::verify_unchanged(&state).context("repository changed during Python extraction")?;
    Ok(snapshot)
}

/// Source-only committed extraction reads immutable Git blobs, never target modules.
pub fn extract_python_at_commit(
    path: &Path,
    commit: &str,
    config: &ParserConfig,
) -> Result<Snapshot> {
    from_state(&git::gather_at(path, commit, Language::Python)?, config)
}

fn from_state(state: &GitState, config: &ParserConfig) -> Result<Snapshot> {
    let sources = read_sources(state)?;
    let parsed = parse_sources(config, Path::new(&state.repo_root), &sources)?;
    let identity = SnapshotIdentity::build(
        Language::Python,
        state.identity.clone(),
        state.commit_sha.clone(),
        state.tree_sha.clone(),
        ToolVersions::syntax_python(),
        state.dirty,
    )
    .with_python(PythonInputs {
        parser_profile_sha256: parsed.fingerprint,
        policy: PYTHON_PARSER_POLICY.to_owned(),
        native: None,
    });
    assemble(identity, &sources, &parsed.files)
}

pub(crate) fn read_sources(state: &GitState) -> Result<BTreeMap<String, Vec<u8>>> {
    let sources: BTreeMap<_, _> = if state.worktree {
        read_python_worktree_files(state)?
    } else {
        state
            .source_files
            .iter()
            .cloned()
            .zip(git::read_files_at_bounded(
                Path::new(&state.repo_root),
                &state.commit_sha,
                &state.source_files,
                8 * 1024 * 1024,
                32 * 1024 * 1024,
            )?)
            .collect()
    }
    .into_iter()
    .collect();
    Ok(sources)
}

fn read_python_worktree_files(state: &GitState) -> Result<Vec<(String, Vec<u8>)>> {
    let root = Path::new(&state.repo_root).canonicalize()?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(root)?;
    let mut remaining = 32 * 1024 * 1024;
    let mut sources = Vec::new();
    for path in &state.source_files {
        let file = open_source(&directory, path)?;
        ensure!(
            file.metadata()?.is_file(),
            "Python source must be a regular file: {path}"
        );
        let limit = remaining.min(8 * 1024 * 1024);
        ensure!(
            file.metadata()?.len() <= limit as u64,
            "Python source budget exceeded: {path}"
        );
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= limit,
            "Python source budget exceeded: {path}"
        );
        remaining -= bytes.len();
        sources.push((path.clone(), bytes));
    }
    Ok(sources)
}

fn open_source(root: &File, path: &str) -> Result<File> {
    let components: Vec<_> = Path::new(path).components().collect();
    ensure!(!components.is_empty(), "empty Python source path");
    let mut directory = root.try_clone()?;
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            anyhow::bail!("Python source path must be repository relative: {path}");
        };
        let name = CString::new(name.as_encoded_bytes())?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if index + 1 == components.len() {
                libc::O_NONBLOCK
            } else {
                libc::O_DIRECTORY
            };
        // Each component is opened relative to an owned directory descriptor;
        // O_NOFOLLOW refuses source links and linked ancestors even during replacement.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("open unlinked Python source {path}"));
        }
        // openat returned a new owned descriptor; File closes it on every exit.
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

fn assemble(
    identity: SnapshotIdentity,
    sources: &BTreeMap<String, Vec<u8>>,
    parsed: &[ParsedFile],
) -> Result<Snapshot> {
    assemble_native(identity, sources, parsed, &[])
}

#[allow(clippy::too_many_lines)] // One file pass keeps AST owner indices bound to emitted node identities.
pub(crate) fn assemble_native(
    identity: SnapshotIdentity,
    sources: &BTreeMap<String, Vec<u8>>,
    parsed: &[ParsedFile],
    resolved: &[ResolvedCall],
) -> Result<Snapshot> {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut files = Vec::new();
    let mut sites = Vec::new();
    let mut body_ids = BTreeMap::new();
    let by_path: BTreeMap<_, _> = parsed
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let lines: BTreeMap<_, _> = parsed
        .iter()
        .map(|file| (file.path.as_str(), line_starts(&sources[&file.path])))
        .collect();
    let resolved_sites: BTreeSet<_> = resolved
        .iter()
        .map(|call| {
            let file = by_path[call.path.as_str()];
            let span = &file.calls[call.call].span;
            (call.path.as_str(), span.start_byte, span.end_byte)
        })
        .collect();
    for file in parsed {
        let declaration_only =
            Path::new(&file.path).extension() == Some(std::ffi::OsStr::new("pyi"));
        let raw = &sources[&file.path];
        let source = Source {
            identity: &identity,
            path: &file.path,
            hash: &file.sha256,
            lines: &lines[file.path.as_str()],
        };
        let file_id = node_id(Language::Python, NodeKind::File, &file.path, &[], "");
        let file_span = source.span(&ByteSpan {
            start_byte: 0,
            end_byte: raw.len(),
            start_line: 1,
            end_line: u32::try_from(source.lines.len())?,
        })?;
        nodes.push(Node {
            python: None,
            id: file_id.clone(),
            kind: NodeKind::File,
            name: file.path.clone(),
            symbol_key: file_id.clone(),
            provenance: source.provenance(file_span),
        });
        let module_id = node_id(
            Language::Python,
            NodeKind::Module,
            &file.path,
            &[],
            &file.path,
        );
        nodes.push(Node {
            python: None,
            id: module_id.clone(),
            kind: NodeKind::Module,
            name: file.path.clone(),
            symbol_key: symbol_key(Language::Python, &file.path, &[], &file.path),
            provenance: source.provenance(file_span),
        });
        edges.push(source.edge(EdgeKind::Contains, &file_id, &module_id, file_span));
        files.push(FileRecord {
            rel_path: file.path.clone(),
            sha256: file.sha256.clone(),
            byte_len: raw.len(),
        });
        let mut ids: Vec<String> = Vec::new();
        let mut scopes: Vec<Vec<String>> = Vec::new();
        for (index, definition) in file
            .definitions
            .iter()
            .enumerate()
            .filter(|_| !declaration_only)
        {
            let mut scope = definition
                .owner
                .map_or_else(Vec::new, |owner| scopes[owner].clone());
            let name = if definition.ambiguous {
                format!("{}@{}", definition.name, definition.ordinal)
            } else {
                definition.name.clone()
            };
            let kind = definition_kind(definition, &file.definitions);
            let span = source.span(&definition.span)?;
            let id = node_id_disambiguated(
                Language::Python,
                kind,
                &file.path,
                &scope,
                &name,
                span.start_byte,
            );
            let key = symbol_key(Language::Python, &file.path, &scope, &name);
            scope.push(name);
            scopes.push(scope);
            let parent = definition
                .owner
                .map_or(module_id.as_str(), |owner| ids[owner].as_str());
            nodes.push(Node {
                id: id.clone(),
                kind,
                name: definition.name.clone(),
                symbol_key: key,
                provenance: source.provenance(span),
                python: Some(PythonDefinition {
                    is_async: definition.kind == "AsyncFunctionDef",
                    ordinal: definition.ordinal,
                    ambiguous: definition.ambiguous,
                    decorators: definition
                        .decorators
                        .iter()
                        .map(|value| source.expression(value))
                        .collect::<Result<_>>()?,
                    bases: definition
                        .bases
                        .iter()
                        .map(|value| source.expression(value))
                        .collect::<Result<_>>()?,
                }),
            });
            edges.push(source.edge(EdgeKind::Contains, parent, &id, span));
            body_ids.insert((file.path.as_str(), index), id.clone());
            ids.push(id);
        }
        let mut import_ordinals = BTreeMap::new();
        for import in file.imports.iter().filter(|_| !declaration_only) {
            let scope = import
                .owner
                .map_or_else(Vec::new, |owner| scopes[owner].clone());
            let ordinal = import_ordinals
                .entry((import.owner, &import.spelling))
                .or_insert(0_usize);
            let name = format!("{}@{ordinal}", import.spelling);
            *ordinal += 1;
            let span = source.span(&import.span)?;
            let id = node_id_disambiguated(
                Language::Python,
                NodeKind::ImportRef,
                &file.path,
                &scope,
                &name,
                span.start_byte,
            );
            let parent = import
                .owner
                .map_or(module_id.as_str(), |owner| ids[owner].as_str());
            nodes.push(Node {
                python: None,
                id: id.clone(),
                kind: NodeKind::ImportRef,
                name: import.spelling.clone(),
                symbol_key: symbol_key(Language::Python, &file.path, &scope, &name),
                provenance: source.provenance(span),
            });
            edges.push(source.edge(EdgeKind::Contains, parent, &id, span));
            edges.push(source.edge(EdgeKind::Imports, parent, &id, span));
        }
        for call in &file.calls {
            sites.push(CallSite {
                rel_path: file.path.clone(),
                span: source.span(&call.span)?,
                raw_name: call.callee.clone(),
                category: if resolved_sites.contains(&(
                    file.path.as_str(),
                    call.span.start_byte,
                    call.span.end_byte,
                )) {
                    CallSiteCategory::Resolved
                } else if identity.is_semantic() {
                    CallSiteCategory::Unsupported
                } else {
                    CallSiteCategory::Observed
                },
            });
        }
    }
    for call in resolved {
        let file = by_path[call.path.as_str()];
        let source = Source {
            identity: &identity,
            path: &file.path,
            hash: &file.sha256,
            lines: &lines[file.path.as_str()],
        };
        let mut edge = source.edge(
            EdgeKind::Calls,
            &body_ids[&(call.path.as_str(), call.caller)],
            &body_ids[&(call.target_path.as_str(), call.target)],
            source.span(&file.calls[call.call].span)?,
        );
        edge.provenance.fact_source = FactSource::Mypyc;
        edges.push(edge);
    }
    sort_nodes(&mut nodes);
    sort_edges(&mut edges);
    sort_files(&mut files);
    sites.sort_by(|left, right| {
        (&left.rel_path, left.span.start_byte, left.span.end_byte).cmp(&(
            &right.rel_path,
            right.span.start_byte,
            right.span.end_byte,
        ))
    });
    let report = build_report(&files, &nodes, &edges, &sites, 0, 0, BTreeMap::new());
    let snapshot = Snapshot {
        identity,
        files,
        nodes,
        edges,
        call_sites: sites,
        report,
        measures: vec![],
        file_measures: vec![],
        clone_groups: vec![],
        twin_pairs: vec![],
    };
    crate::validate(&snapshot)?;
    Ok(snapshot)
}

fn definition_kind(definition: &Definition, definitions: &[Definition]) -> NodeKind {
    if definition.kind == "ClassDef" {
        NodeKind::Type(TypeKind::Class)
    } else if definition
        .owner
        .is_some_and(|owner| definitions[owner].kind == "ClassDef")
    {
        NodeKind::Method
    } else {
        NodeKind::Function
    }
}

struct Source<'a> {
    identity: &'a SnapshotIdentity,
    path: &'a str,
    hash: &'a str,
    lines: &'a [usize],
}

impl Source<'_> {
    fn span(&self, span: &ByteSpan) -> Result<Span> {
        Ok(Span {
            start_byte: span.start_byte,
            end_byte: span.end_byte,
            start_line: span.start_line,
            end_line: span.end_line,
            start_col: u32::try_from(span.start_byte - self.lines[span.start_line as usize - 1])?,
            end_col: u32::try_from(span.end_byte - self.lines[span.end_line as usize - 1])?,
        })
    }

    fn provenance(&self, span: Span) -> Provenance {
        Provenance {
            repository: self.identity.repository.id.clone(),
            commit_sha: self.identity.commit_sha.clone(),
            rel_path: self.path.to_owned(),
            span,
            file_hash: self.hash.to_owned(),
            extractor_version: self.identity.extractor_version.clone(),
            fact_source: Language::Python.structural_fact_source(),
            confidence: u8::MAX,
        }
    }

    fn expression(&self, expression: &SourceExpression) -> Result<PythonExpression> {
        Ok(PythonExpression {
            spelling: expression.spelling.clone(),
            provenance: self.provenance(self.span(&expression.span)?),
        })
    }

    fn edge(&self, kind: EdgeKind, from: &str, to: &str, span: Span) -> Edge {
        Edge {
            id: edge_id(kind, from, to, span.start_byte),
            kind,
            from: from.to_owned(),
            to: to.to_owned(),
            provenance: self.provenance(span),
        }
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    fn declaration_projection() -> (Snapshot, serde_json::Value) {
        let labels: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../../../toolchain/python/declaration_projection_oracle.json"
        ))
        .unwrap();
        let oracle: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../../../toolchain/python/native_oracle.json"
        ))
        .unwrap();
        let case = oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == labels["source_case"])
            .unwrap();
        let sources = case["files"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(path, raw)| (path.clone(), raw.as_str().unwrap().as_bytes().to_vec()))
            .collect();
        let parsed: Vec<ParsedFile> = serde_json::from_slice(include_bytes!(
            "../../../toolchain/python/declaration_census_fixture.json"
        ))
        .unwrap();
        let mut tools = ToolVersions::syntax_python();
        tools.mypyc = Some("2.3.1".to_owned());
        let identity = SnapshotIdentity::build(
            Language::Python,
            crate::schema::RepoIdentity {
                id: "fixture".to_owned(),
                origin: None,
                is_canonical: false,
            },
            "a".repeat(40),
            "b".repeat(40),
            tools,
            false,
        )
        .with_python(PythonInputs {
            parser_profile_sha256: "1".repeat(64),
            policy: PYTHON_PARSER_POLICY.to_owned(),
            native: Some(crate::schema::PythonNativeInputs {
                profile_sha256: "2".repeat(64),
                evidence_sha256: "3".repeat(64),
                policy: crate::schema::PYTHON_NATIVE_POLICY.to_owned(),
            }),
        });
        (assemble(identity, &sources, &parsed).unwrap(), labels)
    }

    #[test]
    fn declared_inputs_preserve_census_without_implementation_nodes() {
        let (snapshot, labels) = declaration_projection();
        let path = labels["declarations"][0].as_str().unwrap();
        let mut kinds: Vec<_> = snapshot
            .nodes
            .iter()
            .filter(|node| node.provenance.rel_path == path)
            .map(|node| node.kind.tag())
            .collect();
        kinds.sort_unstable();
        assert_eq!(serde_json::json!(kinds), labels["expected_node_kinds"]);
        let sites: Vec<_> = snapshot
            .call_sites
            .iter()
            .filter(|site| site.rel_path == path)
            .collect();
        assert_eq!(
            sites.len(),
            labels["expected_call_sites"].as_array().unwrap().len()
        );
        for (site, expected) in sites
            .iter()
            .zip(labels["expected_call_sites"].as_array().unwrap())
        {
            assert_eq!(
                site.span.start_byte,
                expected["span"]["start_byte"].as_u64().unwrap() as usize
            );
            assert_eq!(
                site.span.end_byte,
                expected["span"]["end_byte"].as_u64().unwrap() as usize
            );
            assert_eq!(serde_json::json!(site.category), expected["category"]);
        }
        assert_eq!(
            snapshot
                .edges
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Calls)
                .count(),
            labels["expected_call_edges"].as_array().unwrap().len()
        );
    }

    #[test]
    fn declaration_schema_refuses_bodies_resolved_sites_and_syntax_profiles() {
        let (snapshot, labels) = declaration_projection();
        let path = labels["declarations"][0].as_str().unwrap();
        let stub = snapshot
            .nodes
            .iter()
            .position(|node| node.provenance.rel_path == path && node.kind == NodeKind::Module)
            .unwrap();
        let metadata = snapshot
            .nodes
            .iter()
            .find_map(|node| node.python.clone())
            .unwrap();
        for kind in [
            NodeKind::Function,
            NodeKind::Method,
            NodeKind::Type(TypeKind::Class),
            NodeKind::ImportRef,
        ] {
            let mut invalid = snapshot.clone();
            invalid.nodes[stub].kind = kind;
            invalid.nodes[stub].python = (kind != NodeKind::ImportRef).then(|| metadata.clone());
            assert!(
                crate::validate(&invalid)
                    .unwrap_err()
                    .to_string()
                    .contains("declaration input contains a body/import node")
            );
        }
        let mut invalid = snapshot.clone();
        invalid
            .call_sites
            .iter_mut()
            .find(|site| site.rel_path == path)
            .unwrap()
            .category = CallSiteCategory::Resolved;
        assert!(
            crate::validate(&invalid)
                .unwrap_err()
                .to_string()
                .contains("declaration call site must remain unsupported")
        );
        let mut invalid = snapshot.clone();
        let caller = invalid
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Function)
            .unwrap();
        let site = invalid
            .call_sites
            .iter()
            .find(|site| site.rel_path != path)
            .unwrap();
        let mut provenance = caller.provenance.clone();
        provenance.span = site.span;
        provenance.fact_source = crate::schema::FactSource::Mypyc;
        invalid.edges.push(Edge {
            id: edge_id(
                EdgeKind::Calls,
                &caller.id,
                &invalid.nodes[stub].id,
                site.span.start_byte,
            ),
            kind: EdgeKind::Calls,
            from: caller.id.clone(),
            to: invalid.nodes[stub].id.clone(),
            provenance,
        });
        assert!(
            crate::validate(&invalid)
                .unwrap_err()
                .to_string()
                .contains("must connect callable bodies")
        );
        let mut invalid = snapshot;
        invalid.identity.tools.mypyc = None;
        invalid.identity.python.as_mut().unwrap().native = None;
        invalid.identity.snapshot_id = invalid.identity.compute_snapshot_id();
        assert!(crate::validate(&invalid).is_err());
    }

    #[test]
    fn original_byte_columns_remain_zero_based() {
        let identity = SnapshotIdentity::build(
            Language::Python,
            crate::schema::RepoIdentity {
                id: "fixture".into(),
                origin: None,
                is_canonical: false,
            },
            "commit".into(),
            "tree".into(),
            ToolVersions::syntax_python(),
            false,
        );
        let raw = "# é\r\n    def nested(): pass\r\n".as_bytes();
        let source = Source {
            identity: &identity,
            path: "main.py",
            hash: "unused",
            lines: &line_starts(raw),
        };
        let start = raw.iter().position(|byte| *byte == b'd').unwrap();
        let span = source
            .span(&ByteSpan {
                start_byte: start,
                end_byte: raw.len() - 2,
                start_line: 2,
                end_line: 2,
            })
            .unwrap();
        assert_eq!(span.start_col, 4);
        assert_eq!(span.end_col, 22);
    }

    #[test]
    fn descriptor_reads_refuse_links_replaced_after_inventory() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::write(external.path().join("main.py"), "pass\n").unwrap();
        let directory = File::open(root.path()).unwrap();
        std::os::unix::fs::symlink(external.path().join("main.py"), root.path().join("main.py"))
            .unwrap();
        assert!(open_source(&directory, "main.py").is_err());
        std::os::unix::fs::symlink(external.path(), root.path().join("package")).unwrap();
        assert!(open_source(&directory, "package/main.py").is_err());
        assert!(open_source(&directory, "../main.py").is_err());
        assert!(open_source(&directory, "/main.py").is_err());
        std::fs::remove_file(root.path().join("main.py")).unwrap();
        std::fs::write(root.path().join("main.py"), "pass\n").unwrap();
        let mut bytes = String::new();
        open_source(&directory, "main.py")
            .unwrap()
            .read_to_string(&mut bytes)
            .unwrap();
        assert_eq!(bytes, "pass\n");
    }
}
