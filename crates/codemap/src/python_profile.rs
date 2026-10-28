//! Native query identity only; execution and canonical admission remain separate proofs.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::python_inputs::{CapturedOperator, GUARD_POLICY, MATERIALIZATION_POLICY};
use crate::runtime::{CapturedRuntime, RuntimeRole};
use crate::schema::sha256_hex;

pub const QUERY_HELPERS: &[(&str, &[u8])] = &[
    (
        "probe.py",
        include_bytes!("../../../toolchain/python/probe.py"),
    ),
    (
        "native_binding.py",
        include_bytes!("../../../toolchain/python/native_binding.py"),
    ),
    (
        "package_profile.py",
        include_bytes!("../../../toolchain/python/package_profile.py"),
    ),
    (
        "captured_fs.py",
        include_bytes!("../../../toolchain/python/captured_fs.py"),
    ),
    (
        "captured_fscache.py",
        include_bytes!("../../../toolchain/python/captured_fscache.py"),
    ),
    (
        "compiler_read_scope.py",
        include_bytes!("../../../toolchain/python/compiler_read_scope.py"),
    ),
];

pub const BOOTSTRAP: &[u8] = include_bytes!("../../../toolchain/python/native_bootstrap.py");

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageProfile {
    source_roots: Vec<String>,
    namespace_packages: bool,
    external_stub_roots: Vec<String>,
}

/// This descriptor does not attest loaded images, commit/tree identity, or successful execution.
/// The launcher must bind it to its query and complete all before/after checks.
#[derive(Debug, Serialize)]
pub struct NativeProfile {
    contract: &'static str,
    execution_profile: &'static str,
    extractor_version: &'static str,
    platform: &'static str,
    python_version: [u8; 3],
    compiler_version: &'static str,
    operator_fingerprint: String,
    runtime_fingerprint: String,
    helper_pins: BTreeMap<String, String>,
    policies: BTreeMap<&'static str, &'static str>,
    package_profile: PackageProfile,
    sources: BTreeMap<String, String>,
    native_unit: Vec<String>,
    declarations: Vec<String>,
}

