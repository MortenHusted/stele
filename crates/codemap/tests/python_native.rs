//! Actual owned production transport against independent, unchanged native labels.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::collections::BTreeMap;

use codemap::python_native::NativeSession;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[path = "common/native.rs"]
mod native;

fn sources(case: &Value) -> BTreeMap<String, Vec<u8>> {
    case["files"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(path, raw)| (path.clone(), raw.as_str().unwrap().as_bytes().to_vec()))
        .collect()
}

fn package_profile(case: &Value) -> Value {
    case.get("package_profile").cloned().unwrap_or_else(
        || json!({"source_roots":["."],"namespace_packages":true,"external_stub_roots":[]}),
    )
}

#[test]
#[ignore = "requires the explicit captured compiler/runtime and native operator manifest"]
fn owned_native_transport_refuses_non_utf8_cookie_bytes_and_bom() {
    let root = tempfile::tempdir().unwrap();
    let session = NativeSession::capture(&native::config(), root.path()).unwrap();
    for raw in [
        b"# coding: latin-1\nvalue = '\xc3\xa9'\n".as_slice(),
        b"# coding: latin-1\nvalue = '\xe9'\n".as_slice(),
        b"\xef\xbb\xbfvalue = 1\n".as_slice(),
    ] {
        let error = session
            .query(
                &BTreeMap::from([("sample.py".to_owned(), raw.to_vec())]),
                &json!({"source_roots":["."],"namespace_packages":true,"external_stub_roots":[]}),
            )
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("UTF-8"),
            "native input encoding must refuse explicitly"
        );
    }
}

#[test]
#[ignore = "requires the explicit captured compiler/runtime and native operator manifest"]
fn owned_native_transport_matches_complete_frozen_oracle() {
    let oracle: Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_oracle.json"
    ))
    .unwrap();
    let observations: Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_observation_oracle.json"
    ))
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let session = NativeSession::capture(&native::config(), root.path()).unwrap();
    let locations: Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_evidence_oracle.json"
    ))
    .unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        let result = session
            .query(&sources(case), &package_profile(case))
            .unwrap();
        let data = result.data();
        assert_eq!(data["canonical_admission"], false);
        assert_eq!(data["calls"], case["expected_calls"], "{}", case["name"]);
        assert_eq!(
            data["evidence"].as_array().unwrap().len(),
            case["expected_calls"].as_array().unwrap().len()
        );
        let actual: Vec<_> = data["observations"].as_array().unwrap().iter().map(|item| json!({"path":item["path"],"span":item["call"]["span"],"callee_span":item["call"]["callee_span"],"status":item["status"],"reason":item["reason"]})).collect();
        assert_eq!(
            json!(actual),
            observations["cases"][case["name"].as_str().unwrap()]
        );
        assert_eq!(result.fingerprint().len(), 64);
        let candidates: Vec<_> = data["observations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["status"] == "eligible_candidate")
            .map(|item| json!({"path":item["path"],"call":item["call"]}))
            .collect();
        let evidence = data["evidence"].as_array().unwrap();
        let sites: Vec<_> = evidence
            .iter()
            .map(|item| json!({"path":item["path"],"call":item["call"]}))
            .collect();
        assert_eq!(candidates, sites);
        let coordinates = locations["locations"][case["name"].as_str().unwrap()].as_array();
        assert_eq!(evidence.len(), coordinates.map_or(0, Vec::len));
        for (item, location) in evidence.iter().zip(coordinates.into_iter().flatten()) {
            let coordinates = location.as_array().unwrap();
            let number =
                |index: usize| usize::try_from(coordinates[index].as_u64().unwrap()).unwrap();
            let raw = case["files"][item["path"].as_str().unwrap()]
                .as_str()
                .unwrap();
            assert_eq!(
                item["call"]["span"],
                fixture_byte_span(raw, number(0), number(1), number(0), number(2))
            );
            assert_eq!(item["caller"]["qualified_name"], coordinates[3]);
            assert_eq!(
                item["caller"]["span"],
                fixture_byte_span(raw, number(4), number(5), number(6), number(7))
            );
            assert_eq!(item["target_path"], coordinates[8]);
            let target = case["files"][coordinates[8].as_str().unwrap()]
                .as_str()
                .unwrap();
            assert_eq!(
                item["target"]["span"],
                fixture_byte_span(target, number(9), 0, number(10), number(11))
            );
        }
        for (path, raw) in sources(case) {
            let address = format!("{{source}}/{path}");
            let digest = format!("{:x}", Sha256::digest(raw));
            assert!(
                data["read_ledger"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry[0] == "read" && entry[1] == address && entry[2] == digest)
            );
        }
        eprintln!("Owned native oracle case completed: {}", case["name"]);
    }
}

