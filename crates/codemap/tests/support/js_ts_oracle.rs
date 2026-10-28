use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

#[derive(Deserialize)]
struct Oracle {
    supported_calls: BTreeMap<String, [String; 4]>,
    noncanonical_sites: BTreeSet<String>,
    target_bodies: BTreeMap<String, [u64; 2]>,
}

pub fn assert_oracle(root: &Path, result: &Value) {
    let expected: Oracle =
        serde_json::from_slice(&fs::read(root.join("expected.json")).unwrap()).unwrap();
    let mut actual = BTreeMap::new();
    let mut observed = BTreeSet::new();
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if !entry.file_type().unwrap().is_file() {
            continue;
        }
        let text = fs::read_to_string(entry.path()).unwrap();
        for (line, text) in text.lines().enumerate() {
            let Some((_, label)) = text.split_once("// site:") else {
                continue;
            };
            let sites: Vec<_> = result["sites"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|site| {
                    site["location"]["path"] == entry.file_name().to_str().unwrap()
                        && site["location"]["line"] == line + 1
                })
                .collect();
            assert_eq!(sites.len(), 1, "label {label}");
            observed.insert(label.to_owned());
            let site = sites[0];
            if site["admission"]["reason"] == "proven_binding_candidate" {
                let target = &site["admission"]["target"];
                let key = format!(
                    "{}::{}",
                    target["path"].as_str().unwrap(),
                    site["symbol"].as_str().unwrap()
                );
                let body = &site["admission"]["target_body"];
                assert_eq!(body["path"], target["path"]);
                assert_eq!(
                    [
                        body["start_byte"].as_u64().unwrap(),
                        body["end_byte"].as_u64().unwrap()
                    ],
                    expected.target_bodies[&key],
                    "independent target body for {label}"
                );
                let caller = &site["admission"]["caller"];
                let bytes = fs::read(root.join(caller["path"].as_str().unwrap())).unwrap();
                let span = &bytes[caller["start_byte"].as_u64().unwrap() as usize
                    ..caller["end_byte"].as_u64().unwrap() as usize];
                let caller_name = String::from_utf8_lossy(span)
                    .split("function ")
                    .nth(1)
                    .unwrap()
                    .split('(')
                    .next()
                    .unwrap()
                    .trim()
                    .to_owned();
                actual.insert(
                    label.to_owned(),
                    [
                        entry.file_name().to_str().unwrap().to_owned(),
                        caller_name,
                        site["admission"]["target"]["path"]
                            .as_str()
                            .unwrap()
                            .to_owned(),
                        site["symbol"].as_str().unwrap().to_owned(),
                    ],
                );
            }
        }
    }
    assert_eq!(actual, expected.supported_calls);
    assert_eq!(
        observed,
        expected
            .supported_calls
            .keys()
            .cloned()
            .chain(expected.noncanonical_sites)
            .collect()
    );
    assert_eq!(observed.len(), result["sites"].as_array().unwrap().len());
    let unicode = result["sites"]
        .as_array()
        .unwrap()
        .iter()
        .find(|site| site["location"]["path"] == "unicode.ts")
        .unwrap();
    let range = &unicode["location"];
    let bytes = fs::read(root.join("unicode.ts")).unwrap();
    assert_eq!(
        &bytes[range["start_byte"].as_u64().unwrap() as usize
            ..range["end_byte"].as_u64().unwrap() as usize],
        "π()".as_bytes()
    );
    assert!(range["start_byte"].as_u64() > range["start"].as_u64());
}
