//! Strict native witnesses; successful transport is not canonical body admission.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::python_inputs::{EntryKind, OperatorManifest};
use crate::python_parser::{ByteSpan, line_starts, validate_span};
use crate::schema::sha256_hex;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    canonical_admission: bool,
    calls: Vec<(String, u32, String, String, String)>,
    pub evidence: Vec<Evidence>,
    pub observations: Vec<Observation>,
    read_ledger: Vec<(String, String, String)>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub path: String,
    pub call: NativeCall,
    pub caller: NativeDefinition,
    pub target_path: String,
    pub target: NativeDefinition,
    #[serde(deserialize_with = "string_enum")]
    mechanism: Mechanism,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeDefinition {
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub span: ByteSpan,
    pub declaration_only: bool,
    pub decorators: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeCall {
    pub span: ByteSpan,
    pub callee_span: ByteSpan,
    pub callee: String,
    pub callee_kind: String,
    #[serde(deserialize_with = "nullable_caller")]
    pub caller: Option<String>,
    pub evaluation_context: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub path: String,
    pub call: NativeCall,
    #[serde(deserialize_with = "string_enum")]
    status: Status,
    #[serde(deserialize_with = "string_enum")]
    reason: Reason,
}

#[derive(Debug, Deserialize)]
enum Mechanism {
    #[serde(rename = "direct_native_ir_call")]
    DirectNativeIrCall,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Status {
    EligibleCandidate,
    Withheld,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Reason {
    DirectNativeIrCall,
    DeclarationOnlySource,
    UnsupportedEvaluationContext,
    NoUniqueSupportedNativeBody,
}

fn string_enum<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let value = String::deserialize(deserializer)?;
    T::deserialize(serde::de::value::StringDeserializer::<D::Error>::new(value))
}

fn nullable_caller<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
}

struct Source<'a> {
    text: &'a str,
    lines: Vec<usize>,
}

fn prepare_sources(sources: &BTreeMap<String, Vec<u8>>) -> Result<BTreeMap<&str, Source<'_>>> {
    sources
        .iter()
        .map(|(path, raw)| {
            Ok((
                path.as_str(),
                Source {
                    text: std::str::from_utf8(raw)?,
                    lines: line_starts(raw),
                },
            ))
        })
        .collect()
}

