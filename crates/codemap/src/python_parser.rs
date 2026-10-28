//! Captured source-only Python parsing; no snapshots or canonical call admission.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::python_inputs::{CapturedOperator, OperatorManifest, OperatorRoots};
use crate::python_process::{
    OwnedPython, await_marker, check_budget, read_bounded, verify_helpers, write_input,
};
use crate::runtime::{CapturedRuntime, RuntimeManifest, RuntimeRole};
use crate::runtime_images::DarwinImageCatalog;
use crate::schema::sha256_hex;

const BOOTSTRAP: &[u8] = include_bytes!("../../../toolchain/python/parser_bootstrap.py");
const PROBE: &[u8] = include_bytes!("../../../toolchain/python/probe.py");
const LIMIT: u64 = 32 * 1024 * 1024;
const ERROR_LIMIT: u64 = 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParserConfig {
    pub roots: OperatorRoots,
    pub inputs: OperatorManifest,
    pub runtime: RuntimeManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ByteSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub span: ByteSpan,
    pub declaration_only: bool,
    pub owner: Option<usize>,
    pub ordinal: usize,
    pub ambiguous: bool,
    pub decorators: Vec<SourceExpression>,
    pub bases: Vec<SourceExpression>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceExpression {
    pub span: ByteSpan,
    pub spelling: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub owner: Option<usize>,
    pub span: ByteSpan,
    pub callee_span: ByteSpan,
    pub callee: String,
    pub callee_kind: String,
    pub caller: Option<String>,
    pub evaluation_context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Import {
    pub owner: Option<usize>,
    pub span: ByteSpan,
    pub spelling: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParsedFile {
    pub path: String,
    pub sha256: String,
    pub encoding: String,
    pub definitions: Vec<Definition>,
    pub calls: Vec<Call>,
    pub imports: Vec<Import>,
    pub scopes: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Census {
    contract: String,
    version: String,
    status: String,
    files: Vec<ParsedFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    contract: String,
    version: [u8; 3],
    origins: BTreeMap<String, Option<String>>,
}

/// Parser witnesses are exposed only after complete owned execution postchecks.
#[derive(Debug)]
pub struct ParsedSources {
    pub files: Vec<ParsedFile>,
    pub identity: Value,
    pub fingerprint: String,
}

pub fn parse_sources(
    config: &ParserConfig,
    source_root: &Path,
    sources: &BTreeMap<String, Vec<u8>>,
) -> Result<ParsedSources> {
    parse_inputs(config, source_root, sources, &[])
}

/// Parse explicitly selected declaration inputs as data; this establishes no callable bodies.
pub fn parse_declared_sources(
    config: &ParserConfig,
    source_root: &Path,
    sources: &BTreeMap<String, Vec<u8>>,
    declarations: &[String],
) -> Result<ParsedSources> {
    parse_inputs(config, source_root, sources, declarations)
}

#[allow(clippy::too_many_lines)] // One owned process binds preparation, source parsing and all postchecks.
#[allow(clippy::literal_string_with_formatting_args)] // Captured logical root addresses are literal placeholders.
fn parse_inputs(
    config: &ParserConfig,
    source_root: &Path,
    sources: &BTreeMap<String, Vec<u8>>,
    declarations: &[String],
) -> Result<ParsedSources> {
    ensure!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "captured Python parser requires macOS arm64"
    );
    ensure!(
        config.roots.compiler.is_none(),
        "source-only Python parser refuses a compiler root"
    );
    let query = source_request(sources, declarations)?;
    let operator = CapturedOperator::capture(&config.inputs, &config.roots, source_root)?;
    let original_runtime = CapturedRuntime::capture_python(&config.runtime)?;
    ensure!(
        config
            .runtime
            .files
            .values()
            .all(|file| !file.path.starts_with(source_root)),
        "Python runtime must remain outside application source"
    );
    let private = tempfile::tempdir()?;
    let private_root = fs::canonicalize(private.path())?;
    let copied = operator.materialize(&private_root.join("operator"))?;
    let copied_operator = CapturedOperator::capture(&config.inputs, &copied, source_root)?;
    let mut runtime_manifest = config.runtime.clone();
    for file in runtime_manifest.files.values_mut() {
        if file.role == RuntimeRole::Python {
            ensure!(
                file.sha256 == config.inputs.files["{runtime}/bin/python3.14"],
                "parser runtime executable differs from captured operator"
            );
            file.path = copied.runtime.join("bin/python3.14");
        } else if let Ok(relative) = file.path.strip_prefix(&config.roots.runtime) {
            file.path = copied.runtime.join(relative);
        }
    }
    let runtime = CapturedRuntime::capture_python(&runtime_manifest)?;
    ensure!(
        runtime.fingerprint() == original_runtime.fingerprint(),
        "copied Python runtime identity changed"
    );
    let catalog = DarwinImageCatalog::capture(&runtime)?;
    let state = private_root.join("state");
    let helpers = private_root.join("helpers");
    fs::create_dir(&state)?;
    fs::create_dir(&helpers)?;
    let helper_files = [
        (helpers.join("parser_bootstrap.py"), BOOTSTRAP),
        (helpers.join("probe.py"), PROBE),
    ];
    for (path, bytes) in &helper_files {
        fs::write(path, bytes)?;
    }
    let helper_pins: BTreeMap<_, _> = helper_files
        .iter()
        .map(|(path, bytes)| {
            (
                path.file_name().unwrap().to_str().unwrap(),
                sha256_hex(bytes),
            )
        })
        .collect();
    let ready = state.join("ready");
    let done = state.join("done");
    let errors = state.join("stderr");
    let configuration = json!({"runtime":copied.runtime,"files":config.inputs.files,"directories":config.inputs.directories,"helpers":helper_pins,"ready":ready,"done":done});
    let configuration_bytes = serde_json::to_vec(&configuration)?;
    ensure!(
        configuration_bytes.len() as u64 <= LIMIT,
        "Python parser configuration budget exceeded"
    );
    let configuration_path = helpers.join("configuration.json");
    fs::write(&configuration_path, &configuration_bytes)?;
    let mut policy = copied_operator.guard_policy(&state, &runtime)?;
    for path in std::iter::once(&helpers)
        .chain(helper_files.iter().map(|(path, _)| path))
        .chain(std::iter::once(&configuration_path))
    {
        write!(
            policy,
            " (allow file-read* (literal {}))",
            serde_json::to_string(path)?
        )?;
    }
    let policy_path = state.join("guard.sb");
    fs::write(&policy_path, &policy)?;
    let executable = runtime_file(&runtime_manifest, &RuntimeRole::Python)?;
    ensure!(
        executable == copied.runtime.join("bin/python3.14"),
        "parser must select its copied interpreter"
    );
    let mut process = OwnedPython::new(
        Command::new(runtime_file(&runtime_manifest, &RuntimeRole::Sandbox)?)
            .process_group(0)
            .env_clear()
            .env("HOME", &state)
            .env("TMPDIR", &state)
            .current_dir(&state)
            .arg("-f")
            .arg(&policy_path)
            .arg(executable)
            .args(["-I", "-S", "-B"])
            .arg(&helper_files[0].0)
            .arg(&configuration_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&errors)?)
            .spawn()
            .context("launch owned Python parser")?,
    )?;
    let deadline = Instant::now() + DEADLINE;
    let report: Ready = serde_json::from_slice(&await_marker(
        &process,
        &ready,
        &errors,
        deadline,
        ERROR_LIMIT,
    )?)?;
    validate_ready(&report, &config.inputs, &helper_pins)?;
    let images = catalog.inspect(process.pid())?;
    write_input(&mut process, &query, &errors, deadline)?;
    let result_bytes = await_marker(&process, &done, &errors, deadline, LIMIT)?;
    let census: Census = serde_json::from_slice(&result_bytes)?;
    validate_census(&census, sources)?;
    ensure!(
        images == catalog.inspect(process.pid())?,
        "Python parser images changed"
    );
    operator.verify_unchanged()?;
    copied_operator.verify_unchanged()?;
    original_runtime.verify_unchanged()?;
    runtime.verify_unchanged()?;
    verify_helpers(
        &helper_files,
        &configuration_path,
        &configuration_bytes,
        &policy_path,
        policy.as_bytes(),
    )?;
    write_input(&mut process, b"2", &errors, deadline)?;
    while !process.exited()? {
        check_budget(&errors, deadline)?;
        std::thread::sleep(Duration::from_millis(10));
    }
    check_budget(&errors, deadline)?;
    ensure!(
        process.finish()?.success(),
        "Python parser failed after result"
    );
    ensure!(
        read_bounded(&done, LIMIT)? == result_bytes,
        "Python parser result changed"
    );
    operator.verify_unchanged()?;
    copied_operator.verify_unchanged()?;
    original_runtime.verify_unchanged()?;
    runtime.verify_unchanged()?;
    verify_helpers(
        &helper_files,
        &configuration_path,
        &configuration_bytes,
        &policy_path,
        policy.as_bytes(),
    )?;
    let pins: BTreeMap<_, _> = sources
        .iter()
        .map(|(path, bytes)| (path, sha256_hex(bytes)))
        .collect();
    let source_policy = if declarations.is_empty() {
        "original-py-bytes/8MiB-file/32MiB-request/1"
    } else {
        "original-py-and-selected-pyi-bytes/8MiB-file/32MiB-request/1"
    };
    let identity = json!({"contract":"python-parser-profile/1","python_version":[3,14,7],"platform":"darwin-arm64","extractor_version":env!("CARGO_PKG_VERSION"),"operator_fingerprint":operator.fingerprint(),"runtime_fingerprint":runtime.fingerprint(),"helper_pins":helper_pins,"sources":pins,"policies":{"guard":crate::python_inputs::GUARD_POLICY,"materialization":crate::python_inputs::MATERIALIZATION_POLICY,"parser":"isolated-cpython/frozen-codecs/source-syntax-warnings-disabled/2","source":source_policy,"result":"source-census/json-depth-128/32MiB/1","process":"owned-group/wnowait/180s/1","images":"darwin-dyld-before-after/1"}});
    let fingerprint = sha256_hex(&serde_json::to_vec(&identity)?);
    Ok(ParsedSources {
        files: census.files,
        identity,
        fingerprint,
    })
}

fn source_request(sources: &BTreeMap<String, Vec<u8>>, declarations: &[String]) -> Result<Vec<u8>> {
    ensure!(
        sources.len() <= 100_000,
        "Python source count budget exceeded"
    );
    ensure!(
        declarations.windows(2).all(|pair| pair[0] < pair[1])
            && declarations.iter().all(|path| Path::new(path).extension()
                == Some(OsStr::new("pyi"))
                && sources.contains_key(path)),
        "unique sorted declaration paths must select captured .pyi inputs"
    );
    let mut total = 0;
    let mut files = Vec::new();
    for (path, bytes) in sources {
        ensure!(
            !path.is_empty()
                && !path.starts_with('/')
                && !path.contains(['\\', '\0'])
                && (Path::new(path).extension() == Some(OsStr::new("py"))
                    || Path::new(path).extension() == Some(OsStr::new("pyi"))
                        && declarations.binary_search(path).is_ok())
                && path
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != ".."),
            "normalized .py or explicitly selected .pyi source path required"
        );
        total += path.len() + bytes.len();
        ensure!(
            bytes.len() <= 8 * 1024 * 1024 && total <= LIMIT as usize,
            "Python source byte budget exceeded"
        );
        files.push(json!({"path":path,"source_base64":base64::engine::general_purpose::STANDARD.encode(bytes),"sha256":sha256_hex(bytes)}));
    }
    let mut query = vec![b'1'];
    let mut request = json!({"files":files});
    if !declarations.is_empty() {
        request["declarations"] = json!(declarations);
    }
    query.extend(serde_json::to_vec(&request)?);
    query.push(b'\n');
    ensure!(
        query.len() as u64 <= LIMIT,
        "Python parser request budget exceeded"
    );
    Ok(query)
}

fn runtime_file<'a>(manifest: &'a RuntimeManifest, role: &RuntimeRole) -> Result<&'a Path> {
    let files = manifest
        .files
        .values()
        .filter(|file| &file.role == role)
        .collect::<Vec<_>>();
    ensure!(
        files.len() == 1,
        "Python parser runtime must select exactly one {role:?}"
    );
    Ok(&files[0].path)
}

