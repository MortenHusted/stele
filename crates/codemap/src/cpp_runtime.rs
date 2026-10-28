//! Explicit host profiles for guarded C++ resolution.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::cpp_inputs::CapturedInputs;
use crate::cpp_profile::ProfileRoots;
use crate::lsp::{self, Transport};
use crate::runtime::RuntimeManifest;
use crate::runtime::{CapturedRuntime, RuntimeRole, guard_policy};
use crate::runtime_images::{DarwinImageCatalog, LoadedImages};
use serde_json::{Value, json};
#[cfg(target_os = "linux")]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) enum CppRuntimeWitness {
    Darwin {
        runtime: CapturedRuntime,
        catalog: DarwinImageCatalog,
        loaded: LoadedImages,
    },
    #[cfg(target_os = "linux")]
    Linux {
        fingerprint: String,
        clangd_sha256: String,
        database: DatabaseWitness,
    },
}

impl CppRuntimeWitness {
    pub(crate) fn verify(&self, transport: &Transport, inputs: &CapturedInputs) -> Result<()> {
        match self {
            Self::Darwin {
                runtime,
                catalog,
                loaded,
            } => {
                ensure!(
                    *loaded == catalog.inspect(transport.process_id()?)?,
                    "C++ loaded runtime changed during resolution"
                );
                inputs.verify_unchanged()?;
                runtime.verify_unchanged()
            }
            #[cfg(target_os = "linux")]
            Self::Linux { database, .. } => {
                database.verify()?;
                transport.verify_linux_images()?;
                inputs.verify_unchanged()?;
                database.verify()
            }
        }
    }

    pub(crate) fn fingerprint(&self) -> &str {
        match self {
            Self::Darwin { runtime, .. } => runtime.fingerprint(),
            #[cfg(target_os = "linux")]
            Self::Linux { fingerprint, .. } => fingerprint,
        }
    }

    pub(crate) fn clangd_sha256(&self) -> Result<String> {
        match self {
            Self::Darwin { runtime, .. } => Ok(runtime
                .manifest()
                .files
                .values()
                .find(|pin| pin.role == RuntimeRole::Clangd)
                .ok_or_else(|| anyhow::anyhow!("pinned clangd missing"))?
                .sha256
                .clone()),
            #[cfg(target_os = "linux")]
            Self::Linux { clangd_sha256, .. } => Ok(clangd_sha256.clone()),
        }
    }
}

pub(crate) fn launch(
    manifest: &CppRuntimeManifest,
    roots: &ProfileRoots,
    state: &Path,
    private: &Path,
    inputs: &CapturedInputs,
) -> Result<(Transport, CppRuntimeWitness, Value)> {
    let arguments = [
        "--enable-config=false",
        "--background-index=false",
        "--clang-tidy=false",
        "--pch-storage=memory",
        "--skip-preamble-build",
        "--log=error",
    ];
    let database_argument = format!("--compile-commands-dir={}", private.display());
    match manifest {
        CppRuntimeManifest::Darwin(manifest) => {
            let runtime = CapturedRuntime::capture(manifest)?;
            let catalog = DarwinImageCatalog::capture(&runtime)?;
            let binary = runtime_path(&runtime, &RuntimeRole::Clangd)?;
            let sandbox = runtime_path(&runtime, &RuntimeRole::Sandbox)?;
            let policy = guard_policy(state, &binary)?;
            let mut command = Command::new(sandbox);
            command
                .env_clear()
                .env("HOME", state)
                .env("TMPDIR", state)
                .current_dir(state)
                .stderr(Stdio::null())
                .args(["-p", &policy])
                .arg(binary)
                .args(arguments)
                .arg(database_argument);
            inputs.verify_unchanged()?;
            let transport = Transport::spawn("clangd", &mut command, None)?;
            let capabilities = initialize(&transport, roots)?;
            let loaded = catalog.inspect(transport.process_id()?)?;
            Ok((
                transport,
                CppRuntimeWitness::Darwin {
                    runtime,
                    catalog,
                    loaded,
                },
                capabilities,
            ))
        }
        #[cfg(target_os = "linux")]
        CppRuntimeManifest::Linux(profile) => {
            use crate::linux_process::LinuxLaunch;
            use crate::linux_runtime::{CapturedLinuxRuntime, LinuxRuntimeRole};
            use crate::lsp::LinuxLspConfig;
            use std::collections::BTreeMap;
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(state, fs::Permissions::from_mode(0o700))?;
            let database = DatabaseWitness::capture(&private.join("compile_commands.json"))?;
            let runtime = CapturedLinuxRuntime::capture(&profile.runtime)?;
            let pin = runtime
                .manifest
                .files
                .values()
                .find(|pin| pin.role == LinuxRuntimeRole::Clangd)
                .ok_or_else(|| anyhow::anyhow!("Linux C++ profile requires clangd"))?;
            let executable = pin.path.clone();
            let clangd_sha256 = pin.sha256.clone();
            let mut reads = inputs.captured_read_paths()?;
            reads.push(database.path.clone());
            let launch = LinuxLaunch {
                executable,
                arguments: arguments
                    .into_iter()
                    .map(Into::into)
                    .chain(std::iter::once(database_argument.into()))
                    .collect(),
                environment: BTreeMap::from([
                    ("HOME".into(), state.as_os_str().to_owned()),
                    ("TMPDIR".into(), state.as_os_str().to_owned()),
                ]),
                directory: state.to_path_buf(),
            };
            let transport = Transport::spawn_linux(
                "clangd",
                LinuxLspConfig {
                    launch,
                    runtime,
                    kernel: profile.kernel.clone(),
                    reads,
                    directories: vec![],
                    limits: profile.limits,
                },
            )?;
            let fingerprint = transport.linux_policy_fingerprint()?;
            database.verify()?;
            let capabilities = initialize(&transport, roots)?;
            let witness = CppRuntimeWitness::Linux {
                fingerprint,
                clangd_sha256,
                database,
            };
            witness.verify(&transport, inputs)?;
            Ok((transport, witness, capabilities))
        }
    }
}

