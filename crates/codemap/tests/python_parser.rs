//! Actual owned source-only parser controls; no canonical graph admission.

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::collections::BTreeMap;
use std::fs;

use codemap::python_parser::{parse_declared_sources, parse_sources};
use codemap::schema::sha256_hex;

#[path = "common/python.rs"]
mod python;
use python::config;

#[test]
#[ignore = "requires the explicit captured Darwin parser runtime and complete operator manifest"]
fn owned_parser_keeps_warning_formatting_outside_the_closed_query() {
    let source = tempfile::tempdir().unwrap();
    let root = source.path().canonicalize().unwrap();
    let raw = b"raise RuntimeError('application executed')\ndef target(value):\n    return value\ndef caller():\n    return target('\\q')\n";
    let sources = BTreeMap::from([("warning.py".to_owned(), raw.to_vec())]);
    let config = config();
    let parsed = parse_sources(&config, &root, &sources).unwrap();
    assert_eq!(
        parsed.identity["policies"]["parser"],
        "isolated-cpython/frozen-codecs/source-syntax-warnings-disabled/2"
    );
    let file = &parsed.files[0];
    assert_eq!(file.sha256, sha256_hex(raw));
    assert_eq!(file.definitions.len(), 2);
    let call = file
        .calls
        .iter()
        .find(|call| call.callee == "target")
        .unwrap();
    assert_eq!(
        &raw[call.span.start_byte..call.span.end_byte],
        b"target('\\q')"
    );
    assert_eq!(call.caller.as_deref(), Some("caller"));
    let repeated = parse_sources(&config, &root, &sources).unwrap();
    assert_eq!(parsed.fingerprint, repeated.fingerprint);
    assert_eq!(
        serde_json::to_value(&parsed.files).unwrap(),
        serde_json::to_value(&repeated.files).unwrap()
    );
    let invalid = BTreeMap::from([("invalid.py".to_owned(), b"def !!!\n".to_vec())]);
    let error = parse_sources(&config, &root, &invalid).err().unwrap();
    assert!(format!("{error:#}").contains("SyntaxError"));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
#[ignore = "requires the explicit captured Darwin parser runtime and complete operator manifest"]
fn owned_parser_preserves_every_frozen_native_source_witness() {
    let expected: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/original_body_census_fixture.json"
    ))
    .unwrap();
    let oracle: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_oracle.json"
    ))
    .unwrap();
    let mut sources = BTreeMap::new();
    let mut declarations = Vec::new();
    let mut witnesses = BTreeMap::new();
    for (index, case) in oracle["cases"].as_array().unwrap().iter().enumerate() {
        for file in expected["cases"][case["name"].as_str().unwrap()]
            .as_array()
            .unwrap()
        {
            let path = file["path"].as_str().unwrap();
            let prefixed = format!("case_{index:02}/{path}");
            sources.insert(
                prefixed.clone(),
                case["files"][path].as_str().unwrap().as_bytes().to_vec(),
            );
            if std::path::Path::new(path).extension() == Some(std::ffi::OsStr::new("pyi")) {
                declarations.push(prefixed.clone());
            }
            let mut witness = file.clone();
            witness["path"] = serde_json::json!(prefixed);
            witnesses.insert(prefixed, witness);
        }
    }
    declarations.sort();
    let root = tempfile::tempdir().unwrap();
    let parsed = parse_declared_sources(
        &config(),
        &root.path().canonicalize().unwrap(),
        &sources,
        &declarations,
    )
    .unwrap();
    assert_eq!(parsed.files.len(), witnesses.len());
    for file in parsed.files {
        assert_eq!(
            serde_json::to_value(&file).unwrap(),
            witnesses[&file.path],
            "every original call, body, scope and declaration witness stays frozen"
        );
    }
}

