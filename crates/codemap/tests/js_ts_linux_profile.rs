//! Independent selected-tool producers must not bypass helper identity checks.
#![cfg(target_os = "linux")]

use codemap::js_ts_runtime::JsTsRuntimeManifest;
use codemap::js_ts_semantic::resolve;
use codemap::linux_runtime::{LinuxRuntimeFile, LinuxRuntimeRole};
use codemap::schema::sha256_hex;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[path = "support/js_ts_prepared.rs"]
mod prepared_inputs;

#[test]
#[ignore = "requires native Linux protocol fixture and pinned compiler inputs"]
fn selected_tool_identity_mismatches_are_refused() {
    let producer = std::env::var_os("CODEMAP_JS_TS_LINUX_FIXTURE").unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let source = prepared_inputs::fixture();
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../toolchain/linux/js_ts_helper_protocol_expectations.json"
    ))
    .unwrap();
    let openssl = scratch.path().join("openssl-empty");
    fs::write(&openssl, b"").unwrap();
    for mode in oracle["semantic_reject"].as_array().unwrap() {
        let mode = mode.as_str().unwrap();
        let executable = scratch.path().join(mode);
        fs::copy(&producer, &executable).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o500)).unwrap();
        let mut config = prepared_inputs::prepared(source.path());
        let JsTsRuntimeManifest::Linux(profile) = &mut config.runtime else {
            panic!("explicit Linux profile required");
        };
        profile.runtime.files = BTreeMap::from([
            (
                "fixture".into(),
                LinuxRuntimeFile {
                    path: executable.clone(),
                    sha256: sha256_hex(&fs::read(executable).unwrap()),
                    role: LinuxRuntimeRole::Node,
                },
            ),
            (
                "openssl_empty".into(),
                LinuxRuntimeFile {
                    path: openssl.clone(),
                    sha256: sha256_hex(b""),
                    role: LinuxRuntimeRole::Data,
                },
            ),
        ]);
        let error = resolve(&config)
            .err()
            .expect("mismatched identity must fail");
        let expected = if mode == "ready-version" {
            "invalid helper ready frame"
        } else {
            "invalid helper result identity"
        };
        assert!(format!("{error:#}").contains(expected), "{mode}: {error:#}");
    }
}