fn initialize(transport: &Transport, roots: &ProfileRoots) -> Result<Value> {
    let initialized = transport.request("initialize", &json!({"processId":null,"rootUri":lsp::uri_from_path(&roots.source),"capabilities":{"textDocument":{"definition":{"linkSupport":true},"inactiveRegionsCapabilities":{"inactiveRegions":true}}}}))?;
    transport.notify("initialized", &json!({}))?;
    Ok(initialized["capabilities"].clone())
}

pub(crate) fn runtime_path(runtime: &CapturedRuntime, role: &RuntimeRole) -> Result<PathBuf> {
    let files = runtime
        .manifest()
        .files
        .values()
        .filter(|file| file.role == *role)
        .collect::<Vec<_>>();
    ensure!(
        files.len() == 1,
        "C++ runtime requires one {role:?} executable"
    );
    Ok(files[0].path.clone())
}

#[cfg(target_os = "linux")]
pub(crate) struct DatabaseWitness {
    pub(crate) path: PathBuf,
    handle: fs::File,
    stamp: crate::linux_runtime::FileStamp,
    expected: Vec<u8>,
}

#[cfg(target_os = "linux")]
impl DatabaseWitness {
    fn capture(path: &Path) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o400))?;
        let handle = fs::File::open(path)?;
        let captured = Self {
            path: fs::canonicalize(path)?,
            stamp: crate::linux_runtime::FileStamp::of(&handle.metadata()?),
            handle,
            expected: fs::read(path)?,
        };
        captured.verify()?;
        Ok(captured)
    }

    fn verify(&self) -> Result<()> {
        use crate::linux_runtime::FileStamp;
        use std::os::unix::fs::FileExt;
        let check = || -> Result<()> {
            let metadata = fs::symlink_metadata(&self.path)?;
            ensure!(
                metadata.is_file()
                    && FileStamp::of(&metadata) == self.stamp
                    && FileStamp::of(&self.handle.metadata()?) == self.stamp,
                "private C++ compilation database changed"
            );
            Ok(())
        };
        check()?;
        let mut bytes = vec![0; self.expected.len()];
        self.handle.read_exact_at(&mut bytes, 0)?;
        ensure!(
            bytes == self.expected,
            "private C++ compilation database changed"
        );
        check()
    }
}

/// The Darwin variant retains the original untagged JSON representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CppRuntimeManifest {
    Darwin(RuntimeManifest),
    #[cfg(target_os = "linux")]
    Linux(LinuxCppRuntimeManifest),
}

