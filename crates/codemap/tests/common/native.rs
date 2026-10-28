use std::fs;

use codemap::python_inputs::OperatorRoots;
use codemap::python_native::NativeConfig;
use codemap::schema::sha256_hex;

pub fn config() -> NativeConfig {
    let raw = fs::read(std::env::var_os("CODEMAP_PYTHON_OPERATOR_MANIFEST").unwrap()).unwrap();
    assert_eq!(
        sha256_hex(&raw),
        std::env::var("CODEMAP_PYTHON_OPERATOR_MANIFEST_SHA256").unwrap()
    );
    NativeConfig {
        roots: OperatorRoots {
            runtime: std::env::var_os("CODEMAP_PYTHON_OPERATOR_RUNTIME")
                .unwrap()
                .into(),
            compiler: Some(
                std::env::var_os("CODEMAP_PYTHON_OPERATOR_COMPILER")
                    .unwrap()
                    .into(),
            ),
        },
        inputs: serde_json::from_slice(&raw).unwrap(),
        runtime: serde_json::from_slice(
            &fs::read(std::env::var_os("CODEMAP_PYTHON_RUNTIME_MANIFEST").unwrap()).unwrap(),
        )
        .unwrap(),
    }
}
