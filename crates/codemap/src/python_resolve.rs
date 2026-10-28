//! Original-body matching is internal; the producer obtains both owned witnesses.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};

use crate::python_evidence::{NativeCall, NativeDefinition, Response};
use crate::python_native::NativeResult;
use crate::python_parser::{Call, Definition, ParsedFile, ParsedSources};
use crate::schema::sha256_hex;

#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedCall {
    pub path: String,
    pub call: usize,
    pub caller: usize,
    pub target_path: String,
    pub target: usize,
}

pub fn resolve(
    native: &NativeResult,
    parsed: &ParsedSources,
    sources: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<ResolvedCall>> {
    let pins: BTreeMap<_, _> = sources
        .iter()
        .map(|(path, raw)| (path, sha256_hex(raw)))
        .collect();
    ensure!(
        parsed.identity["sources"] == serde_json::to_value(&pins)?
            && parsed.fingerprint == sha256_hex(&serde_json::to_vec(&parsed.identity)?),
        "Python parser identity differs from original source map"
    );
    ensure!(
        parsed
            .files
            .iter()
            .map(|file| &file.path)
            .eq(sources.keys())
            && parsed
                .files
                .iter()
                .all(|file| pins.get(&file.path) == Some(&file.sha256)),
        "Python parser census differs from original source inventory"
    );
    ensure!(
        native
            .profile()
            .sources()
            .iter()
            .all(|(path, hash)| pins.get(path) == Some(hash)),
        "native compilation inputs differ from original source map"
    );
    match_bodies(native.evidence(), &parsed.files, native.profile().sources())
}

struct FileIndex<'a> {
    file: &'a ParsedFile,
    sites: BTreeMap<(usize, usize), usize>,
    bodies: BTreeMap<(usize, usize), usize>,
}

fn match_bodies(
    response: &Response,
    files: &[ParsedFile],
    selected: &BTreeMap<String, String>,
) -> Result<Vec<ResolvedCall>> {
    let mut indices = BTreeMap::new();
    for file in files
        .iter()
        .filter(|file| selected.contains_key(&file.path))
    {
        ensure!(
            selected[&file.path] == file.sha256,
            "native/parser source hash disagreement"
        );
        let index = FileIndex::new(file)?;
        ensure!(
            indices.insert(file.path.as_str(), index).is_none(),
            "duplicate Python source census"
        );
    }
    ensure!(
        indices.len() == selected.len(),
        "native source absent from Python parser census"
    );
    let mut remaining: BTreeSet<_> = indices
        .iter()
        .flat_map(|(path, index)| index.sites.keys().map(|(start, end)| (*path, *start, *end)))
        .collect();
    for observation in &response.observations {
        let index = indices
            .get(observation.path.as_str())
            .context("native observation outside selected sources")?;
        let ordinal = index.call(&observation.call)?;
        let call = &index.file.calls[ordinal];
        ensure!(
            remaining.remove(&(
                observation.path.as_str(),
                call.span.start_byte,
                call.span.end_byte
            )),
            "duplicate or extra native observation"
        );
    }
    ensure!(
        remaining.is_empty(),
        "native observations omit original Python sites"
    );
    let mut calls = Vec::new();
    for evidence in &response.evidence {
        let caller_file = indices
            .get(evidence.path.as_str())
            .context("native caller source not selected")?;
        let target_file = indices
            .get(evidence.target_path.as_str())
            .context("native target source not selected")?;
        let call = caller_file.call(&evidence.call)?;
        let caller = caller_file.body(&evidence.caller)?;
        let target = target_file.body(&evidence.target)?;
        ensure!(
            caller_file.file.calls[call].owner == Some(caller)
                && caller_file.file.calls[call].evaluation_context == "body"
                && matches!(
                    caller_file.file.definitions[caller].kind.as_str(),
                    "FunctionDef" | "AsyncFunctionDef"
                )
                && target_file.file.definitions[target].kind == "FunctionDef"
                && !target_file.file.definitions[target].declaration_only
                && target_file.file.definitions[target].decorators.is_empty(),
            "native evidence differs from closest Python callable body"
        );
        calls.push(ResolvedCall {
            path: evidence.path.clone(),
            call,
            caller,
            target_path: evidence.target_path.clone(),
            target,
        });
    }
    Ok(calls)
}

