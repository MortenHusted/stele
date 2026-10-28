use codemap::python_inputs::{OperatorManifest, OperatorRoots};
use codemap::python_parser::ParserConfig;
use codemap::schema::sha256_hex;
use std::fs;

pub fn config() -> ParserConfig {
    let manifest =
        fs::read(std::env::var_os("CODEMAP_PYTHON_OPERATOR_MANIFEST").expect("parser manifest"))
            .unwrap();
    assert_eq!(
        sha256_hex(&manifest),
        std::env::var("CODEMAP_PYTHON_OPERATOR_MANIFEST_SHA256").unwrap()
    );
    assert!(std::env::var_os("CODEMAP_PYTHON_OPERATOR_COMPILER").is_none());
    ParserConfig {
        roots: OperatorRoots {
            runtime: std::env::var_os("CODEMAP_PYTHON_OPERATOR_RUNTIME")
                .unwrap()
                .into(),
            compiler: None,
        },
        inputs: serde_json::from_slice::<OperatorManifest>(&manifest).unwrap(),
        runtime: serde_json::from_slice(
            &fs::read(std::env::var_os("CODEMAP_PYTHON_RUNTIME_MANIFEST").unwrap()).unwrap(),
        )
        .unwrap(),
    }
}
