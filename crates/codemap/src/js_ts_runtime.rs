//! Explicit runtime profiles for the captured JS/TS compiler helper.

use std::path::Path;

#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::runtime::{RuntimeManifest, RuntimeRole};

pub(crate) enum CapturedJsTsRuntime {
    Darwin {
        runtime: crate::runtime::CapturedRuntime,
        catalog: crate::runtime_images::DarwinImageCatalog,
    },
    #[cfg(target_os = "linux")]
    Linux {
        runtime: crate::linux_runtime::CapturedLinuxRuntime,
        kernel: crate::linux_kernel_images::LinuxKernelPins,
        fingerprint: String,
    },
}

impl CapturedJsTsRuntime {
    pub(crate) fn capture(manifest: &JsTsRuntimeManifest) -> Result<Self> {
        match manifest {
            JsTsRuntimeManifest::Darwin(manifest) => {
                let runtime = crate::runtime::CapturedRuntime::capture_node(manifest)?;
                let catalog = crate::runtime_images::DarwinImageCatalog::capture(&runtime)?;
                Ok(Self::Darwin { runtime, catalog })
            }
            #[cfg(target_os = "linux")]
            JsTsRuntimeManifest::Linux(profile) => {
                let runtime =
                    crate::linux_runtime::CapturedLinuxRuntime::capture(&profile.runtime)?;
                let fingerprint = crate::schema::sha256_hex(&serde_json::to_vec(&(
                    "linux-js-ts-helper/1",
                    profile.kernel.fingerprint(&runtime)?,
                    crate::js_ts_linux::FLAGS,
                    "--openssl-config=<runtime>/openssl_empty",
                    crate::js_ts_linux::DEADLINE.as_secs(),
                    crate::js_ts_semantic::OUTPUT_LIMIT,
                    crate::js_ts_semantic::ERROR_LIMIT,
                    4096u64,
                    2u64,
                ))?);
                Ok(Self::Linux {
                    runtime,
                    kernel: profile.kernel.clone(),
                    fingerprint,
                })
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

    pub(crate) fn verify_unchanged(&self) -> Result<()> {
        match self {
            Self::Darwin { runtime, .. } => runtime.verify_unchanged(),
            #[cfg(target_os = "linux")]
            Self::Linux { runtime, .. } => runtime.verify_unchanged(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsTsRuntimeManifest {
    Darwin(RuntimeManifest),
    #[cfg(target_os = "linux")]
    Linux(LinuxJsTsRuntimeManifest),
}

impl From<RuntimeManifest> for JsTsRuntimeManifest {
    fn from(manifest: RuntimeManifest) -> Self {
        Self::Darwin(manifest)
    }
}

#[cfg(target_os = "linux")]
pub(crate) const OPENSSL_PIN: &str = "openssl_empty";

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxJsTsRuntimeManifest {
    pub platform: LinuxJsTsPlatform,
    pub runtime: crate::linux_runtime::LinuxRuntimeManifest,
    pub kernel: crate::linux_kernel_images::LinuxKernelPins,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxJsTsPlatform {
    Linux,
}

impl JsTsRuntimeManifest {
    pub(crate) fn validate_host(&self) -> Result<()> {
        match self {
            Self::Darwin(_) => ensure!(
                cfg!(all(target_os = "macos", target_arch = "aarch64")),
                "JS/TS guarded semantics requires macOS arm64"
            ),
            #[cfg(target_os = "linux")]
            Self::Linux(profile) => {
                use crate::linux_runtime::{
                    AnonymousExecutePolicy, LinuxArchitecture, LinuxRuntimeRole,
                };
                let native = match std::env::consts::ARCH {
                    "x86_64" => LinuxArchitecture::X86_64,
                    "aarch64" => LinuxArchitecture::Aarch64,
                    _ => anyhow::bail!("unsupported Linux JS/TS architecture"),
                };
                ensure!(
                    profile.runtime.architecture == native && profile.kernel.architecture == native,
                    "Linux JS/TS profile architecture differs from host"
                );
                ensure!(
                    profile.runtime.anonymous_execute == AnonymousExecutePolicy::Deny,
                    "Linux JS/TS anonymous execution is unsupported"
                );
                let tools = profile
                    .runtime
                    .files
                    .values()
                    .filter(|pin| {
                        matches!(
                            pin.role,
                            LinuxRuntimeRole::Node
                                | LinuxRuntimeRole::Clangd
                                | LinuxRuntimeRole::Python
                        )
                    })
                    .collect::<Vec<_>>();
                ensure!(
                    tools.len() == 1 && tools[0].role == LinuxRuntimeRole::Node,
                    "Linux JS/TS runtime requires exactly one Node executable"
                );
                let openssl = profile
                    .runtime
                    .files
                    .get(OPENSSL_PIN)
                    .context("Linux JS/TS runtime requires an empty OpenSSL data pin")?;
                ensure!(
                    openssl.role == LinuxRuntimeRole::Data
                        && openssl.sha256 == crate::schema::sha256_hex(b""),
                    "Linux JS/TS OpenSSL configuration must be pinned empty data"
                );
                profile.kernel.validate()?;
            }
        }
        Ok(())
    }

    pub(crate) fn node_path(&self) -> Result<&Path> {
        match self {
            Self::Darwin(manifest) => {
                let nodes = manifest
                    .files
                    .values()
                    .filter(|pin| pin.role == RuntimeRole::Node)
                    .collect::<Vec<_>>();
                ensure!(
                    nodes.len() == 1,
                    "runtime requires exactly one Node executable"
                );
                Ok(&nodes[0].path)
            }
            #[cfg(target_os = "linux")]
            Self::Linux(profile) => {
                let nodes = profile
                    .runtime
                    .files
                    .values()
                    .filter(|pin| pin.role == crate::linux_runtime::LinuxRuntimeRole::Node)
                    .collect::<Vec<_>>();
                ensure!(
                    nodes.len() == 1,
                    "runtime requires exactly one Node executable"
                );
                Ok(&nodes[0].path)
            }
        }
    }

    pub(crate) fn paths(&self) -> Vec<&Path> {
        match self {
            Self::Darwin(manifest) => manifest
                .files
                .values()
                .map(|pin| pin.path.as_path())
                .collect(),
            #[cfg(target_os = "linux")]
            Self::Linux(profile) => profile
                .runtime
                .files
                .values()
                .map(|pin| pin.path.as_path())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_darwin_manifest_json_is_preserved() {
        let bytes = json!({"files": {}, "cache_family": {"directory": "/operator/cache", "prefix": "dyld_shared_cache_arm64e"}});
        let legacy: RuntimeManifest = serde_json::from_value(bytes.clone()).unwrap();
        let wrapped: JsTsRuntimeManifest = serde_json::from_value(bytes).unwrap();
        assert_eq!(
            serde_json::to_vec(&legacy).unwrap(),
            serde_json::to_vec(&wrapped).unwrap()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_linux_profile_refuses_unsupported_tool_and_configuration() {
        let architecture = if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "x86_64"
        };
        let profile = json!({
            "platform": "linux",
            "runtime": {"architecture": architecture, "anonymous_execute": "deny", "files": {
                "node": {"path": "/operator/node", "sha256": "0".repeat(64), "role": "node"},
                "openssl_empty": {"path": "/operator/openssl-empty.cnf", "sha256": crate::schema::sha256_hex(b""), "role": "data"}
            }},
            "kernel": {"architecture": architecture, "vdso_size": 4096, "vdso_sha256": "0".repeat(64),
                "vsyscall": if cfg!(target_arch = "aarch64") {"absent"} else {"fixed_emulated"}}
        });
        let parsed: JsTsRuntimeManifest = serde_json::from_value(profile.clone()).unwrap();
        parsed.validate_host().unwrap();
        assert_eq!(parsed.node_path().unwrap(), Path::new("/operator/node"));
        assert_eq!(parsed.paths().len(), 2);
        for (pointer, value) in [
            ("/runtime/anonymous_execute", json!("allow")),
            ("/runtime/files/node/role", json!("clangd")),
            ("/runtime/files/openssl_empty/role", json!("library")),
            ("/runtime/files/openssl_empty/sha256", json!("0".repeat(64))),
            (
                "/kernel/architecture",
                json!(if architecture == "aarch64" {
                    "x86_64"
                } else {
                    "aarch64"
                }),
            ),
        ] {
            let mut changed = profile.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            let rejected: JsTsRuntimeManifest = serde_json::from_value(changed).unwrap();
            assert!(rejected.validate_host().is_err(), "{pointer}");
        }
        let mut missing = profile.clone();
        missing["runtime"]["files"]
            .as_object_mut()
            .unwrap()
            .remove(OPENSSL_PIN);
        let rejected: JsTsRuntimeManifest = serde_json::from_value(missing).unwrap();
        assert!(rejected.validate_host().is_err());
        let mut extra = profile;
        extra["implicit_fallback"] = json!(true);
        assert!(serde_json::from_value::<JsTsRuntimeManifest>(extra).is_err());
    }
}