pub fn validate_response(
    data: &Value,
    sources: &BTreeMap<String, Vec<u8>>,
    operator: &OperatorManifest,
) -> Result<Response> {
    let response: Response = serde_json::from_value(data.clone())?;
    let prepared = prepare_sources(sources)?;
    ensure!(
        !response.canonical_admission,
        "native response claims admission"
    );
    let mut observations = BTreeMap::new();
    let mut previous = None;
    for observation in &response.observations {
        let raw = source(&prepared, &observation.path)?;
        validate_call(&observation.call, raw)?;
        let key = site(&observation.path, &observation.call);
        ensure!(
            previous.is_none_or(|prior| prior < key),
            "native observations not unique and ordered"
        );
        previous = Some(key);
        let reason = if observation.status == Status::EligibleCandidate {
            ensure!(
                observation.call.evaluation_context == "body"
                    && observation.call.caller.is_some()
                    && Path::new(&observation.path).extension() != Some(OsStr::new("pyi")),
                "unsupported native candidate context"
            );
            Reason::DirectNativeIrCall
        } else if Path::new(&observation.path).extension() == Some(OsStr::new("pyi")) {
            Reason::DeclarationOnlySource
        } else if observation.call.evaluation_context != "body" || observation.call.caller.is_none()
        {
            Reason::UnsupportedEvaluationContext
        } else {
            Reason::NoUniqueSupportedNativeBody
        };
        ensure!(
            observation.reason == reason,
            "native observation status/reason disagreement"
        );
        observations.insert(key, observation);
    }
    let mut evidence_sites = BTreeSet::new();
    let mut expected_calls = Vec::new();
    let mut previous = None;
    for evidence in &response.evidence {
        let raw = source(&prepared, &evidence.path)?;
        let target_raw = source(&prepared, &evidence.target_path)?;
        validate_call(&evidence.call, raw)?;
        validate_definition(&evidence.caller, raw)?;
        validate_definition(&evidence.target, target_raw)?;
        ensure!(
            matches!(evidence.mechanism, Mechanism::DirectNativeIrCall)
                && Path::new(&evidence.path).extension() != Some(OsStr::new("pyi"))
                && Path::new(&evidence.target_path).extension() != Some(OsStr::new("pyi"))
                && !evidence.caller.declaration_only
                && !evidence.target.declaration_only
                && matches!(
                    evidence.caller.kind.as_str(),
                    "FunctionDef" | "AsyncFunctionDef"
                )
                && evidence.target.kind == "FunctionDef"
                && evidence.target.decorators.is_empty()
                && evidence.call.evaluation_context == "body"
                && evidence.call.caller.as_deref() == Some(evidence.caller.qualified_name.as_str())
                && evidence.caller.span.start_byte <= evidence.call.span.start_byte
                && evidence.call.span.end_byte <= evidence.caller.span.end_byte,
            "unsupported native body evidence"
        );
        let key = site(&evidence.path, &evidence.call);
        ensure!(
            previous.is_none_or(|prior| prior < key) && evidence_sites.insert(key),
            "native evidence not unique and ordered"
        );
        previous = Some(key);
        ensure!(
            observations.get(&key).is_some_and(|observation| {
                observation.status == Status::EligibleCandidate && observation.call == evidence.call
            }),
            "native evidence lacks matching candidate"
        );
        expected_calls.push((
            evidence.path.clone(),
            evidence.call.span.start_line,
            evidence.call.callee.clone(),
            evidence.target_path.clone(),
            evidence.target.qualified_name.clone(),
        ));
    }
    ensure!(
        observations
            .iter()
            .filter(|(_, observation)| observation.status == Status::EligibleCandidate)
            .all(|(key, _)| evidence_sites.contains(key)),
        "native candidate lacks evidence"
    );
    expected_calls.sort();
    ensure!(
        response.calls == expected_calls,
        "native calls/evidence disagreement"
    );
    validate_ledger(&response.read_ledger, sources, operator)?;
    Ok(response)
}

const fn site<'a>(path: &'a str, call: &NativeCall) -> (&'a str, usize, usize) {
    (path, call.span.start_byte, call.span.end_byte)
}

fn source<'a>(sources: &'a BTreeMap<&str, Source<'a>>, path: &str) -> Result<&'a Source<'a>> {
    sources.get(path).context("native evidence source missing")
}

fn validate_call(call: &NativeCall, raw: &Source<'_>) -> Result<()> {
    validate_span(&call.span, raw.text.as_bytes(), &raw.lines)?;
    validate_span(&call.callee_span, raw.text.as_bytes(), &raw.lines)?;
    let text = raw.text;
    ensure!(
        text.is_char_boundary(call.span.start_byte)
            && text.is_char_boundary(call.span.end_byte)
            && call.span.start_byte <= call.callee_span.start_byte
            && call.callee_span.end_byte <= call.span.end_byte
            && text.get(call.callee_span.start_byte..call.callee_span.end_byte)
                == Some(call.callee.as_str())
            && !call.callee.is_empty()
            && !call.callee_kind.is_empty()
            && matches!(
                call.evaluation_context.as_str(),
                "body"
                    | "module_body"
                    | "class_body"
                    | "definition_setup"
                    | "annotation"
                    | "type_parameter"
                    | "type_alias"
                    | "lambda"
                    | "comprehension"
            ),
        "native call original-byte witness mismatch"
    );
    Ok(())
}

fn validate_definition(definition: &NativeDefinition, raw: &Source<'_>) -> Result<()> {
    validate_span(&definition.span, raw.text.as_bytes(), &raw.lines)?;
    let text = raw.text;
    ensure!(
        !definition.name.is_empty()
            && !definition.qualified_name.is_empty()
            && matches!(
                definition.kind.as_str(),
                "FunctionDef" | "AsyncFunctionDef" | "ClassDef"
            )
            && text.is_char_boundary(definition.span.start_byte)
            && text.is_char_boundary(definition.span.end_byte),
        "invalid native definition witness"
    );
    Ok(())
}

