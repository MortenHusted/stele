//! Reusable source-pinned parser anchors for independent C++ benchmarking.

use crate::cpp_syntax::{ScopeIndex, extract_file};
use crate::lsp::byte_offset_to_lsp_pos;
use crate::schema::sha256_hex;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;

fn write_report(path: &Path, data: &Value) -> anyhow::Result<()> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, data)?;
    writer.flush()?;
    Ok(())
}

#[test]
fn report_creation_refuses_existing_files_and_input_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("source-manifest.json");
    fs::write(&input, b"original witness bytes").unwrap();
    let output = temp.path().join("report.json");
    let report = json!({"status": "synthetic report"});
    write_report(&output, &report).expect("new report");
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&output).unwrap()).unwrap(),
        report
    );
    for alias in [
        &input,
        &temp.path().join(".").join("source-manifest.json"),
        &output,
    ] {
        assert!(write_report(alias, &report).is_err());
    }
    #[cfg(unix)]
    {
        let symlink = temp.path().join("symlink.json");
        let hardlink = temp.path().join("hardlink.json");
        std::os::unix::fs::symlink(&input, &symlink).unwrap();
        fs::hard_link(&input, &hardlink).unwrap();
        for alias in [symlink, hardlink] {
            assert!(write_report(&alias, &report).is_err());
        }
    }
    assert_eq!(fs::read(input).unwrap(), b"original witness bytes");
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(output).unwrap()).unwrap(),
        report
    );
}

fn source_bytes(file: &Value) -> (String, Vec<u8>) {
    let text = file["text"].as_str().expect("source text");
    assert_eq!(
        sha256_hex(text.as_bytes()),
        file["sha256"].as_str().expect("source pin")
    );
    (
        file["path"].as_str().expect("relative path").to_string(),
        text.as_bytes().to_vec(),
    )
}

#[test]
#[ignore = "requires CODEMAP_CPP_SOURCE_MANIFEST and new CODEMAP_CPP_ANCHOR_REPORT"]
fn export_source_pinned_body_and_call_anchors() {
    let input = std::env::var("CODEMAP_CPP_SOURCE_MANIFEST").expect("source manifest");
    let output = std::env::var("CODEMAP_CPP_ANCHOR_REPORT").expect("new local anchor report");
    let data: Value =
        serde_json::from_slice(&fs::read(input).expect("manifest bytes")).expect("JSON");
    let files = data["files"].as_array().expect("manifested source files");
    let source = files.iter().map(source_bytes).collect::<Vec<_>>();
    let index = ScopeIndex::build(&source).expect("source scope index");
    let mut results = Vec::new();
    for (file, (path, bytes)) in files.iter().zip(&source) {
        let raw = extract_file(path, bytes, &index).expect("source anchors");
        let position = |span: &crate::schema::Span| {
            let (line, character) = byte_offset_to_lsp_pos(bytes, span.start_byte);
            json!({"line": line, "character": character})
        };
        results.push(json!({
            "path": path, "sha256": file["sha256"],
            "parse_errors": raw.parse_errors,
            "bodies": raw.bodies.iter().map(|body| json!({
                "span": body.span, "name_span": body.name_span, "position": position(&body.name_span),
                "qualifiers": body.qualifiers.iter().map(|span| json!({"span": span, "position": position(span)})).collect::<Vec<_>>(),
                "normalized_name": body.normalized_name, "lexical_scope": body.lexical_scope,
                "global": body.global, "templated": body.templated, "conditional": body.conditional,
                "parent": body.parent,
            })).collect::<Vec<_>>(),
            "calls": raw.call_anchors.iter().map(|call| json!({
                "span": call.span, "function_span": call.function_span, "name_span": call.name_span,
                "position": position(&call.name_span), "function_kind": call.function_kind,
                "name_kind": call.name_kind, "templated": call.templated, "inside_lambda": call.inside_lambda,
                "caller_span": call.caller_span, "unevaluated": call.unevaluated,
            })).collect::<Vec<_>>(),
        }));
    }
    write_report(Path::new(&output), &json!({
        "status": "Source-pinned parser inventory; no compiled presence, caller, target or acceptance claims",
        "files": results,
    })).expect("new local anchor report");
}

#[test]
fn call_anchors_distinguish_direct_shapes_and_unsupported_contexts() {
    let source = b"void f() { plain(); ns::qualified(); object.member(); pointer(); (*pointer)(); generic<int>(); auto l = [] { nested(); }; }\ntemplate<class T> void t() { dependent(); }\n";
    let raw = extract_file("unit.cpp", source, &ScopeIndex::default()).unwrap();
    assert_eq!(raw.call_anchors.len(), raw.calls.len());
    let shapes = raw
        .call_anchors
        .iter()
        .map(|call| {
            (
                &source[call.function_span.start_byte..call.function_span.end_byte],
                call.function_kind.as_str(),
                call.name_kind.as_str(),
                call.inside_lambda,
                call.templated,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        shapes,
        vec![
            (
                b"plain".as_slice(),
                "identifier",
                "identifier",
                false,
                false
            ),
            (
                b"ns::qualified".as_slice(),
                "qualified_identifier",
                "identifier",
                false,
                false
            ),
            (
                b"object.member".as_slice(),
                "field_expression",
                "field_identifier",
                false,
                false
            ),
            (
                b"pointer".as_slice(),
                "identifier",
                "identifier",
                false,
                false
            ),
            (
                b"(*pointer)".as_slice(),
                "parenthesized_expression",
                "parenthesized_expression",
                false,
                false
            ),
            (
                b"generic<int>".as_slice(),
                "template_function",
                "identifier",
                false,
                true
            ),
            (
                b"nested".as_slice(),
                "identifier",
                "identifier",
                true,
                false
            ),
            (
                b"dependent".as_slice(),
                "identifier",
                "identifier",
                false,
                true
            ),
        ]
    );
}

#[test]
fn template_call_anchors_query_the_name_and_keep_invocations_unsupported() {
    let source = b"void f() { generic<int>(); ns::generic<int>(); object.generic<int>(); Box<int>::run(); }\n";
    let raw = extract_file("unit.cpp", source, &ScopeIndex::default()).unwrap();
    assert_eq!(raw.call_anchors.len(), 4);
    for (anchor, expected) in
        raw.call_anchors
            .iter()
            .zip([b"generic".as_slice(), b"generic", b"generic", b"run"])
    {
        assert_eq!(
            &source[anchor.name_span.start_byte..anchor.name_span.end_byte],
            expected
        );
        assert!(anchor.templated);
        let position = byte_offset_to_lsp_pos(source, anchor.name_span.start_byte);
        assert_eq!(
            position,
            (0, u32::try_from(anchor.name_span.start_byte).unwrap())
        );
    }
}
