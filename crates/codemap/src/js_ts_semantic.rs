//! Bounded compiler evidence over captured data. This module admits no graph facts.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::js_ts_inputs::{CapturedInputs, InputManifest, InputRoots};
use crate::js_ts_runtime::{CapturedJsTsRuntime, JsTsRuntimeManifest};
use crate::runtime::{CapturedRuntime, RuntimeManifest, RuntimeRole, guard_policy};
use crate::runtime_images::DarwinImageCatalog;
use crate::schema::{JS_TS_SEMANTIC_POLICY, JsTsSemanticInputs, sha256_hex};

const HELPER: &[u8] = include_bytes!("../../../toolchain/js_ts/resolve.mjs");
const COLLECTOR: &[u8] = include_bytes!("../../../toolchain/js_ts/collect_candidates.mjs");
const LOCK: &[u8] = include_bytes!("../../../toolchain/js_ts/package-lock.json");
const COMPILER_SHA: &str = "3ae902c92cc44dace175c0e69e13a4b0899f6983c6121d76b9ab8dd5795e7675";
pub(crate) const OUTPUT_LIMIT: u64 = 128 * 1024 * 1024;
pub(crate) const ERROR_LIMIT: u64 = 1024 * 1024;
pub(crate) const DEADLINE: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperConfig {
    pub roots: InputRoots,
    pub inputs: InputManifest,
    pub runtime: JsTsRuntimeManifest,
    pub bundle: PathBuf,
    pub project: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumedInput {
    pub sha256: String,
    pub bytes: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramFile {
    pub path: String,
    pub declaration: bool,
    pub common_js: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lookup {
    pub operation: String,
    pub path: String,
    pub resolved: String,
    pub result: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramDiagnostics {
    pub options: usize,
    pub global: usize,
    pub syntactic: usize,
}

/// Compiler candidates remain non-authoritative until independent source admission.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerEvidence {
    phase: String,
    schema: String,
    compiler: String,
    node: String,
    pub project: String,
    pub options: Value,
    pub program_files: Vec<ProgramFile>,
    pub inputs: BTreeMap<String, ConsumedInput>,
    pub lookups: Vec<Lookup>,
    pub sites: Vec<Value>,
    pub aliases: Vec<Value>,
    pub diagnostics: Vec<Value>,
    pub program_diagnostics: ProgramDiagnostics,
}

pub struct CapturedEvidence {
    pub evidence: CompilerEvidence,
    pub fingerprint: String,
    pub identity: JsTsSemanticInputs,
    inputs: CapturedInputs,
    runtime: CapturedJsTsRuntime,
}

impl CapturedEvidence {
    pub fn read_source(&self, path: &str) -> Result<&str> {
        self.inputs.read(&format!("{{source}}/{path}"))
    }

    pub fn verify_unchanged(&self) -> Result<()> {
        self.inputs.verify_unchanged()?;
        self.runtime.verify_unchanged()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    phase: String,
    node: String,
    typescript: String,
}

pub fn resolve(config: &HelperConfig) -> Result<CapturedEvidence> {
    config.runtime.validate_host()?;
    validate_project(&config.project)?;
    ensure!(
        config.bundle.is_absolute()
            && fs::canonicalize(&config.bundle)? == config.bundle
            && !config.bundle.starts_with(&config.roots.source),
        "operator bundle must be canonical and outside source"
    );
    ensure!(
        !config
            .runtime
            .node_path()?
            .starts_with(&config.roots.source),
        "Node executable must be outside source"
    );
    ensure!(
        config
            .runtime
            .paths()
            .iter()
            .all(|path| !path.starts_with(&config.roots.source)),
        "operator runtime files must be outside source"
    );
    let inputs = CapturedInputs::capture(&config.inputs, &config.roots)?;
    let runtime = CapturedJsTsRuntime::capture(&config.runtime)?;
    let compiler_path = config
        .bundle
        .join("node_modules/typescript/lib/typescript.js");
    let compiler = read_bundle_file(&compiler_path, 16 * 1024 * 1024)?;
    ensure!(
        sha256_hex(&compiler) == COMPILER_SHA,
        "TypeScript compiler pin mismatch"
    );
    let originals = [
        (config.bundle.join("resolve.mjs"), HELPER),
        (config.bundle.join("collect_candidates.mjs"), COLLECTOR),
        (config.bundle.join("package-lock.json"), LOCK),
        (compiler_path, compiler.as_slice()),
    ];
    verify_files(&originals)?;
    let (evidence, evidence_sha256) = match &runtime {
        CapturedJsTsRuntime::Darwin { runtime, catalog } => {
            run_helper(config, &inputs, runtime, catalog, &compiler, &originals)?
        }
        #[cfg(target_os = "linux")]
        CapturedJsTsRuntime::Linux {
            runtime, kernel, ..
        } => run_linux_helper(config, &inputs, runtime, kernel, &compiler, &originals)?,
    };
    let identity = JsTsSemanticInputs {
        project: config.project.clone(),
        inputs_sha256: inputs.fingerprint().to_owned(),
        runtime_sha256: runtime.fingerprint().to_owned(),
        compiler_sha256: COMPILER_SHA.to_owned(),
        helper_sha256: sha256_hex(&serde_json::to_vec(&(
            "js-ts-helper-bundle/1",
            sha256_hex(HELPER),
            sha256_hex(COLLECTOR),
            sha256_hex(LOCK),
        ))?),
        evidence_sha256,
        node_version: evidence.node.clone(),
        typescript_version: evidence.compiler.clone(),
        policy: JS_TS_SEMANTIC_POLICY.to_owned(),
    };
    let fingerprint = sha256_hex(&serde_json::to_vec(&identity)?);
    Ok(CapturedEvidence {
        evidence,
        fingerprint,
        identity,
        inputs,
        runtime,
    })
}

fn run_helper(
    config: &HelperConfig,
    inputs: &CapturedInputs,
    runtime: &CapturedRuntime,
    catalog: &DarwinImageCatalog,
    compiler: &[u8],
    originals: &[(PathBuf, &[u8])],
) -> Result<(CompilerEvidence, String)> {
    let private = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let private_root = fs::canonicalize(private.path())?;
    let state_root = fs::canonicalize(state.path())?;
    let private_files = [
        (private_root.join("resolve.mjs"), HELPER),
        (private_root.join("collect_candidates.mjs"), COLLECTOR),
        (private_root.join("typescript.cjs"), compiler),
    ];
    for (path, bytes) in &private_files {
        fs::write(path, bytes)?;
    }
    let request = serde_json::to_vec(&json!({
        "schema": "js-ts-helper/1", "project": config.project,
        "data": inputs.helper_data(),
        "origins": {"source": config.roots.source, "typescript": config.roots.typescript},
    }))?;
    let request_path = private_root.join("request.json");
    fs::write(&request_path, &request)?;
    let binary = runtime_file(runtime.manifest(), &RuntimeRole::Node)?;
    let sandbox = runtime_file(runtime.manifest(), &RuntimeRole::Sandbox)?;
    let output = state_root.join("stdout");
    let errors = state_root.join("stderr");
    let mut command = Command::new(sandbox);
    command.args(["-p", &guard_policy(&state_root, binary)?]);
    command
        .arg(binary)
        .args(["--no-addons", "--max-old-space-size=4096"]);
    command.arg(&private_files[0].0).arg(&request_path);
    command
        .env_clear()
        .env("HOME", &state_root)
        .env("TMPDIR", &state_root);
    command.current_dir(&state_root).stdin(Stdio::piped());
    command
        .stdout(fs::File::create(&output)?)
        .stderr(fs::File::create(&errors)?);
    let mut process = OwnedChild(command.spawn().context("launch guarded JS/TS helper")?);
    let deadline = Instant::now() + DEADLINE;
    let ready_bytes = await_frame(&mut process.0, &output, &errors, 0, deadline, 4096)?;
    let ready: Ready = serde_json::from_slice(&ready_bytes)?;
    ensure!(
        ready.phase == "ready" && ready.node == "v22.22.2" && ready.typescript == "5.9.3",
        "invalid helper ready frame"
    );
    let before = catalog.inspect(process.0.id())?;
    process
        .0
        .stdin
        .as_mut()
        .context("helper control missing")?
        .write_all(b"1")?;
    let offset = ready_bytes.len() + 1;
    let result_bytes = await_frame(
        &mut process.0,
        &output,
        &errors,
        offset,
        deadline,
        OUTPUT_LIMIT,
    )?;
    let evidence: CompilerEvidence = serde_json::from_slice(&result_bytes)?;
    validate_evidence(&evidence, &config.project, inputs)?;
    ensure!(
        before == catalog.inspect(process.0.id())?,
        "helper loaded images changed"
    );
    runtime.verify_unchanged()?;
    inputs.verify_unchanged()?;
    verify_files(originals)?;
    verify_files(&private_files)?;
    ensure!(
        fs::read(&request_path)? == request,
        "private request changed"
    );
    finish_helper(
        &mut process.0,
        &output,
        &errors,
        deadline,
        offset + result_bytes.len() + 1,
    )?;
    inputs.verify_unchanged()?;
    runtime.verify_unchanged()?;
    Ok((evidence, sha256_hex(&result_bytes)))
}

fn finish_helper(
    child: &mut Child,
    output: &Path,
    errors: &Path,
    deadline: Instant,
    expected_bytes: usize,
) -> Result<()> {
    child
        .stdin
        .as_mut()
        .context("helper control missing")?
        .write_all(b"2")?;
    loop {
        verify_bounds(output, errors)?;
        if let Some(status) = child.try_wait()? {
            verify_bounds(output, errors)?;
            ensure!(
                status.success(),
                "guarded helper failed: {}",
                fs::read_to_string(errors)?
            );
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "guarded helper deadline exceeded"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    ensure!(
        fs::metadata(output)?.len() == expected_bytes as u64,
        "extra helper output"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_linux_helper(
    config: &HelperConfig,
    inputs: &CapturedInputs,
    runtime: &crate::linux_runtime::CapturedLinuxRuntime,
    kernel: &crate::linux_kernel_images::LinuxKernelPins,
    compiler: &[u8],
    originals: &[(PathBuf, &[u8])],
) -> Result<(CompilerEvidence, String)> {
    use crate::js_ts_linux::{FLAGS, HelperSession};
    use crate::linux_guard::LinuxGuard;
    use crate::linux_process::LinuxLaunch;
    use std::os::unix::fs::PermissionsExt;

    let private = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let private_root = fs::canonicalize(private.path())?;
    let state_root = fs::canonicalize(state.path())?;
    fs::set_permissions(&private_root, fs::Permissions::from_mode(0o700))?;
    fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700))?;
    let private_files = [
        (private_root.join("resolve.mjs"), HELPER),
        (private_root.join("collect_candidates.mjs"), COLLECTOR),
        (private_root.join("typescript.cjs"), compiler),
    ];
    for (path, bytes) in &private_files {
        fs::write(path, bytes)?;
    }
    let request = serde_json::to_vec(&json!({
        "schema": "js-ts-helper/1", "project": config.project,
        "data": inputs.helper_data(),
        "origins": {"source": config.roots.source, "typescript": config.roots.typescript},
    }))?;
    let request_path = private_root.join("request.json");
    fs::write(&request_path, &request)?;
    let allowed_files = private_files
        .iter()
        .map(|(path, _)| path.clone())
        .chain(std::iter::once(request_path.clone()))
        .collect::<Vec<_>>();
    let witnesses = allowed_files
        .iter()
        .map(|path| PrivateFileWitness::capture(path))
        .collect::<Result<Vec<_>>>()?;
    verify_files(&private_files)?;
    ensure!(
        fs::read(&request_path)? == request,
        "private request changed"
    );
    verify_private_witnesses(&witnesses)?;
    runtime.verify_unchanged()?;
    inputs.verify_unchanged()?;
    let guard = LinuxGuard::prepare(runtime, &allowed_files, &[], &state_root)?;
    let openssl = runtime
        .manifest
        .files
        .get(crate::js_ts_runtime::OPENSSL_PIN)
        .context("empty OpenSSL pin missing")?;
    let arguments = FLAGS
        .into_iter()
        .map(Into::into)
        .chain(std::iter::once(
            format!("--openssl-config={}", openssl.path.display()).into(),
        ))
        .chain([
            private_files[0].0.as_os_str().to_owned(),
            request_path.as_os_str().to_owned(),
        ])
        .collect();
    let launch = LinuxLaunch {
        executable: config.runtime.node_path()?.to_path_buf(),
        arguments,
        environment: [
            ("HOME".into(), state_root.as_os_str().to_owned()),
            ("TMPDIR".into(), state_root.as_os_str().to_owned()),
        ]
        .into(),
        directory: state_root,
    };
    let mut session = HelperSession::spawn(launch, guard)?;
    let ready: Ready = serde_json::from_slice(&session.read_frame(4096)?)?;
    ensure!(
        ready.phase == "ready" && ready.node == "v22.22.2" && ready.typescript == "5.9.3",
        "invalid helper ready frame"
    );
    let before = session.images(runtime, kernel)?;
    verify_private_witnesses(&witnesses)?;
    session.control(b'1')?;
    let result_bytes = session.read_frame(OUTPUT_LIMIT)?;
    let evidence: CompilerEvidence = serde_json::from_slice(&result_bytes)?;
    validate_evidence(&evidence, &config.project, inputs)?;
    ensure!(
        before == session.images(runtime, kernel)?,
        "helper loaded images changed"
    );
    runtime.verify_unchanged()?;
    inputs.verify_unchanged()?;
    verify_files(originals)?;
    verify_files(&private_files)?;
    ensure!(
        fs::read(&request_path)? == request,
        "private request changed"
    );
    verify_private_witnesses(&witnesses)?;
    session.finish()?;
    inputs.verify_unchanged()?;
    runtime.verify_unchanged()?;
    verify_private_witnesses(&witnesses)?;
    Ok((evidence, sha256_hex(&result_bytes)))
}

#[cfg(target_os = "linux")]
fn verify_private_witnesses(witnesses: &[PrivateFileWitness]) -> Result<()> {
    witnesses.iter().try_for_each(PrivateFileWitness::verify)
}

#[cfg(target_os = "linux")]
struct PrivateFileWitness {
    path: PathBuf,
    handle: fs::File,
    stamp: crate::linux_runtime::FileStamp,
}

#[cfg(target_os = "linux")]
impl PrivateFileWitness {
    fn capture(path: &Path) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o400))?;
        let handle = fs::File::open(path)?;
        let stamp = crate::linux_runtime::FileStamp::of(&handle.metadata()?);
        let witness = Self {
            path: path.to_owned(),
            handle,
            stamp,
        };
        witness.verify()?;
        Ok(witness)
    }

    fn verify(&self) -> Result<()> {
        use crate::linux_runtime::FileStamp;
        let metadata = fs::symlink_metadata(&self.path)?;
        ensure!(
            metadata.is_file()
                && fs::canonicalize(&self.path)? == self.path
                && FileStamp::of(&metadata) == self.stamp
                && FileStamp::of(&self.handle.metadata()?) == self.stamp,
            "private JS/TS helper input changed"
        );
        Ok(())
    }
}

fn validate_project(project: &str) -> Result<()> {
    ensure!(
        !project.is_empty()
            && !project.contains(['\\', '\0'])
            && project
                .split('/')
                .all(|part| !part.is_empty() && !matches!(part, "." | "..")),
        "project requires a relative source path"
    );
    Ok(())
}

fn validate_evidence(
    evidence: &CompilerEvidence,
    project: &str,
    inputs: &CapturedInputs,
) -> Result<()> {
    ensure!(
        evidence.phase == "result"
            && evidence.schema == "js-ts-helper/1"
            && evidence.node == "v22.22.2"
            && evidence.compiler == "5.9.3"
            && evidence.project == project,
        "invalid helper result identity"
    );
    ensure!(
        evidence
            .inputs
            .contains_key(&format!("{{source}}/{project}")),
        "project config not consumed"
    );
    for (name, pin) in &evidence.inputs {
        let text = inputs.read(name)?;
        ensure!(
            text.len() == pin.bytes && sha256_hex(text.as_bytes()) == pin.sha256,
            "compiler consumed-input mismatch"
        );
    }
    let mut previous: Option<&str> = None;
    for file in &evidence.program_files {
        ensure!(
            previous.is_none_or(|path| path < file.path.as_str()),
            "program membership not unique/sorted"
        );
        ensure!(
            evidence.inputs.contains_key(&file.path),
            "program source was not captured/consumed"
        );
        previous = Some(&file.path);
    }
    Ok(())
}

fn runtime_file<'a>(manifest: &'a RuntimeManifest, role: &RuntimeRole) -> Result<&'a Path> {
    let mut files = manifest.files.values().filter(|file| &file.role == role);
    let file = files.next().context("missing runtime role")?;
    ensure!(files.next().is_none(), "duplicate runtime role");
    Ok(&file.path)
}

fn verify_files(files: &[(PathBuf, &[u8])]) -> Result<()> {
    for (path, expected) in files {
        ensure!(
            fs::symlink_metadata(path)?.is_file()
                && fs::canonicalize(path)? == *path
                && read_bundle_file(path, expected.len())? == *expected,
            "helper bundle file changed: {}",
            path.display()
        );
    }
    Ok(())
}

fn read_bundle_file(path: &Path, limit: usize) -> Result<Vec<u8>> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    ensure!(fs::canonicalize(path)? == path, "noncanonical bundle path");
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path)?;
    ensure!(file.metadata()?.is_file(), "bundle input must be regular");
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit && fs::canonicalize(path)? == path,
        "bundle input changed or oversized"
    );
    Ok(bytes)
}