impl<'a> FileIndex<'a> {
    fn new(file: &'a ParsedFile) -> Result<Self> {
        let mut index = Self {
            file,
            sites: BTreeMap::new(),
            bodies: BTreeMap::new(),
        };
        for (ordinal, call) in file.calls.iter().enumerate() {
            ensure!(
                index
                    .sites
                    .insert((call.span.start_byte, call.span.end_byte), ordinal)
                    .is_none(),
                "duplicate Python original call site"
            );
        }
        for (ordinal, definition) in file.definitions.iter().enumerate() {
            ensure!(
                index
                    .bodies
                    .insert(
                        (definition.span.start_byte, definition.span.end_byte),
                        ordinal
                    )
                    .is_none(),
                "duplicate Python original body"
            );
        }
        Ok(index)
    }
    fn call(&self, witness: &NativeCall) -> Result<usize> {
        let ordinal = *self
            .sites
            .get(&(witness.span.start_byte, witness.span.end_byte))
            .context("native call has no original Python site")?;
        let call: &Call = &self.file.calls[ordinal];
        ensure!(
            call.span == witness.span
                && call.callee_span == witness.callee_span
                && call.callee == witness.callee
                && call.callee_kind == witness.callee_kind
                && call.caller == witness.caller
                && call.evaluation_context == witness.evaluation_context,
            "native call differs from original Python witness"
        );
        Ok(ordinal)
    }