fn fixture_byte_span(
    raw: &str,
    first: usize,
    column: usize,
    last: usize,
    end_column: usize,
) -> Value {
    let lines = raw
        .as_bytes()
        .split_inclusive(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    json!({"start_line":first,"end_line":last,
        "start_byte":lines.iter().take(first - 1).map(|line| line.len()).sum::<usize>() + column,
        "end_byte":lines.iter().take(last - 1).map(|line| line.len()).sum::<usize>() + end_column})
}

#[test]
#[ignore = "requires the explicit captured compiler/runtime and native operator manifest"]
fn owned_native_transport_repeats_profile_and_complete_evidence() {
    let oracle: Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_oracle.json"
    ))
    .unwrap();
    let case = &oracle["cases"][0];
    let root = tempfile::tempdir().unwrap();
    let session = NativeSession::capture(&native::config(), root.path()).unwrap();
    let first = session
        .query(&sources(case), &package_profile(case))
        .unwrap();
    for (files, expected) in [
        (
            BTreeMap::from([("../escape.py".to_owned(), b"pass".to_vec())]),
            "normalized repository",
        ),
        (
            BTreeMap::from([("large.py".to_owned(), vec![b' '; 8 * 1024 * 1024 + 1])]),
            "source file budget",
        ),
        (
            (0..5)
                .map(|index| (format!("large{index}.py"), vec![b' '; 7 * 1024 * 1024]))
                .collect(),
            "source query budget",
        ),
    ] {
        assert!(
            session
                .query(&files, &package_profile(case))
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
    assert!(session.query(&sources(case), &json!({"source_roots":["other"],"namespace_packages":true,"external_stub_roots":[]})).is_err());
    let second = session
        .query(&sources(case), &package_profile(case))
        .unwrap();
    assert_eq!(first.data()["calls"], case["expected_calls"]);
    assert_eq!(first.data(), second.data());
    assert_eq!(first.fingerprint(), second.fingerprint());
    assert_eq!(
        first.profile().fingerprint().unwrap(),
        second.profile().fingerprint().unwrap()
    );
}

#[test]
#[ignore = "requires the explicit captured compiler/runtime and native operator manifest"]
fn owned_native_transport_refuses_missing_compiler_and_changed_inputs() {
    let root = tempfile::tempdir().unwrap();
    let mut config = native::config();
    config.roots.compiler = None;
    assert!(
        NativeSession::capture(&config, root.path())
            .err()
            .unwrap()
            .to_string()
            .contains("compiler root")
    );
    let mut config = native::config();
    config
        .inputs
        .files
        .insert("{runtime}/bin/python3.14".to_owned(), "0".repeat(64));
    assert!(NativeSession::capture(&config, root.path()).is_err());
}

#[test]
#[ignore = "requires an explicit pinned local query and independent prior result"]
fn owned_native_transport_matches_pinned_workload() {
    use base64::Engine as _;
    let request_bytes = read_local("CODEMAP_PYTHON_NATIVE_QUERY");
    assert_eq!(
        format!("{:x}", Sha256::digest(&request_bytes)),
        std::env::var("CODEMAP_PYTHON_NATIVE_QUERY_SHA256").unwrap()
    );
    let request: Value = serde_json::from_slice(&request_bytes).unwrap();
    let mut files = BTreeMap::new();
    for entry in request["files"].as_array().unwrap() {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(entry["source_base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(&raw)), entry["sha256"]);
        assert!(
            files
                .insert(entry["path"].as_str().unwrap().to_owned(), raw)
                .is_none()
        );
    }
    let baseline_bytes = read_local("CODEMAP_PYTHON_NATIVE_BASELINE");
    assert_eq!(
        format!("{:x}", Sha256::digest(&baseline_bytes)),
        std::env::var("CODEMAP_PYTHON_NATIVE_BASELINE_SHA256").unwrap()
    );
    let baseline: Value = serde_json::from_slice(&baseline_bytes).unwrap();
    let root = tempfile::tempdir().unwrap();
    let session = NativeSession::capture(&native::config(), root.path()).unwrap();
    let result = session.query(&files, &package_profile(&request)).unwrap();
    let mut actual = result.data().clone();
    actual["execution_profile"] = serde_json::to_value(result.profile()).unwrap();
    actual["execution_profile_fingerprint"] = result.profile().fingerprint().unwrap().into();
    assert!(
        actual == baseline,
        "complete pinned workload evidence/profile changed"
    );
    assert!(
        read_local("CODEMAP_PYTHON_NATIVE_QUERY") == request_bytes,
        "pinned query input changed"
    );
    assert!(
        read_local("CODEMAP_PYTHON_NATIVE_BASELINE") == baseline_bytes,
        "pinned evidence input changed"
    );
}

fn read_local(variable: &str) -> Vec<u8> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(std::env::var_os(variable).unwrap())
        .unwrap();
    assert!(file.metadata().unwrap().is_file());
    let mut bytes = Vec::new();
    file.take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 32 * 1024 * 1024);
    bytes
}
