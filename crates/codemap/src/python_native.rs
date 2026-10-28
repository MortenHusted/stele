//! Owned native compiler queries; attestation does not authorize canonical graph edges.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::process::CommandExt as _;
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
use crate::python_profile::{BOOTSTRAP, NativeProfile, QUERY_HELPERS};
use crate::runtime::{CapturedRuntime, RuntimeFile, RuntimeManifest, RuntimeRole};
use crate::runtime_images::DarwinImageCatalog;
use crate::schema::sha256_hex;

const LIMIT: u64 = 32 * 1024 * 1024;
const ERROR_LIMIT: u64 = 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeConfig {
    pub roots: OperatorRoots,
    pub inputs: OperatorManifest,
    pub runtime: RuntimeManifest,
}

/// Successful owned execution only; source/body admission remains a separate step.
#[derive(Debug)]
pub struct NativeResult {
    data: Value,
    evidence: crate::python_evidence::Response,
    profile: NativeProfile,
    fingerprint: String,
}

impl NativeResult {
    pub(crate) const fn evidence(&self) -> &crate::python_evidence::Response {
        &self.evidence
    }
    pub const fn data(&self) -> &Value {
        &self.data
    }
    pub const fn profile(&self) -> &NativeProfile {
        &self.profile
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// A captured compiler/runtime can serve independent queries in fresh owned processes.
pub struct NativeSession {
    private: tempfile::TempDir,
    operator: CapturedOperator,
    copied_operator: CapturedOperator,
    original_runtime: CapturedRuntime,
    runtime: CapturedRuntime,
    catalog: DarwinImageCatalog,
    roots: OperatorRoots,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    version: [u8; 3],
    compiler: String,
    origins: BTreeMap<String, Option<String>>,
}

impl NativeSession {
    #[allow(clippy::literal_string_with_formatting_args)] // Captured logical root addresses are literal placeholders.
    pub fn capture(config: &NativeConfig, source_root: &Path) -> Result<Self> {
        let source_root = source_root.canonicalize()?;
        let source_root = source_root.as_path();
        ensure!(
            cfg!(all(target_os = "macos", target_arch = "aarch64")),
            "captured Python native queries require macOS arm64"
        );
        let compiler = config
            .roots
            .compiler
            .as_ref()
            .context("native Python queries require a captured compiler root")?;
        let operator = CapturedOperator::capture(&config.inputs, &config.roots, source_root)?;
        let mut original_manifest = config.runtime.clone();
        for module in ["base64", "internal"] {
            let relative = format!("dependencies/librt/{module}.cpython-314-darwin.so");
            let logical = format!("{{compiler}}/{relative}");
            let key = format!("python/compiler/{relative}");
            let expected = RuntimeFile {
                path: compiler.join(&relative),
                sha256: config
                    .inputs
                    .files
                    .get(&logical)
                    .context("captured compiler library missing")?
                    .clone(),
                role: RuntimeRole::Library,
            };
            if let Some(existing) = original_manifest.files.get(&key) {
                ensure!(
                    existing.path == expected.path
                        && existing.sha256 == expected.sha256
                        && existing.role == expected.role,
                    "native compiler runtime library differs from captured inputs"
                );
            } else {
                original_manifest.files.insert(key, expected);
            }
        }
        ensure!(
            original_manifest
                .files
                .values()
                .all(|file| !file.path.starts_with(source_root)),
            "native runtime must remain outside application source"
        );
        let original_runtime = CapturedRuntime::capture_python(&original_manifest)?;
        let private = tempfile::tempdir()?;
        let root = private.path().canonicalize()?;
        let roots = operator.materialize(&root.join("operator"))?;
        let copied_operator = CapturedOperator::capture(&config.inputs, &roots, source_root)?;
        let mut manifest = original_manifest;
        for file in manifest.files.values_mut() {
            if file.role == RuntimeRole::Python {
                ensure!(
                    file.sha256 == config.inputs.files["{runtime}/bin/python3.14"],
                    "native runtime executable differs from captured operator"
                );
                file.path = roots.runtime.join("bin/python3.14");
            } else if let Ok(relative) = file.path.strip_prefix(&config.roots.runtime) {
                file.path = roots.runtime.join(relative);
            } else if let Ok(relative) = file.path.strip_prefix(compiler) {
                file.path = roots.compiler.as_ref().unwrap().join(relative);
            }
        }
        let runtime = CapturedRuntime::capture_python(&manifest)?;
        ensure!(
            runtime.fingerprint() == original_runtime.fingerprint(),
            "copied native runtime identity changed"
        );
        let catalog = DarwinImageCatalog::capture(&runtime)?;
        Ok(Self {
            private,
            operator,
            copied_operator,
            original_runtime,
            runtime,
            catalog,
            roots,
        })
    }

    #[allow(clippy::too_many_lines)] // One owned lifecycle binds source bytes, compiler execution and every postcheck.
    pub fn query(
        &self,
        sources: &BTreeMap<String, Vec<u8>>,
        package_profile: &Value,
    ) -> Result<NativeResult> {
        let profile = NativeProfile::for_query(
            &self.operator,
            &self.original_runtime,
            package_profile,
            sources,
        )?;
        ensure!(
            profile.fingerprint()?
                == NativeProfile::for_query(
                    &self.copied_operator,
                    &self.runtime,
                    package_profile,
                    sources
                )?
                .fingerprint()?,
            "copied native query profile changed"
        );
        let entries: Vec<_> = sources.iter().map(|(path, bytes)| json!({"path":path,"source_base64":base64::engine::general_purpose::STANDARD.encode(bytes),"sha256":sha256_hex(bytes)})).collect();
        let mut query = vec![b'1'];
        query.extend(serde_json::to_vec(
            &json!({"files":entries,"package_profile":package_profile}),
        )?);
        query.push(b'\n');
        ensure!(
            query.len() as u64 <= LIMIT,
            "native Python request budget exceeded"
        );
        let private_root = self.private.path().canonicalize()?;
        let query_root = tempfile::tempdir_in(&private_root)?;
        let root = query_root.path().canonicalize()?;
        let state = root.join("state");
        let helpers = root.join("helpers");
        let outside = root.join("outside");
        for directory in [&state, &helpers, &outside] {
            fs::create_dir(directory)?;
        }
        let outside_file = outside.join("read-control");
        fs::write(&outside_file, b"unapproved")?;
        let helper_files: Vec<_> = QUERY_HELPERS
            .iter()
            .copied()
            .chain(std::iter::once(("bootstrap.py", BOOTSTRAP)))
            .map(|(name, bytes)| (helpers.join(name), bytes))
            .collect();
        for (path, bytes) in &helper_files {
            fs::write(path, bytes)?;
        }
        let pins: BTreeMap<_, _> = helper_files
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
        let configuration = serde_json::to_vec(
            &json!({"runtime":self.roots.runtime,"compiler":self.roots.compiler,"files":self.operator.manifest().files,"directories":self.operator.manifest().directories,"helpers":pins,"ready":ready,"done":done,"outside":outside_file}),
        )?;
        ensure!(
            configuration.len() as u64 <= LIMIT,
            "native Python configuration budget exceeded"
        );
        let configuration_path = helpers.join("configuration.json");
        fs::write(&configuration_path, &configuration)?;
        let mut policy = self.copied_operator.guard_policy(&state, &self.runtime)?;
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
        let mut process = OwnedPython::new(
            Command::new(self.role_file(&RuntimeRole::Sandbox)?)
                .process_group(0)
                .env_clear()
                .env("HOME", &state)
                .env("TMPDIR", &state)
                .current_dir(&state)
                .arg("-f")
                .arg(&policy_path)
                .arg(self.role_file(&RuntimeRole::Python)?)
                .args(["-I", "-S", "-B"])
                .arg(helpers.join("bootstrap.py"))
                .arg(&configuration_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(fs::File::create(&errors)?)
                .spawn()
                .context("launch owned Python compiler query")?,
        )?;
        let deadline = Instant::now() + DEADLINE;
        let report: Ready = serde_json::from_slice(&await_marker(
            &process,
            &ready,
            &errors,
            deadline,
            ERROR_LIMIT,
        )?)?;
        validate_ready(&report, self.operator.manifest(), &pins)?;
        let images = self.catalog.inspect(process.pid())?;
        write_input(&mut process, &query, &errors, deadline)?;
        let result_bytes = await_marker(&process, &done, &errors, deadline, LIMIT)?;
        let data: Value = serde_json::from_slice(&result_bytes)?;
        let evidence =
            crate::python_evidence::validate_response(&data, sources, self.operator.manifest())
                .context("invalid native Python evidence response")?;
        ensure!(
            images == self.catalog.inspect(process.pid())?,
            "native Python query images changed"
        );
        self.verify_unchanged()?;
        verify_helpers(
            &helper_files,
            &configuration_path,
            &configuration,
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
            "native Python query failed after result"
        );
        ensure!(
            read_bounded(&done, LIMIT)? == result_bytes,
            "native Python query result changed"
        );
        self.verify_unchanged()?;
        verify_helpers(
            &helper_files,
            &configuration_path,
            &configuration,
            &policy_path,
            policy.as_bytes(),
        )?;
        let fingerprint = sha256_hex(&serde_json::to_vec(
            &json!({"contract":"python-native-query/1","profile":profile,"policies":{"process":"owned-group/wnowait/180s/1","input":"nonblocking/32MiB/1","output":"regular-nofollow/json-depth-128/32MiB/1","stderr":"bounded/1MiB/1"}}),
        )?);
        Ok(NativeResult {
            data,
            evidence,
            profile,
            fingerprint,
        })
    }

    fn role_file(&self, role: &RuntimeRole) -> Result<&Path> {
        let files: Vec<_> = self
            .runtime
            .manifest()
            .files
            .values()
            .filter(|file| &file.role == role)
            .collect();
        ensure!(
            files.len() == 1,
            "native Python runtime must select exactly one {role:?}"
        );
        Ok(&files[0].path)
    }

    fn verify_unchanged(&self) -> Result<()> {
        self.operator.verify_unchanged()?;
        self.copied_operator.verify_unchanged()?;
        self.original_runtime.verify_unchanged()?;
        self.runtime.verify_unchanged()
    }
}

fn validate_ready(
    report: &Ready,
    inputs: &OperatorManifest,
    helpers: &BTreeMap<&str, String>,
) -> Result<()> {
    ensure!(
        report.version == [3, 14, 7]
            && report.compiler == "2.3.1"
            && ["mypyc.codegen.emitmodule", "librt.base64", "librt.internal"]
                .iter()
                .all(|name| report.origins.contains_key(*name)),
        "invalid native Python ready report"
    );
    for origin in report.origins.values().flatten() {
        ensure!(
            origin == "built-in"
                || origin == "frozen"
                || inputs.files.contains_key(origin)
                || origin
                    .strip_prefix("{helper}/")
                    .is_some_and(|name| helpers.contains_key(name)),
            "unmanifested native Python module origin"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_report_requires_pinned_versions_modules_and_origins() {
        let inputs = OperatorManifest {
            files: BTreeMap::from([("{compiler}/module.so".to_owned(), "0".repeat(64))]),
            directories: BTreeMap::new(),
        };
        let helpers = BTreeMap::from([("native_binding.py", "1".repeat(64))]);
        let mut report = Ready {
            version: [3, 14, 7],
            compiler: "2.3.1".to_owned(),
            origins: ["mypyc.codegen.emitmodule", "librt.base64", "librt.internal"]
                .into_iter()
                .map(|name| (name.to_owned(), Some("{compiler}/module.so".to_owned())))
                .collect(),
        };
        report.origins.insert(
            "native_binding".to_owned(),
            Some("{helper}/native_binding.py".to_owned()),
        );
        assert!(validate_ready(&report, &inputs, &helpers).is_ok());
        report.version[2] = 6;
        assert!(validate_ready(&report, &inputs, &helpers).is_err());
        report.version[2] = 7;
        report.compiler = "2.3.0".to_owned();
        assert!(validate_ready(&report, &inputs, &helpers).is_err());
        report.compiler = "2.3.1".to_owned();
        report.origins.insert(
            "outside".to_owned(),
            Some("/application/plugin.py".to_owned()),
        );
        assert!(validate_ready(&report, &inputs, &helpers).is_err());
        report.origins.remove("outside");
        report.origins.remove("librt.internal");
        assert!(validate_ready(&report, &inputs, &helpers).is_err());
    }
}
