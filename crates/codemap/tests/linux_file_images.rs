#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use codemap::linux_process::{OwnedLinuxChild, StoppedLinuxChild};
use codemap::linux_runtime::{
    AnonymousExecutePolicy, CapturedLinuxRuntime, LinuxArchitecture, LinuxRuntimeFile,
    LinuxRuntimeManifest, LinuxRuntimeRole,
};
use codemap::schema::sha256_hex;

fn manifest(executable: &Path) -> LinuxRuntimeManifest {
    LinuxRuntimeManifest {
        architecture: if cfg!(target_arch = "x86_64") {
            LinuxArchitecture::X86_64
        } else {
            LinuxArchitecture::Aarch64
        },
        anonymous_execute: AnonymousExecutePolicy::Deny,
        files: BTreeMap::from([(
            "fixture".to_owned(),
            LinuxRuntimeFile {
                path: executable.to_path_buf(),
                sha256: sha256_hex(&fs::read(executable).unwrap()),
                role: LinuxRuntimeRole::Clangd,
            },
        )]),
    }
}

fn spawn(executable: &Path, mode: &str, ready: &Path, mapped: &Path) -> OwnedLinuxChild {
    let mut command = Command::new(executable);
    command
        .args([Path::new(mode), ready, mapped])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = OwnedLinuxChild::spawn(&mut command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match fs::read(ready) {
            Ok(bytes) if bytes == b"R" => break,
            Ok(bytes) if bytes.is_empty() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            result => panic!("invalid trusted fixture readiness: {result:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "trusted fixture never became ready: {mode}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    child
}

#[test]
#[ignore = "requires operator-compiled static fixture and native ordinary-user Linux execution"]
fn frozen_live_file_mapping_controls() {
    // SAFETY: geteuid has no arguments or side effects.
    assert_ne!(unsafe { libc::geteuid() }, 0);
    let fixture = fs::canonicalize(
        std::env::var_os("CODEMAP_LINUX_FILE_FIXTURE").expect("operator fixture required"),
    )
    .unwrap();
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../toolchain/linux/file_mapping_expectations.json"
    ))
    .unwrap();
    let mut observed = BTreeMap::new();
    for label in oracle["live"].as_object().unwrap().keys() {
        let root = tempfile::tempdir().unwrap();
        let directory = fs::canonicalize(root.path()).unwrap();
        let executable = directory.join("fixture");
        fs::copy(&fixture, &executable).unwrap();
        let data = directory.join("data");
        fs::write(&data, vec![b'D'; 65536]).unwrap();
        let (pins, mode, mapped) = case_pins(label, &fixture, &executable, &data, &directory);
        let runtime = CapturedLinuxRuntime::capture(&pins).unwrap();
        let mut child = spawn(&executable, mode, &directory.join("ready"), &mapped);
        let stopped = StoppedLinuxChild::capture(&mut child).unwrap();
        match label.as_str() {
            "replaced_pinned_path" => {
                let replacement = directory.join("replacement");
                fs::copy(&fixture, &replacement).unwrap();
                fs::rename(replacement, &executable).unwrap();
            }
            "changed_pinned_bytes" => {
                fs::write(&data, b"changed").unwrap();
            }
            _ => {}
        }
        let result = if label == "unreadable_memory" {
            stopped.read_memory(0, &mut [0])
        } else {
            runtime.verify_file_backed_images(&stopped)
        };
        if label == "static_fixture" {
            result.unwrap();
            observed.insert(label.clone(), "accept");
        } else {
            let error = result.unwrap_err().to_string();
            let expected = match label.as_str() {
                "private_text_changed_disk_unchanged" => "executable memory differs",
                "writable_executable_file" => "private and read-only",
                "extra_mapped_data" | "hardlink_mapped_alias" => "outside pinned closure",
                "deleted_mapped_data" => "unverifiable Linux map path",
                "missing_required_library" => "not mapped",
                "replaced_pinned_path" | "changed_pinned_bytes" => "changed or was replaced",
                "unreadable_memory" => "memory witness unavailable",
                "executable_outside_load" => "outside unique ELF load",
                _ => panic!("unknown frozen control {label}"),
            };
            assert!(error.contains(expected), "{label}: {error}");
            observed.insert(label.clone(), "reject");
        }
        if label == "private_text_changed_disk_unchanged" {
            assert_eq!(
                sha256_hex(&fs::read(&executable).unwrap()),
                pins.files["fixture"].sha256
            );
        }
        stopped.resume().unwrap();
        child.terminate().unwrap();
    }
    assert_eq!(serde_json::to_value(observed).unwrap(), oracle["live"]);
}

fn case_pins(
    label: &str,
    fixture: &Path,
    executable: &Path,
    data: &Path,
    directory: &Path,
) -> (LinuxRuntimeManifest, &'static str, std::path::PathBuf) {
    let mut pins = manifest(executable);
    let (mode, mapped) = match label {
        "private_text_changed_disk_unchanged" => ("tamper", data.to_path_buf()),
        "writable_executable_file" => ("writable", data.to_path_buf()),
        "extra_mapped_data" => ("extra", data.to_path_buf()),
        "deleted_mapped_data" => ("deleted", data.to_path_buf()),
        "hardlink_mapped_alias" => {
            pins.files.insert(
                "data".into(),
                LinuxRuntimeFile {
                    path: data.to_path_buf(),
                    sha256: sha256_hex(&fs::read(data).unwrap()),
                    role: LinuxRuntimeRole::Data,
                },
            );
            let alias = directory.join("alias");
            fs::hard_link(data, &alias).unwrap();
            ("extra", alias)
        }
        "executable_outside_load" => ("outside", executable.to_path_buf()),
        _ => ("ordinary", data.to_path_buf()),
    };
    if label == "changed_pinned_bytes" {
        pins.files.insert(
            "data".into(),
            LinuxRuntimeFile {
                path: data.to_path_buf(),
                sha256: sha256_hex(&fs::read(data).unwrap()),
                role: LinuxRuntimeRole::Data,
            },
        );
    }
    if label == "missing_required_library" {
        let library = directory.join("library");
        let mut elf = fs::read(fixture).unwrap();
        // Descriptor admits this ET_DYN file; the fixture never maps it.
        elf[16..18].copy_from_slice(&3u16.to_le_bytes());
        fs::write(&library, &elf).unwrap();
        pins.files.insert(
            "library".into(),
            LinuxRuntimeFile {
                path: library,
                sha256: sha256_hex(&elf),
                role: LinuxRuntimeRole::Library,
            },
        );
    }
    (pins, mode, mapped)
}