impl From<RuntimeManifest> for CppRuntimeManifest {
    fn from(manifest: RuntimeManifest) -> Self {
        Self::Darwin(manifest)
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxCppRuntimeManifest {
    pub platform: LinuxCppPlatform,
    pub runtime: crate::linux_runtime::LinuxRuntimeManifest,
    pub kernel: crate::linux_kernel_images::LinuxKernelPins,
    pub limits: crate::lsp::LinuxLspLimits,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxCppPlatform {
    Linux,
}

impl CppRuntimeManifest {
    pub(crate) fn validate_host(&self) -> Result<()> {
        match self {
            Self::Darwin(_) => ensure!(
                cfg!(all(target_os = "macos", target_arch = "aarch64")),
                "C++ semantic launch requires a supported macOS arm64 runtime"
            ),
            #[cfg(target_os = "linux")]
            Self::Linux(profile) => {
                use crate::linux_runtime::{LinuxArchitecture, LinuxRuntimeRole};
                let native = match std::env::consts::ARCH {
                    "x86_64" => LinuxArchitecture::X86_64,
                    "aarch64" => LinuxArchitecture::Aarch64,
                    _ => anyhow::bail!("unsupported Linux C++ architecture"),
                };
                ensure!(
                    profile.runtime.architecture == native && profile.kernel.architecture == native,
                    "Linux C++ profile architecture differs from host"
                );
                ensure!(
                    profile
                        .runtime
                        .files
                        .values()
                        .filter(|pin| matches!(
                            pin.role,
                            LinuxRuntimeRole::Clangd
                                | LinuxRuntimeRole::Node
                                | LinuxRuntimeRole::Python
                        ))
                        .all(|pin| pin.role == LinuxRuntimeRole::Clangd),
                    "Linux C++ profile requires clangd"
                );
                ensure!(
                    profile.runtime.anonymous_execute
                        == crate::linux_runtime::AnonymousExecutePolicy::Deny,
                    "Linux C++ requires anonymous execution denial"
                );
                profile.kernel.validate()?;
                profile.limits.validate()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_darwin_profile_retains_its_json_bytes() {
        let original: RuntimeManifest = serde_json::from_value(json!({
            "files": {}, "cache_family": {"directory":"/operator/cache", "prefix":"cache"}
        }))
        .unwrap();
        let bytes = serde_json::to_vec(&original).unwrap();
        let profile: CppRuntimeManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_vec(&profile).unwrap(), bytes);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn frozen_linux_profile_descriptor_controls() {
        use std::collections::BTreeMap;
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../toolchain/linux/cpp_profile_expectations.json"
        ))
        .unwrap();
        let architecture = std::env::consts::ARCH;
        let profile = json!({
            "platform":"linux",
            "runtime":{"architecture":architecture,"anonymous_execute":"deny",
                "files":{"tool":{"path":"/operator/clangd","role":"clangd","sha256":"a".repeat(64)}}},
            "kernel":{"architecture":architecture,"vdso_size":4096,"vdso_sha256":"b".repeat(64),
                "vsyscall":if architecture == "x86_64" {"fixed_emulated"} else {"absent"}},
            "limits":crate::lsp::LinuxLspLimits::default(),
        });
        let mut observed = BTreeMap::new();
        for label in oracle["descriptor"].as_object().unwrap().keys() {
            let mut value = profile.clone();
            match label.as_str() {
                "legacy_darwin_roundtrip" => {
                    legacy_darwin_profile_retains_its_json_bytes();
                    observed.insert(label.clone(), "accept");
                    continue;
                }
                "explicit_linux_profile" => {}
                "linux_missing_platform" => {
                    value.as_object_mut().unwrap().remove("platform");
                }
                "linux_unknown_platform" => value["platform"] = json!("darwin"),
                "linux_missing_kernel" => {
                    value.as_object_mut().unwrap().remove("kernel");
                }
                "linux_unknown_fields" => value["ambient_reads"] = json!(true),
                "linux_mixed_darwin_fields" => {
                    value["cache_family"] = json!({"directory":"/cache", "prefix":"x"});
                }
                "linux_missing_limits" => {
                    value.as_object_mut().unwrap().remove("limits");
                }
                "linux_zero_limits" => value["limits"]["request_millis"] = json!(0),
                "linux_wrong_kernel_arch" => {
                    value["kernel"]["architecture"] = json!(if architecture == "x86_64" {
                        "aarch64"
                    } else {
                        "x86_64"
                    });
                }
                "linux_anonymous_execute_allow" => {
                    value["runtime"]["anonymous_execute"] = json!("allow");
                }
                "linux_unapproved_runtime_role" => {
                    value["runtime"]["files"]["tool"]["role"] = json!("node");
                }
                _ => panic!("unimplemented Linux C++ descriptor control {label}"),
            }
            let accepted = serde_json::from_value::<CppRuntimeManifest>(value)
                .and_then(|profile| profile.validate_host().map_err(serde::de::Error::custom))
                .is_ok();
            observed.insert(label.clone(), if accepted { "accept" } else { "reject" });
        }
        assert_eq!(
            serde_json::to_value(observed).unwrap(),
            oracle["descriptor"]
        );
    }
}
