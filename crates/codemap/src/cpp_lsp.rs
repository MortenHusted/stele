//! A guarded C++ resolution session under one explicit compilation profile.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::cpp_admission::{AdmittedCall, CompiledBody, admit_call};
use crate::cpp_evidence::{SemanticTokens, SymbolInfo};
use crate::cpp_inputs::{CapturedInputs, InputManifest};
use crate::cpp_ownership::OwnerSource;
use crate::cpp_profile::{
    CompilationProfile, ProfileRoots, load_profile, logical_roots, path_option,
};
#[cfg(all(test, target_os = "macos"))]
use crate::cpp_runtime::runtime_path as pinned_path;
use crate::cpp_runtime::{CppRuntimeManifest, CppRuntimeWitness};
use crate::cpp_syntax::{RawFile, ScopeIndex, extract_file};
use crate::git::{is_cpp_header, is_source_path};
use crate::lsp::{self, Transport};
use crate::runtime::RuntimeManifest;
#[cfg(test)]
use crate::runtime::RuntimeRole;
#[cfg(all(test, target_os = "macos"))]
use crate::runtime::{CapturedRuntime, guard_policy};
use crate::schema::{CallSiteCategory, Language, Span};

pub struct CppSemanticResult {
    pub bodies: Vec<CompiledBody>,
    pub calls: Vec<AdmittedCall>,
    pub withheld_bodies: usize,
    pub withheld_calls: usize,
    pub profile_sha256: String,
    pub inputs_sha256: String,
    pub runtime_sha256: String,
    pub policy: &'static str,
    pub clangd_sha256: String,
    pub observations: Vec<CppCallObservation>,
}

pub struct CppCallObservation {
    pub uri: String,
    pub span: Span,
    pub category: CallSiteCategory,
}

pub const POLICY: &str = crate::schema::CPP_SEMANTIC_POLICY;

pub struct CppSession {
    transport: Transport,
    _workspace: tempfile::TempDir,
    inputs: CapturedInputs,
    runtime: CppRuntimeWitness,
    database: PathBuf,
    roots: ProfileRoots,
    profile: CompilationProfile,
    sources: BTreeMap<String, String>,
    parsed: BTreeMap<String, RawFile>,
    capabilities: Value,
}

struct UnitEvidence {
    relative: String,
    tokens: SemanticTokens,
    bodies: Vec<(Option<SymbolInfo>, Option<Vec<SymbolInfo>>)>,
    calls: Vec<CallEvidence>,
    inactive: Vec<Span>,
}

struct CallEvidence {
    info: Option<SymbolInfo>,
    ambiguous: bool,
}

impl CppSession {
    /// Revalidate original commands; no preconstructed profile can bypass admission.
    pub fn start(
        database: &Path,
        roots: &ProfileRoots,
        manifest: &InputManifest,
        runtime: &RuntimeManifest,
        sources: &[(String, Vec<u8>)],
    ) -> Result<Self> {
        Self::start_profile(database, roots, manifest, &runtime.clone().into(), sources)
    }