fn verify_bounds(output: &Path, errors: &Path) -> Result<()> {
    ensure!(
        fs::metadata(output)?.len() <= OUTPUT_LIMIT && fs::metadata(errors)?.len() <= ERROR_LIMIT,
        "helper output budget exceeded"
    );
    Ok(())
}

fn await_frame(
    child: &mut Child,
    output: &Path,
    errors: &Path,
    offset: usize,
    deadline: Instant,
    limit: u64,
) -> Result<Vec<u8>> {
    let mut observed = 0;
    loop {
        verify_bounds(output, errors)?;
        let size = fs::metadata(output)?.len();
        ensure!(
            size.saturating_sub(offset as u64) <= limit,
            "helper frame budget exceeded"
        );
        if size != observed {
            let bytes = fs::read(output)?;
            if let Some(rest) = bytes.get(offset..) {
                if let Some(end) = rest.iter().position(|byte| *byte == b'\n') {
                    ensure!(
                        end + offset + 1 == bytes.len(),
                        "unexpected helper frame/output"
                    );
                    return Ok(rest[..end].to_vec());
                }
            }
            observed = size;
        }
        if let Some(status) = child.try_wait()? {
            verify_bounds(output, errors)?;
            bail!(
                "helper exited before frame ({status}): {}",
                fs::read_to_string(errors)?
            );
        }
        ensure!(
            Instant::now() < deadline,
            "guarded helper deadline exceeded"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_selection_is_source_relative() {
        for invalid in [
            "",
            "/tsconfig.json",
            "../tsconfig.json",
            "a/../b",
            "a\\b",
            "./a",
            "a//b",
        ] {
            assert!(validate_project(invalid).is_err(), "{invalid}");
        }
        validate_project("packages/app/tsconfig.json").unwrap();
    }

    #[test]
    fn bounded_frames_reject_extra_output_and_deadlines() {
        let scratch = tempfile::tempdir().unwrap();
        let output = scratch.path().join("stdout");
        let errors = scratch.path().join("stderr");
        fs::write(&errors, []).unwrap();
        fs::write(&output, b"{}\nextra").unwrap();
        let mut child = OwnedChild(Command::new("/bin/sleep").arg("10").spawn().unwrap());
        assert!(
            await_frame(&mut child.0, &output, &errors, 0, Instant::now(), 4096)
                .unwrap_err()
                .to_string()
                .contains("unexpected helper")
        );
        fs::write(&output, []).unwrap();
        assert!(
            await_frame(&mut child.0, &output, &errors, 0, Instant::now(), 4096)
                .unwrap_err()
                .to_string()
                .contains("deadline")
        );
        fs::write(&errors, vec![b'x'; ERROR_LIMIT as usize + 1]).unwrap();
        assert!(verify_bounds(&output, &errors).is_err());
    }

    #[test]
    fn bundle_reader_refuses_oversize_and_linked_bytes() {
        let scratch = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(scratch.path()).unwrap();
        let file = root.join("helper");
        fs::write(&file, b"pinned").unwrap();
        assert!(read_bundle_file(&file, 5).is_err());
        verify_files(&[(file.clone(), b"pinned")]).unwrap();
        fs::write(&file, b"drift!").unwrap();
        assert!(verify_files(&[(file.clone(), b"pinned")]).is_err());
        #[cfg(unix)]
        {
            let link = root.join("alias");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert!(read_bundle_file(&link, 10).is_err());
            let fifo = root.join("fifo");
            assert!(
                Command::new("/usr/bin/mkfifo")
                    .arg(&fifo)
                    .status()
                    .unwrap()
                    .success()
            );
            assert!(read_bundle_file(&fifo, 10).is_err());
        }
    }

    #[test]
    #[ignore = "requires explicit prepared JS/TS inputs and pinned Darwin runtime/bundle"]
    fn prepared_project_runs_with_captured_inputs_and_image_proofs() {
        let variable = |name| PathBuf::from(std::env::var(name).unwrap());
        let config = HelperConfig {
            roots: InputRoots {
                source: variable("CODEMAP_JS_TS_SOURCE"),
                typescript: variable("CODEMAP_JS_TS_TYPESCRIPT"),
            },
            inputs: serde_json::from_slice(&fs::read(variable("CODEMAP_JS_TS_INPUTS")).unwrap())
                .unwrap(),
            runtime: serde_json::from_slice(&fs::read(variable("CODEMAP_JS_TS_RUNTIME")).unwrap())
                .unwrap(),
            bundle: variable("CODEMAP_JS_TS_PROBE_BUNDLE"),
            project: "tsconfig.json".to_owned(),
        };
        let first = resolve(&config).unwrap();
        assert!(!first.evidence.program_files.is_empty());
        assert_ne!(first.evidence.sites.len(), 0);
        let second = resolve(&config).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
    }
}