    fn body(&self, witness: &NativeDefinition) -> Result<usize> {
        let ordinal = *self
            .bodies
            .get(&(witness.span.start_byte, witness.span.end_byte))
            .context("native body has no original Python definition")?;
        let definition: &Definition = &self.file.definitions[ordinal];
        ensure!(
            definition.span == witness.span
                && definition.name == witness.name
                && definition.qualified_name == witness.qualified_name
                && definition.kind == witness.kind
                && definition.declaration_only == witness.declaration_only
                && definition
                    .decorators
                    .iter()
                    .map(|value| value.spelling.as_str())
                    .eq(witness.decorators.iter().map(String::as_str)),
            "native body differs from original Python definition"
        );
        Ok(ordinal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn native_call(call: &Call) -> Value {
        let mut value = serde_json::to_value(call).unwrap();
        value.as_object_mut().unwrap().remove("owner");
        value
    }

    fn native_definition(definition: &Definition) -> Value {
        let mut value = serde_json::to_value(definition).unwrap();
        let object = value.as_object_mut().unwrap();
        for field in ["owner", "ordinal", "ambiguous", "bases"] {
            object.remove(field);
        }
        object.insert(
            "decorators".to_owned(),
            json!(
                definition
                    .decorators
                    .iter()
                    .map(|expression| &expression.spelling)
                    .collect::<Vec<_>>()
            ),
        );
        value
    }

    fn fixture(
        name: &str,
    ) -> (
        Response,
        Vec<ParsedFile>,
        BTreeMap<String, String>,
        Vec<ResolvedCall>,
    ) {
        let census: Value = serde_json::from_slice(include_bytes!(
            "../../../toolchain/python/original_body_census_fixture.json"
        ))
        .unwrap();
        let labels: Value = serde_json::from_slice(include_bytes!(
            "../../../toolchain/python/original_body_match_oracle.json"
        ))
        .unwrap();
        let observations: Value = serde_json::from_slice(include_bytes!(
            "../../../toolchain/python/native_observation_oracle.json"
        ))
        .unwrap();
        let oracle: Value = serde_json::from_slice(include_bytes!(
            "../../../toolchain/python/native_oracle.json"
        ))
        .unwrap();
        let case = oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == name)
            .unwrap();
        let files: Vec<ParsedFile> = serde_json::from_value(census["cases"][name].clone()).unwrap();
        let selected = files
            .iter()
            .map(|file| (file.path.clone(), file.sha256.clone()))
            .collect();
        let mut evidence = Vec::new();
        let mut expected = Vec::new();
        for (label, legacy) in labels["cases"][name]
            .as_array()
            .unwrap()
            .iter()
            .zip(case["expected_calls"].as_array().unwrap())
        {
            let coordinates = label["location_label"].as_array().unwrap();
            let caller_file = files.iter().find(|file| file.path == legacy[0]).unwrap();
            let target_file = files
                .iter()
                .find(|file| file.path == coordinates[8])
                .unwrap();
            let call = label["call_index"].as_u64().unwrap() as usize;
            let caller = label["caller_definition_index"].as_u64().unwrap() as usize;
            let target = label["target_definition_index"].as_u64().unwrap() as usize;
            expected.push(ResolvedCall {
                path: caller_file.path.clone(),
                call,
                caller,
                target_path: target_file.path.clone(),
                target,
            });
            evidence.push(json!({"path":caller_file.path,"call":native_call(&caller_file.calls[call]),
                "caller":native_definition(&caller_file.definitions[caller]),"target_path":target_file.path,
                "target":native_definition(&target_file.definitions[target]),"mechanism":"direct_native_ir_call"}));
        }
        let mut observed = Vec::new();
        for label in observations["cases"][name].as_array().unwrap() {
            let file = files
                .iter()
                .find(|file| file.path == label["path"])
                .unwrap();
            let call = file
                .calls
                .iter()
                .find(|call| {
                    serde_json::to_value(&call.span).unwrap() == label["span"]
                        && serde_json::to_value(&call.callee_span).unwrap() == label["callee_span"]
                })
                .unwrap();
            observed.push(json!({"path":file.path,"call":native_call(call),"status":label["status"],"reason":label["reason"]}));
        }
        let response = serde_json::from_value(json!({"canonical_admission":false,"calls":[],
            "evidence":evidence,"observations":observed,"read_ledger":[]}))
        .unwrap();
        (response, files, selected, expected)
    }

    #[test]
    fn original_body_matches_agree_with_every_independent_label() {
        let labels: Value = serde_json::from_slice(include_bytes!(
            "../../../toolchain/python/original_body_match_oracle.json"
        ))
        .unwrap();
        for name in labels["cases"].as_object().unwrap().keys() {
            let (response, files, selected, expected) = fixture(name);
            assert_eq!(
                match_bodies(&response, &files, &selected).unwrap(),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn original_body_matching_refuses_census_and_endpoint_corruption() {
        for mutation in [
            "callee",
            "callee_span",
            "context",
            "owner",
            "caller_kind",
            "caller_name",
            "target_name",
            "qualified_scope",
            "target_kind",
            "target_decorator",
            "body_span",
            "source_hash",
            "duplicate_site",
            "missing_site",
            "duplicate_body",
        ] {
            let (response, mut files, selected, _) = fixture("same_unit_direct");
            match mutation {
                "callee" => files[0].calls[0].callee = "other".to_owned(),
                "callee_span" => files[0].calls[0].callee_span.end_byte -= 1,
                "context" => files[0].calls[0].evaluation_context = "lambda".to_owned(),
                "owner" => files[0].calls[0].owner = Some(0),
                "caller_kind" => files[0].definitions[1].kind = "ClassDef".to_owned(),
                "caller_name" => files[0].definitions[1].name = "other".to_owned(),
                "target_name" => files[0].definitions[0].name = "other".to_owned(),
                "qualified_scope" => {
                    files[0].definitions[0].qualified_name = "scope.target".to_owned();
                }
                "target_kind" => files[0].definitions[0].kind = "AsyncFunctionDef".to_owned(),
                "target_decorator" => {
                    let span = files[0].definitions[0].span.clone();
                    files[0].definitions[0].decorators.push(
                        crate::python_parser::SourceExpression {
                            span,
                            spelling: "wrapper".to_owned(),
                            sha256: "0".repeat(64),
                        },
                    );
                }
                "body_span" => files[0].definitions[0].span.end_byte -= 1,
                "source_hash" => files[0].sha256 = "0".repeat(64),
                "duplicate_site" => {
                    let value = files[0].calls[0].clone();
                    files[0].calls.push(value);
                }
                "missing_site" => {
                    files[0].calls.clear();
                }
                "duplicate_body" => {
                    let value = files[0].definitions[0].clone();
                    files[0].definitions.push(value);
                }
                _ => unreachable!(),
            }
            assert!(
                match_bodies(&response, &files, &selected).is_err(),
                "accepted {mutation}"
            );
        }
        let (mut response, files, selected, _) = fixture("same_unit_direct");
        response.observations.clear();
        assert!(match_bodies(&response, &files, &selected).is_err());
    }
}
