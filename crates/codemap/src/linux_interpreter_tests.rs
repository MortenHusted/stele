use super::tests::{elf, fixture, replace_bytes};
use crate::linux_runtime::{
    CapturedLinuxRuntime, LinuxArchitecture, LinuxRuntimeFile, LinuxRuntimeRole,
};
use crate::schema::sha256_hex;
use std::{collections::BTreeSet, fs, os::unix::ffi::OsStrExt, os::unix::fs::symlink};

fn dynamic(interpreter: &[u8], duplicate: bool) -> Vec<u8> {
    let count = if duplicate { 3u16 } else { 2u16 };
    let offset = 64 + usize::from(count) * 56;
    let mut bytes = elf(62, 2);
    bytes.resize(offset + interpreter.len(), 0);
    bytes[56..58].copy_from_slice(&count.to_le_bytes());
    let size = bytes.len() as u64;
    bytes[96..104].copy_from_slice(&size.to_le_bytes());
    bytes[104..112].copy_from_slice(&size.to_le_bytes());
    for index in 1..usize::from(count) {
        let start = 64 + index * 56;
        bytes[start..start + 4].copy_from_slice(&3u32.to_le_bytes());
        bytes[start + 8..start + 16].copy_from_slice(&(offset as u64).to_le_bytes());
        bytes[start + 32..start + 40].copy_from_slice(&(interpreter.len() as u64).to_le_bytes());
    }
    bytes[offset..].copy_from_slice(interpreter);
    bytes
}

#[test]
fn frozen_interpreter_descriptor_controls() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../toolchain/linux/interpreter_expectations.json"
    ))
    .unwrap();
    let mut observed = BTreeSet::new();
    for label in oracle["descriptor"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
    {
        let (_root, mut manifest) = fixture(LinuxArchitecture::X86_64);
        let loader = manifest.files["library"].path.clone();
        manifest.files.get_mut("library").unwrap().role = LinuxRuntimeRole::Loader;
        let mut path = loader.as_os_str().as_bytes().to_vec();
        path.push(0);
        if !label.starts_with("static_") {
            replace_bytes(&mut manifest, "tool", &dynamic(&path, false));
        }
        match label {
            "static_without_loader" => {
                manifest.files.get_mut("library").unwrap().role = LinuxRuntimeRole::Library;
                CapturedLinuxRuntime::capture(&manifest)
                    .unwrap()
                    .verify_unchanged()
                    .unwrap();
            }
            "dynamic_exact_loader" => CapturedLinuxRuntime::capture(&manifest)
                .unwrap()
                .verify_unchanged()
                .unwrap(),
            "interpreter_alias_retarget" => {
                let alias = loader.with_file_name("interpreter-alias");
                symlink(&loader, &alias).unwrap();
                let mut path = alias.as_os_str().as_bytes().to_vec();
                path.push(0);
                replace_bytes(&mut manifest, "tool", &dynamic(&path, false));
                let runtime = CapturedLinuxRuntime::capture(&manifest).unwrap();
                let other = loader.with_file_name("other-loader");
                fs::copy(loader, &other).unwrap();
                fs::remove_file(&alias).unwrap();
                symlink(&other, &alias).unwrap();
                assert!(runtime.verify_unchanged().is_err());
            }
            "loader_pin_drift" | "loader_bytes_change_identity" => {
                let runtime = CapturedLinuxRuntime::capture(&manifest).unwrap();
                let mut bytes = fs::read(loader).unwrap();
                bytes[127] = 1;
                replace_bytes(&mut manifest, "library", &bytes);
                assert!(runtime.verify_unchanged().is_err());
                if label == "loader_bytes_change_identity" {
                    let changed = CapturedLinuxRuntime::capture(&manifest).unwrap();
                    assert_ne!(runtime.fingerprint(), changed.fingerprint());
                }
            }
            _ => assert_rejected_descriptor(label, &mut manifest, &loader, path),
        }
        assert!(observed.insert(label.to_owned()));
    }
    assert_eq!(observed.len(), 20);
}

fn assert_rejected_descriptor(
    label: &str,
    manifest: &mut super::LinuxRuntimeManifest,
    loader: &std::path::Path,
    mut path: Vec<u8>,
) {
    match label {
        "static_with_loader_rejected" => {}
        "dynamic_missing_loader" => {
            manifest.files.remove("library");
        }
        "dynamic_loader_as_library" => {
            manifest.files.get_mut("library").unwrap().role = LinuxRuntimeRole::Library;
        }
        "dynamic_wrong_loader" | "duplicate_loader" => {
            let other = loader.with_file_name("other-loader");
            fs::copy(loader, &other).unwrap();
            let pin = LinuxRuntimeFile {
                path: other,
                sha256: sha256_hex(&fs::read(loader).unwrap()),
                role: LinuxRuntimeRole::Loader,
            };
            if label == "dynamic_wrong_loader" {
                manifest.files.insert("library".into(), pin);
            } else {
                manifest.files.insert("other".into(), pin);
            }
        }
        "loader_wrong_architecture" => {
            replace_bytes(manifest, "library", &elf(183, 3));
        }
        "loader_not_et_dyn" => replace_bytes(manifest, "library", &elf(62, 2)),
        "loader_has_interpreter" => {
            let mut bytes = dynamic(&path, false);
            bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
            replace_bytes(manifest, "library", &bytes);
        }
        "duplicate_pt_interp" => {
            replace_bytes(manifest, "tool", &dynamic(&path, true));
        }
        "empty_interp" => replace_bytes(manifest, "tool", &dynamic(&[0], false)),
        "unterminated_interp" => {
            path.pop();
            replace_bytes(manifest, "tool", &dynamic(&path, false));
        }
        "interior_nul_interp" => {
            path[1] = 0;
            replace_bytes(manifest, "tool", &dynamic(&path, false));
        }
        "relative_interp" => {
            replace_bytes(manifest, "tool", &dynamic(b"relative\0", false));
        }
        "oversized_interp" => {
            let mut path = vec![b'a'; 4097];
            path[0] = b'/';
            path[4096] = 0;
            replace_bytes(manifest, "tool", &dynamic(&path, false));
        }
        "out_of_bounds_interp" => {
            let mut bytes = dynamic(&path, false);
            bytes[128..136].copy_from_slice(&u64::MAX.to_le_bytes());
            replace_bytes(manifest, "tool", &bytes);
        }
        _ => panic!("unimplemented descriptor control {label}"),
    }
    assert!(
        CapturedLinuxRuntime::capture(manifest).is_err(),
        "{label} must reject"
    );
}