impl NativeProfile {
    /// Inputs are immutable query bytes and already captured operator/runtime identities.
    /// Compiler package validation and the launcher's drift/image checks remain mandatory.
    pub fn for_query(
        operator: &CapturedOperator,
        runtime: &CapturedRuntime,
        package_profile: &Value,
        sources: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Self> {
        let executable = runtime
            .manifest()
            .files
            .values()
            .find(|file| file.role == RuntimeRole::Python);
        ensure!(
            executable.is_some_and(|file| {
                file.sha256 == operator.manifest().files["{runtime}/bin/python3.14"]
            }),
            "native profile requires its captured Python executable"
        );
        ensure!(
            operator.manifest().directories.contains_key("{compiler}"),
            "native profile requires a captured compiler root"
        );
        for module in ["base64", "internal"] {
            let logical = format!("{{compiler}}/dependencies/librt/{module}.cpython-314-darwin.so");
            let digest = sha256_hex(operator.read(&logical)?);
            ensure!(
                runtime
                    .manifest()
                    .files
                    .values()
                    .any(|file| { file.role == RuntimeRole::Library && file.sha256 == digest }),
                "native profile requires captured compiler libraries"
            );
        }
        Self::from_inputs(
            operator.fingerprint(),
            runtime.fingerprint(),
            package_profile,
            sources,
        )
    }

    fn from_inputs(
        operator_fingerprint: &str,
        runtime_fingerprint: &str,
        package_profile: &Value,
        sources: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Self> {
        let package_profile: PackageProfile = serde_json::from_value(package_profile.clone())?;
        ensure!(
            !package_profile.source_roots.is_empty()
                && package_profile.source_roots.len() <= 100
                && package_profile.external_stub_roots.is_empty(),
            "bounded source roots and supported stub policy required"
        );
        for (index, root) in package_profile.source_roots.iter().enumerate() {
            ensure!(
                root == "." || normalized_path(root),
                "normalized source root required"
            );
            for previous in &package_profile.source_roots[..index] {
                ensure!(
                    root != previous
                        && root != "."
                        && previous != "."
                        && !root.starts_with(&format!("{previous}/"))
                        && !previous.starts_with(&format!("{root}/")),
                    "overlapping source roots refused"
                );
            }
        }
        ensure!(sources.len() <= 100_000, "native source inventory budget");
        let mut bytes = 0usize;
        let mut pins = BTreeMap::new();
        for (path, source) in sources {
            ensure!(
                normalized_path(path)
                    && matches!(
                        Path::new(path).extension().and_then(OsStr::to_str),
                        Some("py" | "pyi")
                    ),
                "normalized repository Python source path required"
            );
            ensure!(
                package_profile
                    .source_roots
                    .iter()
                    .any(|root| { root == "." || path.starts_with(&format!("{root}/")) }),
                "source outside declared roots"
            );
            ensure!(source.len() <= 8 * 1024 * 1024, "native source file budget");
            bytes += source.len() + path.len();
            ensure!(bytes <= 32 * 1024 * 1024, "native source query budget");
            pins.insert(path.clone(), sha256_hex(source));
        }
        let helper_pins = QUERY_HELPERS
            .iter()
            .copied()
            .chain(std::iter::once(("bootstrap.py", BOOTSTRAP)))
            .map(|(name, bytes)| (name.to_owned(), sha256_hex(bytes)))
            .collect();
        Ok(Self {
            contract: "python-native-profile/1",
            execution_profile: "mypyc-native/single-unit/2.3.1",
            extractor_version: env!("CARGO_PKG_VERSION"),
            platform: "darwin-arm64",
            python_version: [3, 14, 7],
            compiler_version: "2.3.1",
            operator_fingerprint: operator_fingerprint.to_owned(),
            runtime_fingerprint: runtime_fingerprint.to_owned(),
            helper_pins,
            policies: BTreeMap::from([
                ("guard", GUARD_POLICY),
                ("materialization", MATERIALIZATION_POLICY),
                ("search", "ordered-declared-roots/typeshed-only/1"),
                ("cache", "disabled-devnull/1"),
                ("admission", "direct-native-ir/original-source/1"),
                ("compiler_io", "captured-compiler-reads/1"),
                ("unit", "all-supplied-py/single-compilation-unit/1"),
                (
                    "bootstrap",
                    "isolated-cpython/frozen-imports-before-source/1",
                ),
                ("images", "darwin-dyld-before-after/1"),
            ]),
            package_profile,
            native_unit: pins
                .keys()
                .filter(|path| Path::new(path).extension() == Some(OsStr::new("py")))
                .cloned()
                .collect(),
            declarations: pins
                .keys()
                .filter(|path| Path::new(path).extension() == Some(OsStr::new("pyi")))
                .cloned()
                .collect(),
            sources: pins,
        })
    }

    pub fn fingerprint(&self) -> Result<String> {
        Ok(sha256_hex(&serde_json::to_vec(&serde_json::to_value(
            self,
        )?)?))
    }

    pub(crate) const fn sources(&self) -> &BTreeMap<String, String> {
        &self.sources
    }
}

fn normalized_path(path: &str) -> bool {
    !path.contains(['\\', '\0']) && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> NativeProfile {
        NativeProfile::from_inputs(
            &sha256_hex(b"operator"),
            &sha256_hex(b"runtime"),
            &serde_json::json!({"source_roots":["src","lib"],"namespace_packages":true,"external_stub_roots":[]}),
            &BTreeMap::from([
                ("src/a.py".to_owned(), b"def a(): pass\n".to_vec()),
                ("lib/b.pyi".to_owned(), b"def b(): ...\n".to_vec()),
            ]),
        ).unwrap()
    }

    #[test]
    fn identity_has_no_host_paths_and_separates_native_bodies_from_declarations() {
        let profile = fixture();
        assert_eq!(profile.native_unit, ["src/a.py"]);
        assert_eq!(profile.declarations, ["lib/b.pyi"]);
        assert_eq!(
            profile.fingerprint().unwrap(),
            fixture().fingerprint().unwrap()
        );
        let json = serde_json::to_string(&profile).unwrap();
        assert!(!json.contains("/Users/") && !json.contains("/tmp/"));
    }

    #[test]
    fn captured_input_and_policy_changes_invalidate_identity() {
        let original = fixture().fingerprint().unwrap();
        for field in [
            "operator",
            "runtime",
            "helper",
            "roots",
            "namespace",
            "source",
            "inventory",
            "policy",
        ] {
            let mut profile = fixture();
            match field {
                "operator" => profile.operator_fingerprint = sha256_hex(b"changed"),
                "runtime" => profile.runtime_fingerprint = sha256_hex(b"changed"),
                "helper" => {
                    profile
                        .helper_pins
                        .insert("bootstrap.py".to_owned(), sha256_hex(b"changed"));
                }
                "roots" => profile.package_profile.source_roots.reverse(),
                "namespace" => profile.package_profile.namespace_packages = false,
                "source" => {
                    profile
                        .sources
                        .insert("src/a.py".to_owned(), sha256_hex(b"changed"));
                }
                "inventory" => {
                    profile
                        .sources
                        .insert("src/other.py".to_owned(), sha256_hex(b"added"));
                }
                "policy" => {
                    profile.policies.insert("cache", "changed");
                }
                _ => unreachable!(),
            }
            assert_ne!(original, profile.fingerprint().unwrap(), "{field}");
        }
    }

    #[test]
    fn malformed_source_paths_are_refused() {
        for path in ["/a.py", "../a.py", "a//b.py", "a\\b.py", "a.txt"] {
            assert!(NativeProfile::from_inputs("operator", "runtime", &serde_json::json!({"source_roots":["."],"namespace_packages":true,"external_stub_roots":[]}),
                &BTreeMap::from([(path.to_owned(), Vec::new())])).is_err());
        }
    }
}