fn validate_ready(
    report: &Ready,
    inputs: &OperatorManifest,
    helpers: &BTreeMap<&str, String>,
) -> Result<()> {
    ensure!(
        report.contract == "python-parser-ready/1"
            && report.version == [3, 14, 7]
            && report.origins.contains_key("probe")
            && report.origins.contains_key("encodings.latin_1"),
        "invalid Python parser ready report"
    );
    for origin in report.origins.values().flatten() {
        ensure!(
            origin == "built-in"
                || origin == "frozen"
                || inputs.files.contains_key(origin)
                || origin
                    .strip_prefix("{helper}/")
                    .is_some_and(|name| helpers.contains_key(name)),
            "unmanifested Python parser module origin"
        );
    }
    Ok(())
}

fn validate_census(census: &Census, sources: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    ensure!(
        census.contract == "codemap-python-source-census/v2"
            && census.version == "3.14.7"
            && census.status == "Parser and scope witnesses only; no target or admission claims",
        "invalid Python parser census contract"
    );
    ensure!(
        census
            .files
            .iter()
            .map(|file| &file.path)
            .eq(sources.keys()),
        "Python parser source inventory changed"
    );
    for file in &census.files {
        let raw = &sources[&file.path];
        let lines = line_starts(raw);
        ensure!(
            file.sha256 == sha256_hex(raw) && !file.encoding.is_empty(),
            "Python parser source pin or encoding mismatch"
        );
        let owners = validate_definitions(
            &file.definitions,
            raw,
            &lines,
            Path::new(&file.path).extension() == Some(OsStr::new("pyi")),
        )?;
        let mut sites = BTreeSet::new();
        for call in &file.calls {
            validate_span(&call.span, raw, &lines)?;
            validate_span(&call.callee_span, raw, &lines)?;
            validate_owner(call.owner, &call.span, &owners)?;
            ensure!(
                call.span.start_byte <= call.callee_span.start_byte
                    && call.callee_span.end_byte <= call.span.end_byte
                    && sites.insert((call.span.start_byte, call.span.end_byte)),
                "invalid or duplicate Python call witness"
            );
            ensure!(
                matches!(
                    call.evaluation_context.as_str(),
                    "body"
                        | "module_body"
                        | "class_body"
                        | "definition_setup"
                        | "annotation"
                        | "type_parameter"
                        | "type_alias"
                        | "lambda"
                        | "comprehension"
                ),
                "unknown Python call evaluation context"
            );
        }
        for import in &file.imports {
            validate_span(&import.span, raw, &lines)?;
            validate_owner(import.owner, &import.span, &owners)?;
        }
    }
    Ok(())
}

