#![cfg(target_os = "linux")]

use std::fs;

use codemap::linux_runtime::{CapturedLinuxRuntime, LinuxArchitecture, LinuxRuntimeManifest};

/// Operator preparation supplies explicit real-runtime byte pins. This test
/// executes no application code and makes no loaded-image or sandbox claim.
#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires an explicit Linux operator runtime manifest"]
fn linux_operator_runtime_capture_relocation_and_drift() {
    let path = std::env::var("CODEMAP_LINUX_RUNTIME_MANIFEST").expect("explicit runtime manifest");
    let manifest: LinuxRuntimeManifest = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let expected = match std::env::consts::ARCH {
        "x86_64" => LinuxArchitecture::X86_64,
        "aarch64" => LinuxArchitecture::Aarch64,
        other => panic!("unsupported native architecture: {other}"),
    };
    assert_eq!(manifest.architecture, expected);
    let captured = CapturedLinuxRuntime::capture(&manifest).unwrap();
    captured.verify_unchanged().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temporary.path()).unwrap();
    let mut relocated = manifest;
    for (index, pin) in relocated.files.values_mut().enumerate() {
        let destination = root.join(format!("file-{index}"));
        fs::copy(&pin.path, &destination).unwrap();
        pin.path = destination;
    }
    let other = CapturedLinuxRuntime::capture(&relocated).unwrap();
    assert_eq!(captured.fingerprint(), other.fingerprint());
    for name in relocated.files.keys() {
        let pin = &relocated.files[name];
        let bytes = fs::read(&pin.path).unwrap();
        fs::write(&pin.path, b"changed operator bytes").unwrap();
        assert!(other.verify_unchanged().is_err());
        assert!(CapturedLinuxRuntime::capture(&relocated).is_err());
        fs::write(&pin.path, bytes).unwrap();
    }
    // Restoring identical bytes cannot restore the original captured witness.
    assert!(other.verify_unchanged().is_err());
    captured.verify_unchanged().unwrap();
}