fn validate_ledger(
    ledger: &[(String, String, String)],
    sources: &BTreeMap<String, Vec<u8>>,
    operator: &OperatorManifest,
) -> Result<()> {
    let mut files = operator.files.clone();
    let mut directories = operator.directories.clone();
    directories.insert("{source}".to_owned(), BTreeMap::new());
    for (path, raw) in sources {
        let address = format!("{{source}}/{path}");
        files.insert(address.clone(), sha256_hex(raw));
        let (mut parent, name) = address
            .rsplit_once('/')
            .context("invalid native source address")?;
        directories
            .entry(parent.to_owned())
            .or_default()
            .insert(name.to_owned(), EntryKind::File);
        while parent != "{source}" {
            let (ancestor, name) = parent
                .rsplit_once('/')
                .context("invalid native source directory")?;
            directories
                .entry(ancestor.to_owned())
                .or_default()
                .insert(name.to_owned(), EntryKind::Directory);
            parent = ancestor;
        }
    }
    let mut previous = None;
    let mut reads = BTreeSet::new();
    ensure!(ledger.len() <= 100_000, "native read ledger budget");
    for entry in ledger {
        ensure!(
            previous.is_none_or(|prior| prior < entry),
            "native ledger not unique and ordered"
        );
        previous = Some(entry);
        let (operation, address, result) = entry;
        if operation == "cache" && address == "{disabled-cache}" && result == "disabled" {
            continue;
        }
        let mut components = address.split('/');
        ensure!(
            matches!(
                components.next(),
                Some("{source}" | "{compiler}" | "{runtime}")
            ) && components.all(|part| !matches!(part, "" | "." | ".."))
                && !address.contains(['\\', '\0']),
            "invalid native ledger address"
        );
        let file = files.get(address);
        let directory = directories.get(address);
        let expected = match operation.as_str() {
            "read" => file.cloned().unwrap_or_else(|| {
                if directory.is_some() {
                    "directory"
                } else {
                    "absent"
                }
                .to_owned()
            }),
            "stat" => if file.is_some() {
                "file"
            } else if directory.is_some() {
                "directory"
            } else {
                "absent"
            }
            .to_owned(),
            "listdir" => directory.map_or_else(
                || if file.is_some() { "file" } else { "absent" }.to_owned(),
                directory_digest,
            ),
            _ => anyhow::bail!("invalid native ledger operation"),
        };
        ensure!(
            *result == expected,
            "native read ledger differs from captured bytes"
        );
        if operation == "read" && address.starts_with("{source}/") && file.is_some() {
            reads.insert(address.as_str());
        }
    }
    ensure!(
        sources
            .keys()
            .all(|path| reads.contains(format!("{{source}}/{path}").as_str())),
        "native source read ledger incomplete"
    );
    Ok(())
}