fn validate_definitions(
    definitions: &[Definition],
    raw: &[u8],
    lines: &[usize],
    declaration_only: bool,
) -> Result<BTreeMap<usize, Option<usize>>> {
    let mut owners = BTreeMap::from([(0, None)]);
    let mut stack: Vec<usize> = Vec::new();
    let mut counts = BTreeMap::new();
    let mut ordinals = BTreeMap::new();
    for definition in definitions {
        *counts
            .entry((definition.owner, &definition.name))
            .or_insert(0) += 1;
    }
    let mut previous_start = None;
    for (index, definition) in definitions.iter().enumerate() {
        validate_span(&definition.span, raw, lines)?;
        ensure!(
            definition.declaration_only == declaration_only
                && !definition.name.is_empty()
                && matches!(
                    definition.kind.as_str(),
                    "FunctionDef" | "AsyncFunctionDef" | "ClassDef"
                )
                && (definition.kind == "ClassDef" || definition.bases.is_empty()),
            "invalid Python implementation witness"
        );
        let mut start = definition.span.start_byte;
        let mut decorator_end = 0;
        for decorator in &definition.decorators {
            validate_expression(decorator, raw, lines)?;
            ensure!(
                decorator_end <= decorator.span.start_byte
                    && decorator.span.end_byte <= definition.span.start_byte,
                "invalid Python decorator order or span"
            );
            start = start.min(decorator.span.start_byte);
            decorator_end = decorator.span.end_byte;
        }
        for base in &definition.bases {
            validate_expression(base, raw, lines)?;
            ensure!(
                contains(&definition.span, &base.span),
                "Python base outside class"
            );
        }
        ensure!(
            previous_start.is_none_or(|previous| previous < start),
            "Python definitions are not in source order"
        );
        previous_start = Some(start);
        while stack
            .last()
            .is_some_and(|parent| definitions[*parent].span.end_byte <= start)
        {
            let parent = stack.pop().expect("checked nonempty stack");
            owners.insert(definitions[parent].span.end_byte, stack.last().copied());
        }
        ensure!(
            definition.owner == stack.last().copied(),
            "invalid Python lexical parent"
        );
        let expected_name = if let Some(parent) = definition.owner {
            ensure!(
                definitions[parent].span.start_byte <= start
                    && definitions[parent].span.end_byte >= definition.span.end_byte,
                "Python child outside lexical parent"
            );
            format!("{}.{}", definitions[parent].qualified_name, definition.name)
        } else {
            definition.name.clone()
        };
        ensure!(
            definition.qualified_name == expected_name,
            "invalid Python qualified ownership"
        );
        let key = (definition.owner, &definition.name);
        let ordinal = ordinals.entry(key).or_insert(0);
        ensure!(
            definition.ordinal == *ordinal && definition.ambiguous == (counts[&key] > 1),
            "invalid Python repeated-definition witness"
        );
        *ordinal += 1;
        owners.insert(start, Some(index));
        stack.push(index);
    }
    while let Some(index) = stack.pop() {
        owners.insert(definitions[index].span.end_byte, stack.last().copied());
    }
    Ok(owners)
}