#[test]
#[ignore = "requires the explicit captured Darwin parser runtime and complete operator manifest"]
fn owned_parser_preserves_explicit_declaration_census() {
    let labels: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/declaration_projection_oracle.json"
    ))
    .unwrap();
    let oracle: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_oracle.json"
    ))
    .unwrap();
    let case = oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == labels["source_case"])
        .unwrap();
    let sources: BTreeMap<_, _> = case["files"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(path, raw)| (path.clone(), raw.as_str().unwrap().as_bytes().to_vec()))
        .collect();
    let declarations: Vec<String> = serde_json::from_value(labels["declarations"].clone()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let config = config();
    assert!(parse_sources(&config, &root, &sources).is_err());
    assert!(parse_declared_sources(&config, &root, &sources, &[]).is_err());
    let first = parse_declared_sources(&config, &root, &sources, &declarations).unwrap();
    let second = parse_declared_sources(&config, &root, &sources, &declarations).unwrap();
    assert_eq!(first.fingerprint, second.fingerprint);
    assert_eq!(
        serde_json::to_value(&first.files).unwrap(),
        serde_json::to_value(&second.files).unwrap()
    );
    assert_eq!(
        first.identity["policies"]["source"],
        "original-py-and-selected-pyi-bytes/8MiB-file/32MiB-request/1"
    );
    for file in &first.files {
        assert_eq!(file.sha256, sha256_hex(&sources[&file.path]));
        let declaration = declarations.contains(&file.path);
        assert!(
            file.definitions
                .iter()
                .all(|definition| definition.declaration_only == declaration)
        );
        if declaration {
            assert_eq!(
                serde_json::json!(
                    file.definitions
                        .iter()
                        .map(|definition| &definition.name)
                        .collect::<Vec<_>>()
                ),
                labels["expected_declaration_names"]
            );
            assert_eq!(
                file.calls.len(),
                labels["expected_call_sites"].as_array().unwrap().len()
            );
            for (call, expected) in file
                .calls
                .iter()
                .zip(labels["expected_call_sites"].as_array().unwrap())
            {
                assert_eq!(serde_json::to_value(&call.span).unwrap(), expected["span"]);
                assert_eq!(
                    serde_json::to_value(&call.callee_span).unwrap(),
                    expected["callee_span"]
                );
                assert_eq!(serde_json::json!(call.owner), expected["owner"]);
                assert_eq!(serde_json::json!(call.caller), expected["caller"]);
                assert_eq!(call.evaluation_context, expected["evaluation_context"]);
            }
        }
    }
}

#[test]
#[ignore = "requires the explicit captured Darwin parser runtime and complete operator manifest"]
fn owned_parser_preserves_original_encodings_and_source_only_behavior() {
    let config = config();
    let source = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(source.path()).unwrap();
    fs::write(
        root.join("sitecustomize.py"),
        b"raise RuntimeError('startup hook executed')\n",
    )
    .unwrap();
    fs::write(
        root.join("json.py"),
        b"raise RuntimeError('application import executed')\n",
    )
    .unwrap();
    let mut files = BTreeMap::new();
    for (label, raw) in [
        (
            "unicode",
            "# café\ndef outer():\n    return target('🙂')\n"
                .as_bytes()
                .to_vec(),
        ),
        (
            "latin",
            b"# coding: latin-1\ndef outer():\n    return target('\xe9')\n".to_vec(),
        ),
    ] {
        for (ending, newline) in [
            ("lf", b"\n".as_slice()),
            ("crlf", b"\r\n".as_slice()),
            ("cr", b"\r".as_slice()),
        ] {
            let mut bytes = Vec::new();
            for part in raw.split_inclusive(|byte| *byte == b'\n') {
                bytes.extend_from_slice(part.strip_suffix(b"\n").unwrap());
                bytes.extend_from_slice(newline);
            }
            files.insert(format!("{label}_{ending}.py"), bytes);
        }
    }
    files.insert(
        "bom.py".to_owned(),
        [
            b"\xef\xbb\xbf".as_slice(),
            "def outer():\n    return target('é')\n".as_bytes(),
        ]
        .concat(),
    );
    files.insert("modern.py".to_owned(), b"type Alias[T] = list[T]\ndef outer[T](value: T):\n    text = t'value: {value}'\n    try:\n        return target()\n    except ValueError, TypeError:\n        return None\n".to_vec());
    files.insert(
        "sideeffect.py".to_owned(),
        b"raise RuntimeError('application must never execute')\n".to_vec(),
    );
    let parsed = parse_sources(&config, &root, &files).unwrap();
    assert_eq!(parsed.files.len(), files.len());
    assert_eq!(
        parsed.identity["operator_fingerprint"],
        std::env::var("CODEMAP_PYTHON_OPERATOR_FINGERPRINT").unwrap()
    );
    for file in &parsed.files {
        let raw = &files[&file.path];
        assert_eq!(file.sha256, sha256_hex(raw));
        assert_eq!(file.calls.len(), 1);
        let call = &file.calls[0];
        let callee = &raw[call.callee_span.start_byte..call.callee_span.end_byte];
        if file.path == "sideeffect.py" {
            assert_eq!(callee, b"RuntimeError");
            assert!(call.caller.is_none());
            assert_eq!(call.evaluation_context, "module_body");
        } else {
            assert_eq!(callee, b"target");
            assert_eq!(call.callee, "target");
            assert_eq!(call.caller.as_deref(), Some("outer"));
            assert_eq!(call.evaluation_context, "body");
        }
    }
    let repeated = parse_sources(&config, &root, &files).unwrap();
    assert_eq!(parsed.identity, repeated.identity);
    assert_eq!(parsed.fingerprint, repeated.fingerprint);
    assert_eq!(
        serde_json::to_value(parsed.files).unwrap(),
        serde_json::to_value(repeated.files).unwrap()
    );
}