fn directory_digest(entries: &BTreeMap<String, EntryKind>) -> String {
    sha256_hex(
        entries
            .iter()
            .map(|(name, kind)| {
                let kind = match kind {
                    EntryKind::File => "file",
                    EntryKind::Directory => "directory",
                };
                format!("{name}\0{kind}")
            })
            .collect::<Vec<_>>()
            .join("\0")
            .as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (Value, BTreeMap<String, Vec<u8>>, OperatorManifest) {
        let oracle: Value = serde_json::from_slice(include_bytes!(
            "../../../toolchain/python/native_oracle.json"
        ))
        .unwrap();
        let labels: Value = serde_json::from_slice(include_bytes!(
            "../../../toolchain/python/native_observation_oracle.json"
        ))
        .unwrap();
        let case = &oracle["cases"][0];
        assert_eq!(case["name"], "same_unit_direct");
        let raw = case["files"]["sample.py"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec();
        let label = &labels["cases"]["same_unit_direct"][0];
        let call = json!({"span": label["span"], "callee_span": label["callee_span"],
            "callee":"target", "callee_kind":"Name", "caller":"caller", "evaluation_context":"body"});
        let body_span = |first: usize, last: usize, column: usize| {
            let lines = raw
                .split_inclusive(|byte| *byte == b'\n')
                .collect::<Vec<_>>();
            json!({"start_line":first,"end_line":last,
                "start_byte":lines.iter().take(first-1).map(|line| line.len()).sum::<usize>(),
                "end_byte":lines.iter().take(last-1).map(|line| line.len()).sum::<usize>()+column})
        };
        let caller = json!({"name":"caller","qualified_name":"caller","kind":"FunctionDef",
            "span":body_span(3,4,20),"declaration_only":false,"decorators":[]});
        let target = json!({"name":"target","qualified_name":"target","kind":"FunctionDef",
            "span":body_span(1,2,16),"declaration_only":false,"decorators":[]});
        let operator = OperatorManifest {
            files: BTreeMap::from([("{compiler}/stub.pyi".to_owned(), sha256_hex(b"stub"))]),
            directories: BTreeMap::from([
                (
                    "{compiler}".to_owned(),
                    BTreeMap::from([("stub.pyi".to_owned(), EntryKind::File)]),
                ),
                ("{runtime}".to_owned(), BTreeMap::new()),
            ]),
        };
        let mut ledger = vec![
            ("cache", "{disabled-cache}", "disabled".to_owned()),
            ("read", "{source}/sample.py", sha256_hex(&raw)),
            ("read", "{compiler}/stub.pyi", sha256_hex(b"stub")),
            ("stat", "{source}/absent.py", "absent".to_owned()),
            ("stat", "{source}/sample.py", "file".to_owned()),
            ("listdir", "{source}", sha256_hex(b"sample.py\0file")),
        ];
        ledger.sort();
        let data = json!({"canonical_admission":false,"calls":case["expected_calls"],
            "evidence":[{"path":"sample.py","call":call,"caller":caller,"target_path":"sample.py",
                "target":target,"mechanism":"direct_native_ir_call"}],
            "observations":[{"path":"sample.py","call":call,"status":label["status"],"reason":label["reason"]}],
            "read_ledger":ledger});
        (
            data,
            BTreeMap::from([("sample.py".to_owned(), raw)]),
            operator,
        )
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One labelled mutation matrix accounts for the frozen refusal contract.
    fn response_controls_match_independently_frozen_contract() {
        let (baseline, sources, operator) = fixture();
        validate_response(&baseline, &sources, &operator).unwrap();
        let contract: Value = serde_json::from_slice(include_bytes!(
            "../../../toolchain/python/native_response_contract_oracle.json"
        ))
        .unwrap();
        let mut exercised = BTreeSet::new();
        for (name, pointer, replacement) in [
            (
                "missing_nullable_caller",
                "/observations/0/call/caller",
                Value::Null,
            ),
            (
                "changed_cache_sentinel",
                "/read_ledger/0/2",
                json!("enabled"),
            ),
            (
                "object_mechanism",
                "/evidence/0/mechanism",
                json!({"direct_native_ir_call":null}),
            ),
            (
                "object_status",
                "/observations/0/status",
                json!({"eligible_candidate":null}),
            ),
            (
                "object_reason",
                "/observations/0/reason",
                json!({"direct_native_ir_call":null}),
            ),
            ("unknown_response_field", "/extra", json!(true)),
            (
                "unknown_nested_field",
                "/evidence/0/call/extra",
                json!(true),
            ),
            (
                "canonical_admission_true",
                "/canonical_admission",
                json!(true),
            ),
            (
                "unknown_mechanism",
                "/evidence/0/mechanism",
                json!("name_only"),
            ),
            (
                "unknown_status",
                "/observations/0/status",
                json!("resolved"),
            ),
            (
                "unknown_reason",
                "/observations/0/reason",
                json!("inferred"),
            ),
            ("candidate_without_evidence", "/evidence", json!([])),
            ("evidence_without_candidate", "/observations", json!([])),
            ("legacy_calls_disagree", "/calls", json!([])),
            (
                "invalid_call_span",
                "/evidence/0/call/span/end_byte",
                json!(usize::MAX),
            ),
            (
                "invalid_callee_span",
                "/evidence/0/call/callee_span/start_byte",
                json!(0),
            ),
            (
                "changed_callee_spelling",
                "/evidence/0/call/callee",
                json!("other"),
            ),
            (
                "invalid_body_span",
                "/evidence/0/target/span/end_line",
                json!(999),
            ),
            (
                "target_declaration_only",
                "/evidence/0/target/declaration_only",
                json!(true),
            ),
            (
                "target_async",
                "/evidence/0/target/kind",
                json!("AsyncFunctionDef"),
            ),
            (
                "target_decorated",
                "/evidence/0/target/decorators",
                json!(["decorator"]),
            ),
            (
                "unsupported_evidence_context",
                "/evidence/0/call/evaluation_context",
                json!("definition_setup"),
            ),
            (
                "unknown_source_path",
                "/evidence/0/target_path",
                json!("outside.py"),
            ),
        ] {
            let mut data = baseline.clone();
            if name == "missing_nullable_caller" {
                data["calls"] = json!([]);
                data["evidence"] = json!([]);
                data["observations"][0]["status"] = json!("withheld");
                data["observations"][0]["reason"] = json!("unsupported_evaluation_context");
                data["observations"][0]["call"]["evaluation_context"] = json!("lambda");
                data["observations"][0]["call"]["caller"] = Value::Null;
                validate_response(&data, &sources, &operator).unwrap();
                data["observations"][0]["call"]
                    .as_object_mut()
                    .unwrap()
                    .remove("caller");
            } else if pointer.ends_with("/extra") {
                let (parent, _) = pointer.rsplit_once('/').unwrap();
                let object = if parent.is_empty() {
                    &mut data
                } else {
                    data.pointer_mut(parent).unwrap()
                };
                object
                    .as_object_mut()
                    .unwrap()
                    .insert("extra".to_owned(), replacement);
            } else {
                *data.pointer_mut(pointer).unwrap() = replacement;
            }
            assert!(
                validate_response(&data, &sources, &operator).is_err(),
                "accepted {name}"
            );
            exercised.insert(name);
        }
        for (name, array) in [
            ("duplicate_observation", "observations"),
            ("duplicate_evidence", "evidence"),
            ("duplicate_ledger_entry", "read_ledger"),
        ] {
            let mut data = baseline.clone();
            let first = data[array][0].clone();
            data[array].as_array_mut().unwrap().push(first);
            assert!(
                validate_response(&data, &sources, &operator).is_err(),
                "accepted {name}"
            );
            exercised.insert(name);
        }
        for name in [
            "missing_source_read",
            "changed_source_read_digest",
            "invalid_ledger_operation",
            "invalid_ledger_address",
            "changed_negative_lookup",
        ] {
            let mut data = baseline.clone();
            let ledger = data["read_ledger"].as_array_mut().unwrap();
            match name {
                "missing_source_read" => {
                    ledger
                        .retain(|entry| !(entry[0] == "read" && entry[1] == "{source}/sample.py"));
                }
                "changed_source_read_digest" => {
                    ledger
                        .iter_mut()
                        .find(|entry| entry[0] == "read" && entry[1] == "{source}/sample.py")
                        .unwrap()[2] = json!(sha256_hex(b"changed"));
                }
                "invalid_ledger_operation" => ledger[0][0] = json!("write"),
                "invalid_ledger_address" => ledger[0][1] = json!("{source}/../outside.py"),
                "changed_negative_lookup" => {
                    ledger
                        .iter_mut()
                        .find(|entry| entry[1] == "{source}/absent.py")
                        .unwrap()[2] = json!("file");
                }
                _ => unreachable!(),
            }
            assert!(
                validate_response(&data, &sources, &operator).is_err(),
                "accepted {name}"
            );
            exercised.insert(name);
        }
        assert_eq!(
            exercised,
            contract["rejected"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect()
        );
    }

    #[test]
    fn withheld_sites_and_negative_reads_are_valid_observations() {
        let (mut data, sources, operator) = fixture();
        data["calls"] = json!([]);
        data["evidence"] = json!([]);
        data["observations"][0]["status"] = json!("withheld");
        data["observations"][0]["reason"] = json!("no_unique_supported_native_body");
        validate_response(&data, &sources, &operator).unwrap();
        data["observations"][0]["reason"] = json!("unsupported_evaluation_context");
        assert!(validate_response(&data, &sources, &operator).is_err());
        data["observations"][0]["call"]["evaluation_context"] = json!("lambda");
        data["observations"][0]["call"]["caller"] = Value::Null;
        validate_response(&data, &sources, &operator).unwrap();
    }
}