const fn contains(outer: &ByteSpan, inner: &ByteSpan) -> bool {
    outer.start_byte <= inner.start_byte && inner.end_byte <= outer.end_byte
}

fn validate_expression(expression: &SourceExpression, raw: &[u8], lines: &[usize]) -> Result<()> {
    validate_span(&expression.span, raw, lines)?;
    ensure!(
        !expression.spelling.is_empty()
            && expression.sha256
                == sha256_hex(&raw[expression.span.start_byte..expression.span.end_byte]),
        "Python expression original-byte pin mismatch"
    );
    Ok(())
}

fn validate_owner(
    owner: Option<usize>,
    span: &ByteSpan,
    owners: &BTreeMap<usize, Option<usize>>,
) -> Result<()> {
    let expected = owners
        .range(..=span.start_byte)
        .next_back()
        .and_then(|(_, owner)| *owner);
    ensure!(
        owner == expected,
        "invalid Python observation lexical owner"
    );
    if let Some((boundary, _)) = owners
        .range((
            std::ops::Bound::Excluded(span.start_byte),
            std::ops::Bound::Unbounded,
        ))
        .next()
    {
        ensure!(
            span.end_byte <= *boundary,
            "Python observation crosses lexical owners"
        );
    }
    Ok(())
}

pub(crate) fn validate_span(span: &ByteSpan, raw: &[u8], lines: &[usize]) -> Result<()> {
    ensure!(
        span.start_byte < span.end_byte && span.end_byte <= raw.len(),
        "Python witness span outside original source"
    );
    ensure!(
        lines.partition_point(|start| *start <= span.start_byte) == span.start_line as usize
            && lines.partition_point(|start| *start <= span.end_byte) == span.end_line as usize,
        "Python witness line/byte span mismatch"
    );
    Ok(())
}