#[test]
#[ignore = "requires the explicit captured Darwin parser runtime and complete operator manifest"]
fn owned_parser_refuses_invalid_python() {
    let config = config();
    let source = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(source.path()).unwrap();
    let files = BTreeMap::from([("bad.py".to_owned(), b"def broken(:\n".to_vec())]);
    assert!(parse_sources(&config, &root, &files).is_err());
}

#[test]
#[ignore = "requires the explicit captured Darwin parser runtime and complete operator manifest"]
fn owned_parser_matches_independent_lexical_ownership() {
    let expected: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/python_parser/lexical_ownership.json"
    ))
    .unwrap();
    let raw = expected["source"].as_str().unwrap().as_bytes();
    let unicode = expected["source"]
        .as_str()
        .unwrap()
        .replace("method(self", "méthod(self")
        .replace("Outer(Base", "Outer(Básé");
    let files = BTreeMap::from([
        ("original.py".to_owned(), raw.to_vec()),
        (
            "inserted.py".to_owned(),
            [b"# unrelated comment\n\n".as_slice(), raw].concat(),
        ),
        (
            "latin.py".to_owned(),
            [b"# coding: latin-1\r\n# caf\xe9\r\n".as_slice(), raw].concat(),
        ),
        ("unicode.py".to_owned(), unicode.into_bytes()),
    ]);
    let source = tempfile::tempdir().unwrap();
    let parsed =
        parse_sources(&config(), &fs::canonicalize(source.path()).unwrap(), &files).unwrap();
    for file in parsed.files {
        let raw = &files[&file.path];
        let mut expected_definitions = expected["definitions"].clone();
        let method = if file.path == "unicode.py" {
            expected_definitions[0]["bases"][0] = "Básé".into();
            expected_definitions[1]["name"] = "méthod".into();
            "méthod"
        } else {
            "method"
        };
        let definitions: Vec<_> = file.definitions.iter().map(|definition| serde_json::json!({
            "name": definition.name, "kind": definition.kind, "owner": definition.owner,
            "ordinal": definition.ordinal, "ambiguous": definition.ambiguous,
            "decorators": definition.decorators.iter().map(|value| &value.spelling).collect::<Vec<_>>(),
            "bases": definition.bases.iter().map(|value| &value.spelling).collect::<Vec<_>>()
        })).collect();
        assert_eq!(serde_json::json!(definitions), expected_definitions);
        for definition in &file.definitions {
            for expression in definition.decorators.iter().chain(&definition.bases) {
                let bytes = &raw[expression.span.start_byte..expression.span.end_byte];
                assert_eq!(bytes, expression.spelling.as_bytes());
                assert_eq!(expression.sha256, sha256_hex(bytes));
            }
        }
        let calls: BTreeMap<_, _> = file
            .calls
            .iter()
            .map(|call| (call.callee.as_str(), call.owner))
            .collect();
        assert_eq!(serde_json::json!(calls), expected["call_owners"]);
        let imports: BTreeMap<_, _> = file
            .imports
            .iter()
            .map(|import| (import.spelling.as_str(), import.owner))
            .collect();
        assert_eq!(serde_json::json!(imports), expected["import_owners"]);
        for call in &file.calls {
            match call.callee.as_str() {
                "default" | "method_decorate" | "decorate" => {
                    assert!(call.caller.is_none());
                    assert_eq!(call.evaluation_context, "definition_setup");
                }
                "duplicate" => assert_eq!(
                    call.caller.as_deref(),
                    Some(format!("Outer.{method}.nest").as_str())
                ),
                "target" => assert_eq!(
                    call.caller.as_deref(),
                    Some(format!("Outer.{method}").as_str())
                ),
                name => panic!("unexpected call {name}"),
            }
        }
    }
}
