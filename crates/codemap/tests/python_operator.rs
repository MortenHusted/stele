//! Explicit U1 operator capture only; no Python snapshot admission.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use codemap::python_inputs::{CapturedOperator, OperatorManifest, OperatorRoots};
use codemap::schema::sha256_hex;

fn manifest_bytes(path: &PathBuf) -> Vec<u8> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .unwrap();
    let budget = 32 * 1024 * 1024;
    let metadata = file.metadata().unwrap();
    assert!(metadata.is_file() && metadata.len() <= budget);
    let mut bytes = Vec::new();
    (&mut file)
        .take(budget + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() as u64 <= budget);
    bytes
}

#[test]
#[ignore = "requires an explicit pinned closed Python interpreter/compiler operator bundle"]
fn captures_explicit_operator_bundle() {
    capture_bundle(Some(PathBuf::from(
        std::env::var_os("CODEMAP_PYTHON_OPERATOR_COMPILER").expect("compiler root"),
    )));
}

#[test]
#[ignore = "requires an explicit pinned closed Python parser runtime without compiler inputs"]
fn captures_explicit_parser_runtime() {
    assert!(std::env::var_os("CODEMAP_PYTHON_OPERATOR_COMPILER").is_none());
    capture_bundle(None);
}

#[allow(clippy::literal_string_with_formatting_args)] // Captured logical roots are literal placeholders.
fn capture_bundle(compiler: Option<PathBuf>) {
    let path = PathBuf::from(
        std::env::var_os("CODEMAP_PYTHON_OPERATOR_MANIFEST").expect("operator manifest"),
    );
    let pin = std::env::var("CODEMAP_PYTHON_OPERATOR_MANIFEST_SHA256").expect("manifest pin");
    let bytes = manifest_bytes(&path);
    assert_eq!(sha256_hex(&bytes), pin);
    let manifest: OperatorManifest = serde_json::from_slice(&bytes).unwrap();
    let roots = OperatorRoots {
        runtime: PathBuf::from(
            std::env::var_os("CODEMAP_PYTHON_OPERATOR_RUNTIME").expect("runtime root"),
        ),
        compiler,
    };
    let scratch = tempfile::tempdir().unwrap();
    let private = fs::canonicalize(scratch.path()).unwrap();
    let source = private.join("source");
    fs::create_dir(&source).unwrap();
    let captured = CapturedOperator::capture(&manifest, &roots, &source).unwrap();
    assert_eq!(
        captured.fingerprint(),
        std::env::var("CODEMAP_PYTHON_OPERATOR_FINGERPRINT")
            .expect("independent logical fingerprint")
    );
    for name in [
        "{runtime}/bin/python3.14",
        "{runtime}/lib/python3.14/ast.py",
    ] {
        assert_eq!(
            sha256_hex(captured.read(name).unwrap()),
            manifest.files[name]
        );
    }
    if roots.compiler.is_some() {
        let name = "{compiler}/mypy/__init__.py";
        assert_eq!(
            sha256_hex(captured.read(name).unwrap()),
            manifest.files[name]
        );
    } else {
        assert!(captured.read("{compiler}/mypy/__init__.py").is_err());
        assert!(!manifest.directories.contains_key("{compiler}"));
    }
    assert!(captured.read("{runtime}/lib/python314.zip").is_err());
    assert!(
        !captured.manifest().directories["{runtime}/lib/python3.14"].contains_key("site-packages")
    );
    let repeat = CapturedOperator::capture(&manifest, &roots, &source).unwrap();
    assert_eq!(captured.fingerprint(), repeat.fingerprint());
    let copied = captured.materialize(&private.join("operator")).unwrap();
    if roots.compiler.is_none() {
        assert!(copied.compiler.is_none());
        assert!(!private.join("operator/compiler").exists());
    }
    let copied_capture = CapturedOperator::capture(&manifest, &copied, &source).unwrap();
    assert_eq!(captured.fingerprint(), copied_capture.fingerprint());
    for name in manifest.files.keys() {
        assert_eq!(
            captured.read(name).unwrap(),
            copied_capture.read(name).unwrap()
        );
    }
    captured.verify_unchanged().unwrap();
    assert_eq!(sha256_hex(&manifest_bytes(&path)), pin);
}
