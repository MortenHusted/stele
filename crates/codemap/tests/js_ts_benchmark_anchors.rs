//! Source-only parser witnesses for an independently labelled benchmark.
use codemap::schema::sha256_hex;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn write_report(path: &Path, report: &Value) -> anyhow::Result<()> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    serde_json::to_writer_pretty(file, report)?;
    Ok(())
}

fn anchors(manifest: &Value) -> anyhow::Result<Value> {
    let mut paths = BTreeSet::new();
    let mut files = Vec::new();
    for file in manifest["files"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("source files required"))?
    {
        let path = file["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("source path required"))?;
        let text = file["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("source text required"))?;
        anyhow::ensure!(paths.insert(path), "duplicate source path");
        anyhow::ensure!(
            sha256_hex(text.as_bytes()) == file["sha256"],
            "source digest mismatch"
        );
        let raw = codemap::js_ts_syntax::extract_file(path, text.as_bytes())?;
        let bodies: Vec<_> = raw
            .bodies
            .iter()
            .map(|body| {
                let definition = &raw.defs[body.definition];
                json!({"span": definition.span, "callable_span": body.callable,
                "body_span": body.body, "name_span": body.name, "normalized_name": definition.name,
                "scope": definition.scope, "kind": definition.kind})
            })
            .collect();
        let calls: Vec<_> = raw.call_anchors.iter().map(|call| json!({
            "span": call.span, "callee_span": call.callee, "name_span": call.callee_name,
            "caller_body": call.caller_body, "constructor": call.constructor, "tagged": call.tagged,
        })).collect();
        files.push(
            json!({"path": path, "sha256": file["sha256"], "parse_errors": raw.parse_errors,
            "bodies": bodies, "calls": calls, "jsx_names": raw.jsx_names.iter().map(|span| json!({"span": span, "name": &text[span.start_byte..span.end_byte]})).collect::<Vec<_>>() }),
        );
    }
    files.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    Ok(
        json!({"contract": "codemap-js-ts-source-anchors/v1", "commit": manifest["commit"],
        "status": "Source-pinned parser witnesses only; no semantic membership, target or acceptance claims", "files": files}),
    )
}

#[test]
fn source_witnesses_distinguish_names_callers_and_signature_only_material() {
    let text = "export declare function external(): void;\nexport function target() {}\nexport function caller() { target(); object.method(); target`tag`; } // comment\n";
    let report = anchors(
        &json!({"commit": "synthetic", "files": [{"path": "source.ts",
        "text": text, "sha256": sha256_hex(text.as_bytes())}]}),
    )
    .unwrap();
    let file = &report["files"][0];
    assert_eq!(file["parse_errors"], 0);
    assert_eq!(file["bodies"].as_array().unwrap().len(), 2);
    let calls = file["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 3);
    let names: Vec<_> = calls
        .iter()
        .map(|call| {
            &text[call["name_span"]["start_byte"].as_u64().unwrap() as usize
                ..call["name_span"]["end_byte"].as_u64().unwrap() as usize]
        })
        .collect();
    assert_eq!(names, ["target", "method", "target"]);
    assert!(
        calls
            .iter()
            .all(|call| call["caller_body"] == file["bodies"][1]["body_span"])
    );
    assert_eq!(calls[2]["tagged"], true);
    let jsx = "export function View() { return <Footer>{target() && <DebugFooter />}</Footer>; }";
    let raw = codemap::js_ts_syntax::extract_file("view.tsx", jsx.as_bytes()).unwrap();
    assert_eq!(raw.jsx_names.len(), 2);
    assert!(raw.jsx_names.iter().all(|name| raw.call_anchors.iter().all(
        |call| call.span.end_byte <= name.start_byte || name.end_byte <= call.span.start_byte
    )));
}

#[test]
fn source_pin_duplicate_path_and_output_aliases_fail_closed() {
    let file = json!({"path": "source.ts", "text": "function f() {}", "sha256": "wrong"});
    assert!(anchors(&json!({"files": [file]})).is_err());
    let file = json!({"path": "source.ts", "text": "function f() {}", "sha256": sha256_hex(b"function f() {}")});
    assert!(anchors(&json!({"files": [file,file]})).is_err());
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("anchors.json");
    write_report(&output, &json!({"test": true})).unwrap();
    assert!(write_report(&output, &json!({"test": false})).is_err());
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(output).unwrap()).unwrap(),
        json!({"test": true})
    );
}

#[test]
#[ignore = "requires pinned source manifest and a new local anchor output path"]
fn export_source_pinned_call_and_body_anchors() {
    let input = std::env::var_os("CODEMAP_JS_TS_SOURCE_MANIFEST").unwrap();
    let output = std::env::var_os("CODEMAP_JS_TS_ANCHOR_REPORT").unwrap();
    let manifest: Value = serde_json::from_slice(&fs::read(input).unwrap()).unwrap();
    write_report(Path::new(&output), &anchors(&manifest).unwrap()).unwrap();
}