pub(crate) fn line_starts(raw: &[u8]) -> Vec<usize> {
    let mut starts = vec![0];
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'\r' {
            if index + 1 < raw.len() && raw[index + 1] == b'\n' {
                index += 1;
            }
            starts.push(index + 1);
        } else if raw[index] == b'\n' {
            starts.push(index + 1);
        }
        index += 1;
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_validation_refuses_corrupt_ownership_and_expression_records() {
        let raw = b"def outer():\n    @decorate()\n    def inner():\n        target()\n";
        let lines = line_starts(raw);
        let span = |needle: &[u8], end: usize| {
            let start = raw
                .windows(needle.len())
                .position(|value| value == needle)
                .unwrap();
            ByteSpan {
                start_byte: start,
                end_byte: end,
                start_line: u32::try_from(lines.partition_point(|value| *value <= start)).unwrap(),
                end_line: u32::try_from(lines.partition_point(|value| *value <= end)).unwrap(),
            }
        };
        let decorator_span = span(b"decorate()", 28);
        assert_eq!(
            &raw[decorator_span.start_byte..decorator_span.end_byte],
            b"decorate()"
        );
        let decorator = SourceExpression {
            sha256: sha256_hex(&raw[decorator_span.start_byte..decorator_span.end_byte]),
            spelling: "decorate()".to_owned(),
            span: decorator_span,
        };
        let outer = Definition {
            name: "outer".to_owned(),
            qualified_name: "outer".to_owned(),
            kind: "FunctionDef".to_owned(),
            span: span(b"def outer", raw.len() - 1),
            declaration_only: false,
            owner: None,
            ordinal: 0,
            ambiguous: false,
            decorators: vec![],
            bases: vec![],
        };
        let inner = Definition {
            name: "inner".to_owned(),
            qualified_name: "outer.inner".to_owned(),
            kind: "FunctionDef".to_owned(),
            span: span(b"def inner", raw.len() - 1),
            declaration_only: false,
            owner: Some(0),
            ordinal: 0,
            ambiguous: false,
            decorators: vec![decorator],
            bases: vec![],
        };
        let definitions = vec![outer, inner];
        let owners = validate_definitions(&definitions, raw, &lines, false).unwrap();
        assert!(validate_definitions(&definitions, raw, &lines, true).is_err());
        let mut declared = definitions.clone();
        for definition in &mut declared {
            definition.declaration_only = true;
        }
        validate_definitions(&declared, raw, &lines, true).unwrap();
        assert!(validate_definitions(&declared, raw, &lines, false).is_err());
        declared[1].declaration_only = false;
        assert!(validate_definitions(&declared, raw, &lines, true).is_err());
        let call = span(b"target()", raw.len() - 1);
        validate_owner(Some(1), &call, &owners).unwrap();
        assert!(validate_owner(Some(0), &call, &owners).is_err());
        assert!(validate_owner(None, &call, &owners).is_err());
        for corruption in 0..8 {
            let mut invalid = definitions.clone();
            match corruption {
                0 => invalid[1].owner = Some(usize::MAX),
                1 => invalid[1].owner = None,
                2 => invalid[1].ordinal = 1,
                3 => invalid[1].ambiguous = true,
                4 => invalid[1].qualified_name = "inner".to_owned(),
                5 => invalid[1].decorators[0].sha256 = "0".repeat(64),
                6 => invalid[1].span.end_byte -= 1,
                7 => invalid[0].bases = invalid[1].decorators.clone(),
                _ => unreachable!(),
            }
            if corruption == 6 {
                // A parent shorter than its child must fail even with valid line coordinates.
                invalid[0].span.end_byte -= 2;
            }
            assert!(
                validate_definitions(&invalid, raw, &lines, false).is_err(),
                "corruption {corruption}"
            );
        }
    }

    #[test]
    fn source_request_refuses_declarations_paths_and_byte_budget() {
        let sources = BTreeMap::from([("types.pyi".to_owned(), b"pass\n".to_vec())]);
        let declarations = vec!["types.pyi".to_owned()];
        let request = source_request(&sources, &declarations).unwrap();
        let value: Value = serde_json::from_slice(&request[1..request.len() - 1]).unwrap();
        assert_eq!(value["declarations"], json!(declarations));
        for selection in [
            vec![],
            vec!["absent.pyi".to_owned()],
            vec!["types.pyi".to_owned(), "types.pyi".to_owned()],
            vec!["types.py".to_owned()],
        ] {
            assert!(source_request(&sources, &selection).is_err());
        }
        for path in [
            "types.pyi",
            "../source.py",
            "/source.py",
            "bad//source.py",
            "bad\\source.py",
            "source.PY",
        ] {
            assert!(
                source_request(
                    &BTreeMap::from([(path.to_owned(), b"pass\n".to_vec())]),
                    &[]
                )
                .is_err()
            );
        }
        assert!(
            source_request(
                &BTreeMap::from([("source.py".to_owned(), vec![b' '; 8 * 1024 * 1024 + 1])]),
                &[]
            )
            .is_err()
        );
    }

    #[test]
    fn byte_span_validation_retains_crlf_cr_and_bom_coordinates() {
        let raw = b"\xef\xbb\xbf# header\r\ndef outer():\r    target()\n";
        let start = raw.windows(6).position(|bytes| bytes == b"target").unwrap();
        let mut span = ByteSpan {
            start_byte: start,
            end_byte: start + 8,
            start_line: 3,
            end_line: 3,
        };
        let lines = line_starts(raw);
        validate_span(&span, raw, &lines).unwrap();
        span.start_line = 2;
        assert!(validate_span(&span, raw, &lines).is_err());
        span.start_line = 3;
        span.end_byte = raw.len() + 1;
        assert!(validate_span(&span, raw, &lines).is_err());
    }

    #[test]
    fn waiting_and_backpressured_input_have_deadlines() {
        let private = tempfile::tempdir().unwrap();
        let errors = private.path().join("errors");
        fs::write(&errors, b"").unwrap();
        let mut process = OwnedPython::new(
            Command::new("/bin/sleep")
                .arg("5")
                .process_group(0)
                .stdin(Stdio::piped())
                .spawn()
                .unwrap(),
        )
        .unwrap();
        assert!(
            await_marker(
                &process,
                &private.path().join("absent"),
                &errors,
                Instant::now(),
                LIMIT
            )
            .is_err()
        );
        let began = Instant::now();
        assert!(
            write_input(
                &mut process,
                &vec![b'x'; 1024 * 1024],
                &errors,
                began + Duration::from_millis(100)
            )
            .is_err()
        );
        assert!(began.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn nonregular_or_oversized_results_are_refused() {
        let private = tempfile::tempdir().unwrap();
        assert!(read_bounded(private.path(), LIMIT).is_err());
        let oversized = private.path().join("oversized");
        fs::File::create(&oversized)
            .unwrap()
            .set_len(LIMIT + 1)
            .unwrap();
        assert!(read_bounded(&oversized, LIMIT).is_err());
        let alias = private.path().join("alias");
        std::os::unix::fs::symlink(&oversized, &alias).unwrap();
        assert!(read_bounded(&alias, LIMIT).is_err());
    }
}