    pub fn start_profile(
        database: &Path,
        roots: &ProfileRoots,
        manifest: &InputManifest,
        runtime: &CppRuntimeManifest,
        sources: &[(String, Vec<u8>)],
    ) -> Result<Self> {
        runtime.validate_host()?;
        let canonical = canonical_roots(roots)?;
        let roots = &canonical;
        let profile = load_profile(database, roots)?;
        let inputs = CapturedInputs::capture(manifest, roots)?;
        let sources = captured_sources(sources, &inputs)?;
        let source_bytes = sources
            .iter()
            .map(|(path, text)| (path.clone(), text.as_bytes().to_vec()))
            .collect::<Vec<_>>();
        let index = ScopeIndex::build(&source_bytes)?;
        let parsed = source_bytes
            .iter()
            .map(|(path, bytes)| Ok((path.clone(), extract_file(path, bytes, &index)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let workspace = tempfile::tempdir()?;
        let private = fs::canonicalize(workspace.path())?;
        let state = private.join("state");
        fs::create_dir(&state)?;
        let commands =
            prepare_commands(&profile, roots, &sources, &private, &inputs.overlay_path())?;
        fs::write(
            private.join("compile_commands.json"),
            serde_json::to_vec(&commands)?,
        )?;
        inputs.verify_unchanged()?;
        ensure!(
            load_profile(database, roots)? == profile,
            "C++ compilation profile changed before launch"
        );
        let (transport, runtime, capabilities) =
            crate::cpp_runtime::launch(runtime, roots, &state, &private, &inputs)?;
        Ok(Self {
            transport,
            _workspace: workspace,
            inputs,
            runtime,
            database: database.to_path_buf(),
            roots: roots.clone(),
            profile,
            sources,
            parsed,
            capabilities,
        })
    }

    /// A failed query or postcheck returns no partial semantic result.
    pub fn resolve(self) -> Result<CppSemanticResult> {
        self.resolve_with_query_lanes(1)
    }

    pub fn resolve_with_query_lanes(self, query_lanes: usize) -> Result<CppSemanticResult> {
        ensure!(
            (1..=8).contains(&query_lanes),
            "C++ query lanes must be between 1 and 8"
        );
        #[cfg(target_os = "linux")]
        if matches!(&self.runtime, CppRuntimeWitness::Linux { .. }) {
            self.runtime.verify(&self.transport, &self.inputs)?;
            ensure!(
                load_profile(&self.database, &self.roots)? == self.profile,
                "C++ compilation profile changed before queries"
            );
        }
        let evidence = self
            .profile
            .application_units
            .keys()
            .filter(|path| is_source_path(Language::Cpp, path) && !is_cpp_header(path))
            .map(|path| self.query_unit(path, query_lanes))
            .collect::<Result<Vec<_>>>()?;
        let uris = self
            .sources
            .keys()
            .map(|path| {
                (
                    path.clone(),
                    lsp::uri_from_path(&self.roots.source.join(path)),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let owners = self
            .sources
            .iter()
            .map(|(path, text)| OwnerSource {
                uri: &uris[path],
                text,
                raw: &self.parsed[path],
            })
            .collect::<Vec<_>>();
        let mut bodies = Vec::new();
        let mut withheld_bodies = 0;
        for unit in &evidence {
            let source = owners
                .iter()
                .find(|source| source.uri == uris[&unit.relative])
                .context("compiled source is absent")?;
            for (anchor, (info, qualifiers)) in source.raw.bodies.iter().zip(&unit.bodies) {
                let admitted =
                    info.as_ref()
                        .zip(qualifiers.as_ref())
                        .and_then(|(info, qualifiers)| {
                            CompiledBody::admit(
                                source,
                                anchor,
                                &unit.tokens,
                                info.clone(),
                                qualifiers,
                                &owners,
                            )
                        });
                if let Some(body) = admitted {
                    bodies.push(body);
                } else {
                    withheld_bodies += 1;
                }
            }
        }
        let (calls, observations, withheld_calls) =
            admit_calls(&evidence, &owners, &uris, &bodies)?;
        self.runtime.verify(&self.transport, &self.inputs)?;
        ensure!(
            load_profile(&self.database, &self.roots)? == self.profile,
            "C++ compilation profile changed during resolution"
        );
        self.transport.process_id()?;
        self.transport.shutdown();
        Ok(CppSemanticResult {
            bodies,
            calls,
            withheld_bodies,
            withheld_calls,
            profile_sha256: self.profile.fingerprint.clone(),
            inputs_sha256: self.inputs.fingerprint().to_owned(),
            runtime_sha256: self.runtime.fingerprint().to_owned(),
            policy: POLICY,
            clangd_sha256: self.runtime.clangd_sha256()?,
            observations,
        })
    }

    fn query_unit(&self, relative: &str, query_lanes: usize) -> Result<UnitEvidence> {
        let text = self
            .sources
            .get(relative)
            .context("compilation unit is outside selected source inventory")?;
        let raw = &self.parsed[relative];
        ensure!(
            raw.parse_errors == 0,
            "compiled C++ unit has source parse errors"
        );
        let uri = lsp::uri_from_path(&self.roots.source.join(relative));
        self.transport.notify(
            "textDocument/didOpen",
            &json!({"textDocument":{"uri":uri,"languageId":"cpp","version":1,"text":text}}),
        )?;
        let inactive = self.wait_diagnostics(&uri, text)?;
        let response = self.transport.request(
            "textDocument/semanticTokens/full",
            &json!({"textDocument":{"uri":uri}}),
        )?;
        let tokens = SemanticTokens::parse(&self.capabilities, &response, text)?;
        let spans: Vec<_> = raw
            .bodies
            .iter()
            .flat_map(|body| std::iter::once(body.name_span).chain(body.qualifiers.iter().copied()))
            .chain(raw.call_anchors.iter().map(|call| call.name_span))
            .collect();
        let mut answers = self
            .symbol_batch(&uri, text, &spans, query_lanes)?
            .into_iter();
        let mut bodies = Vec::new();
        for body in &raw.bodies {
            let info = answers.next().context("missing body symbol answer")?.info;
            let qualifiers = body
                .qualifiers
                .iter()
                .map(|_| {
                    Ok(answers
                        .next()
                        .context("missing qualifier symbol answer")?
                        .info)
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .collect::<Option<Vec<_>>>();
            bodies.push((info, qualifiers));
        }
        let calls = answers.collect();
        #[cfg(target_os = "linux")]
        if matches!(&self.runtime, CppRuntimeWitness::Linux { .. }) {
            self.transport.notify(
                "textDocument/didClose",
                &json!({"textDocument":{"uri":uri}}),
            )?;
        }
        Ok(UnitEvidence {
            relative: relative.to_owned(),
            tokens,
            bodies,
            calls,
            inactive,
        })
    }

    fn symbol_batch(
        &self,
        uri: &str,
        text: &str,
        spans: &[Span],
        query_lanes: usize,
    ) -> Result<Vec<CallEvidence>> {
        let mut answers = Vec::with_capacity(spans.len());
        for chunk in spans.chunks(query_lanes) {
            let results = std::thread::scope(|scope| {
                let workers: Vec<_> = chunk.iter().map(|span| scope.spawn(|| {
                    let (line, character) = lsp::byte_offset_to_lsp_pos(text.as_bytes(), span.start_byte);
                    let response = self.transport.request("textDocument/symbolInfo", &json!({"textDocument":{"uri":uri},"position":{"line":line,"character":character}}))?;
                    let ambiguous = response.as_array().is_some_and(|rows| rows.len() > 1);
                    Ok(CallEvidence { info: SymbolInfo::parse(&response)?, ambiguous })
                })).collect();
                workers
                    .into_iter()
                    .map(|worker| {
                        worker
                            .join()
                            .map_err(|_| anyhow::anyhow!("C++ symbol query worker panicked"))?
                    })
                    .collect::<Result<Vec<_>>>()
            })?;
            answers.extend(results);
        }
        Ok(answers)
    }

    fn wait_diagnostics(&self, uri: &str, text: &str) -> Result<Vec<Span>> {
        let mut clean = false;
        let mut inactive = None;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let message = self
                .transport
                .unsolicited()?
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
            if message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == uri
            {
                let rows = message["params"]["diagnostics"]
                    .as_array()
                    .context("clangd omitted diagnostics")?;
                ensure!(
                    rows.iter()
                        .all(|row| matches!(row["severity"].as_u64(), Some(2..=4))),
                    "clangd reported erroneous or unclassified diagnostics"
                );
                clean = true;
            }
            if message["method"] == "textDocument/inactiveRegions"
                && message["params"]["textDocument"]["uri"] == uri
            {
                inactive = Some(parse_inactive_regions(&message["params"]["regions"], text)?);
            }
            if clean {
                if let Some(regions) = inactive {
                    return Ok(regions);
                }
            }
        }
    }
}

fn admit_calls(
    evidence: &[UnitEvidence],
    owners: &[OwnerSource<'_>],
    uris: &BTreeMap<String, String>,
    bodies: &[CompiledBody],
) -> Result<(Vec<AdmittedCall>, Vec<CppCallObservation>, usize)> {
    let mut calls = Vec::new();
    let mut withheld_calls = 0;
    let mut observations = Vec::new();
    for unit in evidence {
        let source = owners
            .iter()
            .find(|source| source.uri == uris[&unit.relative])
            .context("compiled source is absent")?;
        for (anchor, evidence) in source.raw.call_anchors.iter().zip(&unit.calls) {
            let inactive = unit.inactive.iter().any(|region| {
                region.start_byte <= anchor.span.start_byte
                    && region.end_byte >= anchor.span.end_byte
            });
            let admitted = (!inactive)
                .then(|| {
                    evidence
                        .info
                        .as_ref()
                        .and_then(|info| admit_call(source, anchor, &unit.tokens, info, bodies))
                })
                .flatten();
            let category = admitted.map_or_else(
                || {
                    withheld_calls += 1;
                    classify_call(source, anchor, evidence, unit, bodies, uris, inactive)
                },
                |call| {
                    calls.push(call);
                    CallSiteCategory::Resolved
                },
            );
            observations.push(CppCallObservation {
                uri: source.uri.to_owned(),
                span: anchor.span,
                category,
            });
        }
        for call in &source.raw.calls {
            if !source
                .raw
                .call_anchors
                .iter()
                .any(|anchor| anchor.span == call.span)
            {
                withheld_calls += 1;
                let inactive = unit.inactive.iter().any(|region| {
                    region.start_byte <= call.span.start_byte
                        && region.end_byte >= call.span.end_byte
                });
                observations.push(CppCallObservation {
                    uri: source.uri.to_owned(),
                    span: call.span,
                    category: if inactive {
                        CallSiteCategory::Inactive
                    } else {
                        CallSiteCategory::Unsupported
                    },
                });
            }
        }
    }
    Ok((calls, observations, withheld_calls))
}

fn classify_call(
    source: &OwnerSource<'_>,
    anchor: &crate::cpp_syntax::CallAnchor,
    evidence: &CallEvidence,
    unit: &UnitEvidence,
    bodies: &[CompiledBody],
    uris: &BTreeMap<String, String>,
    inactive: bool,
) -> CallSiteCategory {
    if inactive {
        return CallSiteCategory::Inactive;
    }
    if anchor.templated
        || anchor.inside_lambda
        || anchor.unevaluated
        || anchor.caller_span.is_none()
        || !matches!(
            anchor.function_kind.as_str(),
            "identifier" | "qualified_identifier" | "field_expression"
        )
        || !matches!(anchor.name_kind.as_str(), "identifier" | "field_identifier")
    {
        return CallSiteCategory::Unsupported;
    }
    if evidence.ambiguous {
        return CallSiteCategory::Ambiguous;
    }
    if let Some(token) =
        crate::cpp_ownership::exact_token(&unit.tokens, source.text, &anchor.name_span)
    {
        let (line, character) =
            lsp::byte_offset_to_lsp_pos(source.text.as_bytes(), anchor.name_span.start_byte);
        if unit
            .tokens
            .call_kind(line, character, token.length)
            .is_none()
        {
            return CallSiteCategory::Unsupported;
        }
    }
    let Some(info) = &evidence.info else {
        return CallSiteCategory::Unmapped;
    };
    if bodies
        .iter()
        .filter(|body| body.info.identity == info.identity)
        .count()
        > 1
    {
        return CallSiteCategory::Ambiguous;
    }
    if let Some((path, _)) = uris
        .iter()
        .find(|(_, uri)| uri.as_str() == info.owner_location().uri())
    {
        if is_cpp_header(path) {
            return CallSiteCategory::Unsupported;
        }
    } else {
        return CallSiteCategory::ExternalCrate;
    }
    CallSiteCategory::Unmapped
}

fn parse_inactive_regions(value: &Value, text: &str) -> Result<Vec<Span>> {
    let mut regions = value
        .as_array()
        .context("clangd omitted inactive regions")?
        .iter()
        .map(|range| {
            let position = |point: &Value| -> Result<(usize, u32)> {
                let line = point["line"]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .context("invalid inactive region line")?;
                let character = point["character"]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .context("invalid inactive region column")?;
                let byte = lsp::lsp_pos_to_byte(text.as_bytes(), line, character);
                ensure!(
                    lsp::byte_offset_to_lsp_pos(text.as_bytes(), byte) == (line, character),
                    "inactive region outside captured source"
                );
                Ok((byte, line))
            };
            let (start, start_line) = position(&range["start"])?;
            let (end, end_line) = position(&range["end"])?;
            ensure!(start < end, "inactive region is empty or reversed");
            let column = |byte| {
                text[..byte]
                    .rfind('\n')
                    .map_or(byte, |newline| byte - newline - 1)
            };
            Ok(Span::from_ts(
                start,
                end,
                (start_line as usize, column(start)),
                (end_line as usize, column(end)),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    regions.sort_by_key(|region| region.start_byte);
    ensure!(
        regions
            .windows(2)
            .all(|pair| pair[0].end_byte <= pair[1].start_byte),
        "inactive regions overlap"
    );
    Ok(regions)
}

fn captured_sources(
    sources: &[(String, Vec<u8>)],
    inputs: &CapturedInputs,
) -> Result<BTreeMap<String, String>> {
    ensure!(
        !sources.is_empty(),
        "C++ selected source inventory is empty"
    );
    let mut selected = BTreeMap::new();
    for (relative, expected) in sources {
        ensure!(
            is_source_path(Language::Cpp, relative),
            "non-C++ source in C++ inventory"
        );
        let bytes = inputs.read(&format!("{{source}}/{relative}"))?;
        ensure!(
            bytes == *expected,
            "selected C++ source differs from captured inputs"
        );
        ensure!(
            selected
                .insert(relative.clone(), String::from_utf8(bytes)?)
                .is_none(),
            "duplicate C++ source inventory path"
        );
    }
    Ok(selected)
}

fn prepare_commands(
    profile: &CompilationProfile,
    roots: &ProfileRoots,
    sources: &BTreeMap<String, String>,
    private: &Path,
    overlay: &Path,
) -> Result<Vec<Value>> {
    let named = logical_roots(roots)?;
    let driver = private.join("clang++");
    fs::write(&driver, "inert codemap C++ driver\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&driver, fs::Permissions::from_mode(0o400))?;
    }
    let mut commands = Vec::new();
    for (relative, command) in &profile.application_units {
        if !is_source_path(Language::Cpp, relative) || is_cpp_header(relative) {
            continue;
        }
        ensure!(
            sources.contains_key(relative),
            "compiled C++ unit is outside selected source inventory"
        );
        validate_explicit_profile(&command.arguments)?;
        let mut arguments = vec![driver.to_string_lossy().into_owned()];
        let mut original = command.arguments[1..].iter();
        while let Some(argument) = original.next() {
            if argument == "-o" {
                original.next().context("missing output filename")?;
                continue;
            }
            arguments.push(materialize(argument, &named));
        }
        arguments.splice(
            1..1,
            [
                "--no-default-config".to_owned(),
                "-ivfsoverlay".to_owned(),
                overlay.to_string_lossy().into_owned(),
            ],
        );
        commands.push(json!({"directory":materialize(&command.directory, &named),"file":roots.source.join(relative),"arguments":arguments}));
    }
    ensure!(
        !commands.is_empty(),
        "compilation profile has no selected C++ units"
    );
    Ok(commands)
}

fn materialize(token: &str, roots: &[(&str, &Path)]) -> String {
    for (name, path) in roots {
        let marker = format!("{{{name}}}");
        if let Some(start) = token.find(&marker) {
            return format!(
                "{}{}{}",
                &token[..start],
                path.display(),
                &token[start + marker.len()..]
            );
        }
    }
    token.to_owned()
}

pub fn canonical_roots(roots: &ProfileRoots) -> Result<ProfileRoots> {
    let directory = |path: &Path| -> Result<PathBuf> {
        let path = fs::canonicalize(path)?;
        ensure!(path.is_dir(), "C++ profile root must be a directory");
        Ok(path)
    };
    Ok(ProfileRoots {
        source: directory(&roots.source)?,
        build: roots.build.as_deref().map(directory).transpose()?,
        sysroot: roots.sysroot.as_deref().map(directory).transpose()?,
        dependencies: roots
            .dependencies
            .iter()
            .map(|(name, path)| Ok((name.clone(), directory(path)?)))
            .collect::<Result<Vec<_>>>()?,
    })
}

fn validate_explicit_profile(arguments: &[String]) -> Result<()> {
    let (mut target, mut sysroot, mut resource, mut language, mut nostdinc, mut nostdincxx) =
        (false, false, false, false, false, false);
    let mut args = arguments.iter().skip(1);
    while let Some(argument) = args.next() {
        if let Some((option, joined)) = path_option(argument) {
            let _value = joined
                .or_else(|| args.next().map(String::as_str))
                .context("C++ path option omitted value")?;
            match option.trim_end_matches('=') {
                "-isysroot" | "--sysroot" => sysroot = true,
                "-resource-dir" => resource = true,
                _ => {}
            }
        } else {
            match argument.as_str() {
                "-x" => {
                    ensure!(
                        matches!(args.next().map(String::as_str), Some("c++" | "c++-header")),
                        "C++ profile language must explicitly be C++"
                    );
                    language = true;
                }
                "-target" | "--target" => {
                    args.next().context("C++ target omitted value")?;
                    target = true;
                }
                "-D" | "-U" | "-arch" => {
                    args.next().context("C++ option omitted value")?;
                }
                "-nostdinc" => nostdinc = true,
                "-nostdinc++" => nostdincxx = true,
                _ if argument.starts_with("--target=") || argument.starts_with("-target=") => {
                    target = true;
                }
                _ => {}
            }
        }
    }
    ensure!(
        language,
        "C++ profile requires explicit -x c++ or -x c++-header"
    );
    ensure!(
        target && sysroot && resource,
        "C++ profile requires explicit target, sysroot and resource directory"
    );
    ensure!(
        nostdinc && nostdincxx,
        "C++ profile must disable default include searches"
    );
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::schema::sha256_hex;

    pub(super) struct Fixture {
        _workspace: tempfile::TempDir,
        pub(super) roots: ProfileRoots,
        pub(super) manifest: InputManifest,
        pub(super) sources: Vec<(String, Vec<u8>)>,
        pub(super) database: PathBuf,
    }

    impl Fixture {
        pub(super) fn new() -> Self {
            let workspace = tempfile::Builder::new()
                .prefix("codemap session é ")
                .tempdir()
                .unwrap();
            let root = fs::canonicalize(workspace.path()).unwrap();
            let files = [
                (
                    "include/ops.hpp",
                    include_str!("../tests/fixtures/cpp_calls/include/ops.hpp"),
                ),
                (
                    "include/typeinfo",
                    include_str!("../tests/fixtures/cpp_calls/include/typeinfo"),
                ),
                (
                    "src/ops.cpp",
                    include_str!("../tests/fixtures/cpp_calls/src/ops.cpp"),
                ),
                (
                    "src/caller.cpp",
                    include_str!("../tests/fixtures/cpp_calls/src/caller.cpp"),
                ),
                (
                    "src/contexts.cpp",
                    include_str!("../tests/fixtures/cpp_calls/src/contexts.cpp"),
                ),
            ];
            let mut sources = Vec::new();
            let mut manifest = BTreeMap::new();
            for (relative, text) in files {
                let path = root.join(relative);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, text).unwrap();
                manifest.insert(
                    format!("{{source}}/{relative}"),
                    sha256_hex(text.as_bytes()),
                );
                if is_source_path(Language::Cpp, relative) {
                    sources.push((relative.to_owned(), text.as_bytes().to_vec()));
                }
            }
            let driver = root.join("clang++");
            fs::write(&driver, "#!/bin/sh\ntouch TARGET_EXECUTED\n").unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&driver, fs::Permissions::from_mode(0o755)).unwrap();
            }
            fs::write(
                root.join(".clangd"),
                "CompileFlags:\n  Add: [-include, /does/not/exist.hpp]\n",
            )
            .unwrap();
            let commands = ["src/ops.cpp", "src/caller.cpp", "src/contexts.cpp"].iter().map(|relative| json!({"directory":root,"file":root.join(relative),"arguments":[driver,"-std=c++20","-x","c++","-target","aarch64-unknown-linux-gnu","-isysroot",root,"-resource-dir",root,"-nostdinc","-nostdinc++","-I",root.join("include"),"-c",root.join(relative)]})).collect::<Vec<_>>();
            let database = root.join("compile_commands.json");
            fs::write(&database, serde_json::to_vec(&commands).unwrap()).unwrap();
            Self {
                _workspace: workspace,
                roots: ProfileRoots {
                    source: root,
                    build: None,
                    sysroot: None,
                    dependencies: Vec::new(),
                },
                manifest: InputManifest { files: manifest },
                sources,
                database,
            }
        }

        fn commit(&self) {
            let git = |args: &[&str]| {
                let output = Command::new("git")
                    .arg("-C")
                    .arg(&self.roots.source)
                    .args(args)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            };
            git(&["init", "-q"]);
            git(&[
                "remote",
                "add",
                "origin",
                "https://example.test/cpp-fixture.git",
            ]);
            git(&["add", "."]);
            git(&[
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "-qm",
                "fixture",
            ]);
        }
    }

    #[test]
    fn inactive_regions_require_exact_utf16_ranges() {
        let text = "é😀()\nsecond\n";
        let range = json!({"start":{"line":0,"character":1},"end":{"line":0,"character":3}});
        let regions = parse_inactive_regions(&json!([range]), text).unwrap();
        assert_eq!((regions[0].start_byte, regions[0].end_byte), (2, 6));
        assert_eq!((regions[0].start_col, regions[0].end_col), (2, 6));
        for invalid in [
            Value::Null,
            json!([{"start":{"line":0,"character":2},"end":{"line":0,"character":3}}]),
            json!([{"start":{"line":0,"character":3},"end":{"line":0,"character":1}}]),
            json!([{"start":{"line":0,"character":1},"end":{"line":3,"character":0}}]),
            json!([range, range]),
        ] {
            assert!(parse_inactive_regions(&invalid, text).is_err());
        }
    }

    #[test]
    fn commands_keep_search_order_and_use_only_our_overlay_and_inert_driver() {
        let fixture = Fixture::new();
        let profile = load_profile(&fixture.database, &fixture.roots).unwrap();
        let inputs = CapturedInputs::capture(&fixture.manifest, &fixture.roots).unwrap();
        let sources = captured_sources(&fixture.sources, &inputs).unwrap();
        let private = tempfile::tempdir().unwrap();
        let commands = prepare_commands(
            &profile,
            &fixture.roots,
            &sources,
            private.path(),
            &inputs.overlay_path(),
        )
        .unwrap();
        assert_eq!(commands.len(), 3);
        let argv = commands[0]["arguments"].as_array().unwrap();
        assert_eq!(argv[0], private.path().join("clang++").to_str().unwrap());
        assert!(argv.iter().any(|value| value == "-ivfsoverlay"));
        assert!(!argv.iter().any(|value| value == "-o"));
        assert_eq!(
            materialize("-DMACRO={0}", &logical_roots(&fixture.roots).unwrap()),
            "-DMACRO={0}"
        );
        for option in [
            "-target",
            "-isysroot",
            "-resource-dir",
            "-nostdinc",
            "-nostdinc++",
        ] {
            let mut invalid = profile.clone();
            for command in invalid.application_units.values_mut() {
                command.arguments.retain(|argument| argument != option);
            }
            assert!(
                prepare_commands(
                    &invalid,
                    &fixture.roots,
                    &sources,
                    private.path(),
                    &inputs.overlay_path()
                )
                .is_err()
            );
        }
        assert!(!fixture.roots.source.join("TARGET_EXECUTED").exists());
    }

    #[test]
    fn source_inventory_requires_exact_captured_bytes_and_cpp_purity() {
        let fixture = Fixture::new();
        let inputs = CapturedInputs::capture(&fixture.manifest, &fixture.roots).unwrap();
        let mut sources = fixture.sources.clone();
        sources[0].1.push(b' ');
        assert!(captured_sources(&sources, &inputs).is_err());
        assert!(captured_sources(&[("other.py".to_owned(), Vec::new())], &inputs).is_err());
        let mut sources = fixture.sources;
        sources.push(sources[0].clone());
        assert!(captured_sources(&sources, &inputs).is_err());
    }

    #[test]
    fn framework_paths_are_normalized_and_outside_roots_are_rejected() {
        let fixture = Fixture::new();
        let mut commands: Value =
            serde_json::from_slice(&fs::read(&fixture.database).unwrap()).unwrap();
        for command in commands.as_array_mut().unwrap() {
            let args = command["arguments"].as_array_mut().unwrap();
            args.splice(
                1..1,
                [
                    json!("-iframework"),
                    json!(fixture.roots.source.join("frameworks")),
                ],
            );
        }
        fs::write(&fixture.database, serde_json::to_vec(&commands).unwrap()).unwrap();
        let profile = load_profile(&fixture.database, &fixture.roots).unwrap();
        assert!(profile.application_units.values().all(|command| {
            command
                .arguments
                .contains(&"{source}/frameworks".to_owned())
        }));
        commands[0]["arguments"][2] = json!("/outside/frameworks");
        fs::write(&fixture.database, serde_json::to_vec(&commands).unwrap()).unwrap();
        assert!(load_profile(&fixture.database, &fixture.roots).is_err());
    }

    #[test]
    fn joined_sysroot_and_explicit_language_follow_the_command_parser() {
        let fixture = Fixture::new();
        let mut commands: Value =
            serde_json::from_slice(&fs::read(&fixture.database).unwrap()).unwrap();
        for command in commands.as_array_mut().unwrap() {
            let args = command["arguments"].as_array_mut().unwrap();
            let position = args.iter().position(|value| value == "-isysroot").unwrap();
            let joined = format!("-isysroot{}", args[position + 1].as_str().unwrap());
            args.splice(position..position + 2, [json!(joined)]);
        }
        fs::write(&fixture.database, serde_json::to_vec(&commands).unwrap()).unwrap();
        let profile = load_profile(&fixture.database, &fixture.roots).unwrap();
        for command in profile.application_units.values() {
            validate_explicit_profile(&command.arguments).unwrap();
            let mut no_language = command.arguments.clone();
            let position = no_language.iter().position(|value| value == "-x").unwrap();
            no_language.drain(position..position + 2);
            assert!(validate_explicit_profile(&no_language).is_err());
            let mut c_language = command.arguments.clone();
            c_language[position + 1] = "c".to_owned();
            assert!(validate_explicit_profile(&c_language).is_err());
        }
    }

    #[test]
    fn checkout_aliases_share_the_canonical_overlay_and_document_paths() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let alias_root = tempfile::tempdir().unwrap();
        let alias = alias_root.path().join("checkout");
        symlink(&fixture.roots.source, &alias).unwrap();
        let mut roots = fixture.roots.clone();
        roots.source = alias;
        let canonical = canonical_roots(&roots).unwrap();
        assert_eq!(canonical.source, fixture.roots.source);
        let profile = load_profile(&fixture.database, &canonical).unwrap();
        let inputs = CapturedInputs::capture(&fixture.manifest, &canonical).unwrap();
        let sources = captured_sources(&fixture.sources, &inputs).unwrap();
        let private = tempfile::tempdir().unwrap();
        let commands = prepare_commands(
            &profile,
            &canonical,
            &sources,
            private.path(),
            &inputs.overlay_path(),
        )
        .unwrap();
        assert_eq!(
            commands[0]["directory"],
            fixture.roots.source.to_str().unwrap()
        );
        assert!(commands.iter().all(|command| {
            command["file"]
                .as_str()
                .unwrap()
                .starts_with(fixture.roots.source.to_str().unwrap())
        }));
    }

    #[test]
    #[ignore = "requires explicit pinned native C++ runtime"]
    #[allow(clippy::too_many_lines)] // One end-to-end fixture verifies the guarded canonical result.
    pub(super) fn canonical_extraction_matches_the_independent_labelled_calls() {
        let mut fixture = Fixture::new();
        let (_, contexts) = fixture
            .sources
            .iter_mut()
            .find(|(path, _)| path == "src/contexts.cpp")
            .unwrap();
        contexts.extend_from_slice(
            b"\n#if 0\nvoid inactive_call() { ops::pick(1); } // site:inactive\n#endif\n",
        );
        fs::write(fixture.roots.source.join("src/contexts.cpp"), &*contexts).unwrap();
        fixture
            .manifest
            .files
            .insert("{source}/src/contexts.cpp".into(), sha256_hex(contexts));
        let context_text = String::from_utf8(contexts.clone()).unwrap();
        let (_, header) = fixture
            .sources
            .iter_mut()
            .find(|(path, _)| path == "include/ops.hpp")
            .unwrap();
        let guarded = format!(
            "#ifndef CODEMAP_FIXTURE_OPS\n#define CODEMAP_FIXTURE_OPS\n{}\n#endif\n",
            String::from_utf8_lossy(header)
        );
        *header = guarded.into_bytes();
        fs::write(fixture.roots.source.join("include/ops.hpp"), &*header).unwrap();
        fixture
            .manifest
            .files
            .insert("{source}/include/ops.hpp".into(), sha256_hex(header));
        for (path, bytes) in [
            (
                "src/unselected.cpp",
                b"#include \"ops.hpp\"\nvoid unselected() { ops::pick(1); }\n".as_slice(),
            ),
            (
                "include/unselected.HPP",
                b"inline void header_call() { ops::pick(1); }\n".as_slice(),
            ),
        ] {
            fs::write(fixture.roots.source.join(path), bytes).unwrap();
            fixture
                .manifest
                .files
                .insert(format!("{{source}}/{path}"), sha256_hex(bytes));
            fixture.sources.push((path.to_owned(), bytes.to_vec()));
        }
        fixture.commit();
        let runtime: CppRuntimeManifest = serde_json::from_slice(
            &fs::read(std::env::var("CODEMAP_CPP_RUNTIME_MANIFEST").unwrap()).unwrap(),
        )
        .unwrap();
        let config = crate::cpp_snapshot::CppExtractConfig {
            compilation_database: fixture.database.clone(),
            translation_units: vec![
                "src/ops.cpp".into(),
                "src/caller.cpp".into(),
                "src/contexts.cpp".into(),
            ],
            query_lanes: 1,
            roots: fixture.roots.clone(),
            inputs: fixture.manifest.clone(),
            runtime,
        };
        let options = crate::extract::ExtractOptions {
            path: fixture.roots.source.clone(),
            language: Language::Cpp,
            calls_required: true,
            ..crate::extract::ExtractOptions::default()
        };
        let snapshot = crate::cpp_snapshot::extract_cpp(&options, &config).unwrap();
        assert!(snapshot.identity.is_semantic());
        let mut parallel = config.clone();
        parallel.query_lanes = 4;
        let repeated = crate::cpp_snapshot::extract_cpp_worktree(&options, &parallel).unwrap();
        assert_eq!(
            crate::extract::to_json(&snapshot).unwrap(),
            crate::extract::to_json(&repeated).unwrap()
        );
        assert_canonical_fixture(&snapshot, &fixture);
        assert_canonical_cli(&snapshot, &options, &config);
        for (path, expected) in [
            ("src/unselected.cpp", CallSiteCategory::Unprofiled),
            ("include/unselected.HPP", CallSiteCategory::Unsupported),
        ] {
            let sites: Vec<_> = snapshot
                .call_sites
                .iter()
                .filter(|site| site.rel_path == path)
                .collect();
            assert_eq!(sites.len(), 1);
            assert_eq!(sites[0].category, expected);
        }
        let inactive = snapshot
            .call_sites
            .iter()
            .find(|site| {
                site.rel_path == "src/contexts.cpp"
                    && context_text
                        .lines()
                        .nth(site.span.start_line as usize - 1)
                        .unwrap()
                        .contains("site:inactive")
            })
            .unwrap();
        assert_eq!(inactive.category, CallSiteCategory::Inactive);
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.provenance.fact_source == crate::schema::FactSource::Clangd)
        );
        assert!(!fixture.roots.source.join("TARGET_EXECUTED").exists());
        if let Some(path) = std::env::var_os("CODEMAP_CPP_CANONICAL_SNAPSHOT_OUT") {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .unwrap();
            std::io::Write::write_all(
                &mut file,
                crate::extract::to_json(&snapshot).unwrap().as_bytes(),
            )
            .unwrap();
        }
    }

    fn assert_canonical_cli(
        expected: &crate::schema::Snapshot,
        options: &crate::extract::ExtractOptions,
        config: &crate::cpp_snapshot::CppExtractConfig,
    ) {
        let binary = std::env::var("CODEMAP_CPP_CLI")
            .expect("set CODEMAP_CPP_CLI to the built stele binary");
        let output_dir = tempfile::tempdir().unwrap();
        let profile = output_dir.path().join("profile.json");
        let output = output_dir.path().join("snapshot.json");
        fs::write(&profile, serde_json::to_vec(config).unwrap()).unwrap();
        let run = || {
            Command::new(&binary)
                .args([
                    "code",
                    "extract",
                    "--language",
                    "cpp",
                    "--calls",
                    "required",
                    "--path",
                ])
                .arg(&options.path)
                .arg("--cpp-profile")
                .arg(&profile)
                .arg("--out")
                .arg(&output)
                .output()
                .unwrap()
        };
        let result = run();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let actual: crate::schema::Snapshot =
            serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
        assert_eq!(
            crate::extract::to_json(expected).unwrap(),
            crate::extract::to_json(&actual).unwrap()
        );
        fs::remove_file(&output).unwrap();
        let mut missing = config.clone();
        missing.translation_units.pop();
        fs::write(&profile, serde_json::to_vec(&missing).unwrap()).unwrap();
        let rejected = run();
        assert!(!rejected.status.success());
        assert!(
            String::from_utf8_lossy(&rejected.stderr)
                .contains("explicitly selected translation units")
        );
        assert!(
            !output.exists(),
            "failed selection must not write a partial snapshot"
        );
    }

    fn assert_canonical_fixture(snapshot: &crate::schema::Snapshot, fixture: &Fixture) {
        let oracle: Value =
            serde_json::from_str(include_str!("../tests/fixtures/cpp_calls/expected.json"))
                .unwrap();
        let expected = oracle["canonical"].as_array().unwrap();
        let calls: Vec<_> = snapshot
            .edges
            .iter()
            .filter(|edge| edge.kind == crate::schema::EdgeKind::Calls)
            .collect();
        assert_eq!(calls.len(), expected.len());
        let mut seen = std::collections::BTreeSet::new();
        for call in calls {
            assert_eq!(
                call.provenance.fact_source,
                crate::schema::FactSource::Clangd
            );
            let text = String::from_utf8_lossy(
                &fixture
                    .sources
                    .iter()
                    .find(|(path, _)| *path == call.provenance.rel_path)
                    .unwrap()
                    .1,
            );
            let line = text
                .lines()
                .nth(call.provenance.span.start_line as usize - 1)
                .unwrap();
            let label = expected
                .iter()
                .find(|label| {
                    line.trim_end()
                        .ends_with(&format!("site:{}", label["site"].as_str().unwrap()))
                })
                .expect("canonical edge must have an independent positive label");
            assert!(seen.insert(label["site"].as_str().unwrap()));
            let caller = snapshot
                .nodes
                .iter()
                .find(|node| node.id == call.from)
                .unwrap();
            let target = snapshot
                .nodes
                .iter()
                .find(|node| node.id == call.to)
                .unwrap();
            assert!(
                caller
                    .symbol_key
                    .rsplit_once('#')
                    .unwrap()
                    .1
                    .starts_with(&format!("{}(", label["caller"].as_str().unwrap()))
            );
            assert!(
                target
                    .symbol_key
                    .ends_with(&format!("#{}", label["callee"].as_str().unwrap()))
            );
            let body_text = String::from_utf8_lossy(
                &fixture
                    .sources
                    .iter()
                    .find(|(path, _)| *path == target.provenance.rel_path)
                    .unwrap()
                    .1,
            );
            assert!(
                body_text
                    .lines()
                    .nth(target.provenance.span.start_line as usize - 1)
                    .unwrap()
                    .trim_end()
                    .ends_with(&format!("def:{}", label["definition"].as_str().unwrap()))
            );
        }
        assert_eq!(seen.len(), expected.len());
    }

    #[test]
    #[ignore = "requires explicit pinned native C++ runtime"]
    pub(super) fn production_session_matches_the_independent_labelled_calls() {
        let fixture = Fixture::new();
        let runtime_path =
            std::env::var("CODEMAP_CPP_RUNTIME_MANIFEST").expect("explicit runtime manifest");
        let runtime: CppRuntimeManifest =
            serde_json::from_slice(&fs::read(runtime_path).unwrap()).unwrap();
        let result = CppSession::start_profile(
            &fixture.database,
            &fixture.roots,
            &fixture.manifest,
            &runtime,
            &fixture.sources,
        )
        .unwrap()
        .resolve()
        .unwrap();
        let oracle: Value =
            serde_json::from_str(include_str!("../tests/fixtures/cpp_calls/expected.json"))
                .unwrap();
        let expected = oracle["canonical"].as_array().unwrap();
        assert_eq!(result.calls.len(), expected.len());
        let mut seen = std::collections::BTreeSet::new();
        for edge in &result.calls {
            let caller = &result.bodies[edge.caller];
            let target = &result.bodies[edge.target];
            let path = lsp::path_from_uri(&caller.uri);
            let source = fixture
                .sources
                .iter()
                .find(|(relative, _)| fixture.roots.source.join(relative) == Path::new(&path))
                .unwrap();
            let line = String::from_utf8_lossy(&source.1)
                .lines()
                .nth(edge.span.start_line as usize - 1)
                .unwrap()
                .to_owned();
            let label = expected
                .iter()
                .find(|label| {
                    line.trim_end()
                        .ends_with(&format!("site:{}", label["site"].as_str().unwrap()))
                })
                .expect("every admitted edge has an independent positive site label");
            let name = |body: &CompiledBody| {
                body.definition
                    .scope
                    .iter()
                    .chain(std::iter::once(&body.definition.name))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("::")
            };
            let expected_caller = label["caller"].as_str().unwrap();
            assert!(name(caller).starts_with(&format!("{expected_caller}(")));
            assert_eq!(name(target), label["callee"].as_str().unwrap());
            let target_path = lsp::path_from_uri(&target.uri);
            let target_source = fixture
                .sources
                .iter()
                .find(|(relative, _)| {
                    fixture.roots.source.join(relative) == Path::new(&target_path)
                })
                .unwrap();
            let expected_definition_line = String::from_utf8_lossy(&target_source.1)
                .lines()
                .position(|line| {
                    line.trim_end()
                        .ends_with(&format!("// def:{}", label["definition"].as_str().unwrap()))
                })
                .unwrap();
            assert_eq!(
                target.name_span.start_line as usize - 1,
                expected_definition_line
            );
            assert!(seen.insert(label["site"].as_str().unwrap().to_owned()));
        }
        assert_eq!(seen.len(), expected.len());
        assert_eq!(result.profile_sha256.len(), 64);
        assert_eq!(result.inputs_sha256.len(), 64);
        assert_eq!(result.runtime_sha256.len(), 64);
        assert!(!fixture.roots.source.join("TARGET_EXECUTED").exists());
    }

    #[test]
    #[ignore = "requires explicit pinned native C++ runtime"]
    #[allow(clippy::too_many_lines)] // Compare three complete guarded extractions under one Git tree.
    #[allow(clippy::literal_string_with_formatting_args)] // Manifest keys contain literal logical roots.
    pub(super) fn canonical_identity_tracks_defines_and_generated_headers() {
        use codemap_graph::schema::{EdgeKind, FactSource};

        let mut fixture = Fixture::new();
        let build = tempfile::tempdir().unwrap();
        let build_root = fs::canonicalize(build.path()).unwrap();
        fixture.roots.build = Some(build_root.clone());
        let source = b"#include \"generated.hpp\"\nint first() { return 1; }\nint second() { return 2; }\nint choose() {\n#if CHOICE\nreturn first();\n#else\nreturn second();\n#endif\n}\n";
        fs::write(fixture.roots.source.join("src/choice.cpp"), source).unwrap();
        fixture
            .manifest
            .files
            .insert("{source}/src/choice.cpp".into(), sha256_hex(source));
        let header = build_root.join("generated.hpp");
        let header_bytes = b"#ifndef CHOICE\n#define CHOICE 0\n#endif\n";
        fs::write(&header, header_bytes).unwrap();
        fixture
            .manifest
            .files
            .insert("{build}/generated.hpp".into(), sha256_hex(header_bytes));
        let root = &fixture.roots.source;
        let mut commands = json!([{"directory":root,"file":root.join("src/choice.cpp"),"arguments":[root.join("clang++"),"-std=c++20","-x","c++","-target","aarch64-unknown-linux-gnu","-isysroot",root,"-resource-dir",root,"-nostdinc","-nostdinc++","-I",build_root,"-c",root.join("src/choice.cpp")]}]);
        fixture.database = build_root.join("compile_commands.json");
        let baseline_database = serde_json::to_vec(&commands).unwrap();
        fs::write(&fixture.database, &baseline_database).unwrap();
        fixture.commit();
        let runtime: CppRuntimeManifest = serde_json::from_slice(
            &fs::read(std::env::var("CODEMAP_CPP_RUNTIME_MANIFEST").unwrap()).unwrap(),
        )
        .unwrap();
        let mut config = crate::cpp_snapshot::CppExtractConfig {
            compilation_database: fixture.database.clone(),
            translation_units: vec!["src/choice.cpp".into()],
            query_lanes: 1,
            roots: fixture.roots.clone(),
            inputs: fixture.manifest.clone(),
            runtime,
        };
        let options = crate::extract::ExtractOptions {
            path: fixture.roots.source.clone(),
            language: Language::Cpp,
            calls_required: true,
            ..crate::extract::ExtractOptions::default()
        };
        let baseline = crate::cpp_snapshot::extract_cpp(&options, &config).unwrap();
        commands[0]["arguments"]
            .as_array_mut()
            .unwrap()
            .push(json!("-DCHOICE=1"));
        fs::write(&fixture.database, serde_json::to_vec(&commands).unwrap()).unwrap();
        let defined = crate::cpp_snapshot::extract_cpp(&options, &config).unwrap();
        fs::write(&fixture.database, baseline_database).unwrap();
        let changed_header = b"#ifndef CHOICE\n#define CHOICE 1\n#endif\n";
        fs::write(&header, changed_header).unwrap();
        let stale = crate::cpp_snapshot::extract_cpp(&options, &config).unwrap_err();
        assert!(format!("{stale:#}").contains("digest"), "{stale:#}");
        config
            .inputs
            .files
            .insert("{build}/generated.hpp".into(), sha256_hex(changed_header));
        let generated = crate::cpp_snapshot::extract_cpp(&options, &config).unwrap();
        for (snapshot, target) in [
            (&baseline, "second()"),
            (&defined, "first()"),
            (&generated, "first()"),
        ] {
            codemap_graph::validate(snapshot).unwrap();
            assert!(!snapshot.identity.dirty);
            assert_eq!(snapshot.identity.commit_sha, baseline.identity.commit_sha);
            assert_eq!(snapshot.identity.tree_sha, baseline.identity.tree_sha);
            let calls: Vec<_> = snapshot
                .edges
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Calls)
                .collect();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].provenance.fact_source, FactSource::Clangd);
            assert_eq!(
                snapshot
                    .nodes
                    .iter()
                    .find(|node| node.id == calls[0].from)
                    .unwrap()
                    .name,
                "choose()"
            );
            assert_eq!(
                snapshot
                    .nodes
                    .iter()
                    .find(|node| node.id == calls[0].to)
                    .unwrap()
                    .name,
                target
            );
        }
        let identity = baseline.identity.cpp_semantic.as_ref().unwrap();
        let define_identity = defined.identity.cpp_semantic.as_ref().unwrap();
        let header_identity = generated.identity.cpp_semantic.as_ref().unwrap();
        assert_ne!(identity.profile_sha256, define_identity.profile_sha256);
        assert_eq!(identity.inputs_sha256, define_identity.inputs_sha256);
        assert_eq!(identity.profile_sha256, header_identity.profile_sha256);
        assert_ne!(identity.inputs_sha256, header_identity.inputs_sha256);
        assert_ne!(baseline.identity.snapshot_id, defined.identity.snapshot_id);
        assert_ne!(
            baseline.identity.snapshot_id,
            generated.identity.snapshot_id
        );
        assert!(!fixture.roots.source.join("TARGET_EXECUTED").exists());
    }

    #[test]
    #[ignore = "requires explicit pinned native C++ runtime"]
    #[cfg(target_os = "macos")]
    fn production_guard_denies_network_execution_and_outside_writes() {
        use std::io::ErrorKind;
        use std::net::{TcpListener, TcpStream};

        if let Ok(state) = std::env::var("CODEMAP_CPP_GUARD_PROBE_STATE") {
            let outside = std::env::var("CODEMAP_CPP_GUARD_PROBE_OUTSIDE").unwrap();
            let address = std::env::var("CODEMAP_CPP_GUARD_PROBE_ADDRESS").unwrap();
            assert_eq!(
                fs::write(&outside, "changed").unwrap_err().kind(),
                ErrorKind::PermissionDenied
            );
            assert_eq!(
                Command::new("/usr/bin/false").status().unwrap_err().kind(),
                ErrorKind::PermissionDenied
            );
            assert_eq!(
                TcpStream::connect(address).unwrap_err().kind(),
                ErrorKind::PermissionDenied
            );
            fs::write(Path::new(&state).join("allowed"), "private state").unwrap();
            return;
        }
        let manifest: RuntimeManifest = serde_json::from_slice(
            &fs::read(std::env::var("CODEMAP_CPP_RUNTIME_MANIFEST").unwrap()).unwrap(),
        )
        .unwrap();
        let runtime = CapturedRuntime::capture(&manifest).unwrap();
        let sandbox = pinned_path(&runtime, &RuntimeRole::Sandbox).unwrap();
        let workspace = tempfile::Builder::new()
            .prefix("codemap guard é ")
            .tempdir()
            .unwrap();
        let root = fs::canonicalize(workspace.path()).unwrap();
        let state = root.join("state");
        fs::create_dir(&state).unwrap();
        let outside = root.join("outside");
        fs::write(&outside, "unchanged").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let binary = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let output = Command::new(sandbox)
            .env_clear()
            .env("HOME", &state)
            .env("TMPDIR", &state)
            .env("CODEMAP_CPP_GUARD_PROBE_STATE", &state)
            .env("CODEMAP_CPP_GUARD_PROBE_OUTSIDE", &outside)
            .env(
                "CODEMAP_CPP_GUARD_PROBE_ADDRESS",
                listener.local_addr().unwrap().to_string(),
            )
            .current_dir(&state)
            .args(["-p", &guard_policy(&state, &binary).unwrap()])
            .arg(binary)
            .args([
                "--exact",
                "cpp_lsp::tests::production_guard_denies_network_execution_and_outside_writes",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read_to_string(outside).unwrap(), "unchanged");
        assert_eq!(
            fs::read_to_string(state.join("allowed")).unwrap(),
            "private state"
        );
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        runtime.verify_unchanged().unwrap();
    }

    #[test]
    #[ignore = "requires explicit pinned native C++ runtime"]
    pub(super) fn production_session_fails_when_the_owned_tool_exits() {
        let fixture = Fixture::new();
        let runtime: CppRuntimeManifest = serde_json::from_slice(
            &fs::read(std::env::var("CODEMAP_CPP_RUNTIME_MANIFEST").unwrap()).unwrap(),
        )
        .unwrap();
        let session = CppSession::start_profile(
            &fixture.database,
            &fixture.roots,
            &fixture.manifest,
            &runtime,
            &fixture.sources,
        )
        .unwrap();
        let pid = session.transport.process_id().unwrap();
        let killed = Command::new("/bin/kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        assert!(session.resolve().is_err());
        assert!(!fixture.roots.source.join("TARGET_EXECUTED").exists());
        for (index, mut invalid) in [runtime.clone(), runtime].into_iter().enumerate() {
            let (path, digest) = match &mut invalid {
                CppRuntimeManifest::Darwin(manifest) => {
                    let pin = manifest
                        .files
                        .values_mut()
                        .find(|pin| pin.role == RuntimeRole::Clangd)
                        .unwrap();
                    (&mut pin.path, &mut pin.sha256)
                }
                #[cfg(target_os = "linux")]
                CppRuntimeManifest::Linux(profile) => {
                    let pin = profile
                        .runtime
                        .files
                        .values_mut()
                        .find(|pin| pin.role == crate::linux_runtime::LinuxRuntimeRole::Clangd)
                        .unwrap();
                    (&mut pin.path, &mut pin.sha256)
                }
            };
            if index == 0 {
                *path = fixture.roots.source.join("missing-clangd");
            } else {
                *digest = "0".repeat(64);
            }
            assert!(
                CppSession::start_profile(
                    &fixture.database,
                    &fixture.roots,
                    &fixture.manifest,
                    &invalid,
                    &fixture.sources
                )
                .is_err()
            );
        }
    }

    #[test]
    #[ignore = "requires explicit pinned native C++ runtime"]
    pub(super) fn production_session_rejects_original_input_drift() {
        let fixture = Fixture::new();
        let runtime_path =
            std::env::var("CODEMAP_CPP_RUNTIME_MANIFEST").expect("explicit runtime manifest");
        let runtime: CppRuntimeManifest =
            serde_json::from_slice(&fs::read(runtime_path).unwrap()).unwrap();
        let session = CppSession::start_profile(
            &fixture.database,
            &fixture.roots,
            &fixture.manifest,
            &runtime,
            &fixture.sources,
        )
        .unwrap();
        let header = fixture.roots.source.join("include/ops.hpp");
        let mut text = fs::read_to_string(&header).unwrap();
        text.push_str("\n// changed original input\n");
        fs::write(header, text).unwrap();
        let error = session
            .resolve()
            .err()
            .expect("drift must return no semantic result");
        assert!(
            format!("{error:#}").contains("C++ input changed"),
            "{error:#}"
        );
        assert!(!fixture.roots.source.join("TARGET_EXECUTED").exists());
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "cpp_linux_source_tests.rs"]
mod linux_source_tests;
