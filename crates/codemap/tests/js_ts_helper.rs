//! Actual guarded compiler controls. Candidate evidence is not canonical admission.

use std::fs;

use codemap::js_ts_semantic::resolve;
use serde_json::json;

#[path = "support/js_ts_oracle.rs"]
mod oracle;

#[path = "support/js_ts_prepared.rs"]
mod prepared_inputs;
use prepared_inputs::{fixture, prepared};

#[test]
#[ignore = "requires explicit pinned Darwin runtime and JS/TS compiler bundle"]
fn guarded_helper_preserves_the_frozen_binding_oracle() {
    let scratch = fixture();
    let root = fs::canonicalize(scratch.path()).unwrap();
    let result = resolve(&prepared(&root)).unwrap();
    oracle::assert_oracle(&root, &json!({"sites": result.evidence.sites}));
    let second = fixture();
    let clone = fs::canonicalize(second.path()).unwrap();
    assert_eq!(
        result.fingerprint,
        resolve(&prepared(&clone)).unwrap().fingerprint
    );
}

#[test]
#[ignore = "requires explicit pinned Darwin runtime and JS/TS compiler bundle"]
fn guarded_helper_refuses_external_type_roots_and_plugins() {
    let scratch = fixture();
    let root = fs::canonicalize(scratch.path()).unwrap();
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("tsconfig.json")).unwrap()).unwrap();
    for outside in [
        "/external/ambient",
        "/node_modules/@types",
        "/__codemap_/ambient",
    ] {
        let mut config = original.clone();
        config["compilerOptions"]["typeRoots"] = json!([outside]);
        fs::write(
            root.join("tsconfig.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let error = resolve(&prepared(&root)).err().unwrap().to_string();
        assert!(
            error.contains("configured path outside captured roots"),
            "{error}"
        );
    }
    let mut config = original;
    config["compilerOptions"]["plugins"] = json!([{ "name": "./target-execution.js" }]);
    fs::write(
        root.join("tsconfig.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let error = resolve(&prepared(&root)).err().unwrap().to_string();
    assert!(error.contains("plugins are unsupported"), "{error}");
}

#[test]
#[ignore = "requires explicit pinned Darwin runtime and JS/TS compiler bundle"]
fn guarded_helper_refuses_unpinned_content_and_stale_dependencies() {
    let scratch = fixture();
    let root = fs::canonicalize(scratch.path()).unwrap();
    let mut config = prepared(&root);
    config.inputs.files.remove("{source}/leaf.js");
    let error = resolve(&config).err().unwrap().to_string();
    assert!(
        error.contains("compiler host failure") && error.contains("uncaptured content"),
        "{error}"
    );
    let config = prepared(&root);
    fs::write(
        root.join("declarations.d.ts"),
        "export declare function changed(): void;\n",
    )
    .unwrap();
    let error = resolve(&config).err().unwrap().to_string();
    assert!(error.contains("digest"), "{error}");
}

#[test]
#[ignore = "requires explicit pinned Darwin runtime and JS/TS compiler bundle"]
fn guarded_helper_refuses_invalid_programs() {
    let scratch = fixture();
    let root = fs::canonicalize(scratch.path()).unwrap();
    let original = fs::read(root.join("tsconfig.json")).unwrap();
    for (config, message) in [
        (
            json!({"extends": "./tsconfig.json", "include": ["./*"]}),
            "invalid or referenced project",
        ),
        (
            json!({"references": [{"path": "./another"}], "include": ["./*"]}),
            "invalid or referenced project",
        ),
        (
            json!({"extends": "./missing.json", "include": ["./*"]}),
            "invalid or referenced project",
        ),
    ] {
        fs::write(
            root.join("tsconfig.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let error = resolve(&prepared(&root)).err().unwrap().to_string();
        assert!(error.contains(message), "{error}");
    }
    fs::write(root.join("tsconfig.json"), original).unwrap();
    fs::write(
        root.join("missing-module.ts"),
        "import { unavailable } from './missing'; export function call() { unavailable(); }\n",
    )
    .unwrap();
    let error = resolve(&prepared(&root)).err().unwrap().to_string();
    assert!(error.contains("invalid semantic program"), "{error}");
}

#[test]
#[ignore = "requires explicit pinned Darwin runtime and JS/TS compiler bundle"]
fn guarded_helper_refuses_external_alias_targets_before_fallback() {
    let scratch = fixture();
    let root = fs::canonicalize(scratch.path()).unwrap();
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("tsconfig.json")).unwrap()).unwrap();
    for target in [
        "/node_modules/dep",
        "../../node_modules/dep",
        "/node_modules/*",
    ] {
        let mut config = original.clone();
        config["compilerOptions"]["paths"] = json!({"dep": [target, "./leaf.js"]});
        fs::write(
            root.join("tsconfig.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let error = resolve(&prepared(&root)).err().unwrap().to_string();
        assert!(
            error.contains("configured path outside captured roots"),
            "{error}"
        );
    }
}
