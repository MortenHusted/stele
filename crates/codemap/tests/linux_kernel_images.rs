#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use codemap::linux_guard::LinuxGuard;
use codemap::linux_kernel_images::{LinuxKernelPins, LinuxVsyscallPolicy};
use codemap::linux_process::{LinuxLaunch, OwnedLinuxChild, StoppedLinuxChild};
use codemap::linux_runtime::{
    AnonymousExecutePolicy, CapturedLinuxRuntime, LinuxArchitecture, LinuxRuntimeFile,
    LinuxRuntimeManifest, LinuxRuntimeRole,
};
use codemap::schema::sha256_hex;

fn runtime(fixture: &Path, policy: AnonymousExecutePolicy) -> CapturedLinuxRuntime {
    CapturedLinuxRuntime::capture(&LinuxRuntimeManifest {
        architecture: if cfg!(target_arch = "x86_64") {
            LinuxArchitecture::X86_64
        } else {
            LinuxArchitecture::Aarch64
        },
        anonymous_execute: policy,
        files: BTreeMap::from([(
            "fixture".into(),
            LinuxRuntimeFile {
                path: fixture.to_path_buf(),
                sha256: sha256_hex(&fs::read(fixture).unwrap()),
                role: LinuxRuntimeRole::Clangd,
            },
        )]),
    })
    .unwrap()
}

fn spawn(
    fixture: &Path,
    runtime: &CapturedLinuxRuntime,
    directory: &Path,
    guarded: bool,
) -> OwnedLinuxChild {
    fs::create_dir(directory).unwrap();
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    let ready = directory.join("ready");
    let mut command = Command::new(fixture);
    command.arg("wait").arg(&ready);
    let child = if guarded {
        let guard = LinuxGuard::prepare(runtime, &[], &[], directory).unwrap();
        OwnedLinuxChild::spawn_guarded(
            LinuxLaunch {
                executable: fixture.to_path_buf(),
                arguments: vec!["wait".into(), ready.as_os_str().to_owned()],
                environment: BTreeMap::new(),
                directory: directory.to_path_buf(),
            },
            guard,
        )
        .unwrap()
    } else {
        OwnedLinuxChild::spawn(&mut command).unwrap()
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read(&ready).ok().as_deref() != Some(b"R") {
        assert!(
            Instant::now() < deadline,
            "kernel fixture readiness deadline exceeded"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    child
}

fn record(
    observed: &mut BTreeMap<&'static str, &'static str>,
    label: &'static str,
    result: anyhow::Result<()>,
    reason: Option<&str>,
) {
    let accepted = result.is_ok();
    if let Some(reason) = reason {
        let error = result.expect_err(label).to_string();
        assert!(error.contains(reason), "{label}: {error}");
    } else {
        result.expect(label);
    }
    assert!(
        observed
            .insert(label, if accepted { "accept" } else { "reject" })
            .is_none()
    );
}

#[test]
#[ignore = "requires native ordinary-parent Linux and operator-compiled static kernel fixture"]
fn frozen_real_kernel_image_controls() {
    let fixture: PathBuf = fs::canonicalize(
        std::env::var_os("CODEMAP_LINUX_KERNEL_FIXTURE").expect("operator fixture required"),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let directory = fs::canonicalize(root.path()).unwrap();
    let disk_digest = sha256_hex(&fs::read(&fixture).unwrap());
    let runtime = runtime(&fixture, AnonymousExecutePolicy::Deny);
    let (kernel, bytes) = operator_kernel_pins(&fixture, &directory);
    let mut observed = BTreeMap::new();
    fingerprint_control(&kernel, &runtime, &mut observed);
    let mut changed = kernel.clone();
    changed.vdso_sha256 = "0".repeat(64);
    let mut child = spawn(&fixture, &runtime, &directory.join("guarded"), true);
    let id = child.id();
    let stopped = StoppedLinuxChild::capture(&mut child).unwrap();
    record(
        &mut observed,
        "guarded_real_kernel_image",
        runtime.verify_complete_images(&stopped, &kernel),
        None,
    );
    record(
        &mut observed,
        "real_vdso_wrong_digest",
        runtime.verify_complete_images(&stopped, &changed),
        Some("operator byte pins"),
    );
    changed = kernel.clone();
    changed.vdso_size += 4096;
    record(
        &mut observed,
        "real_vdso_wrong_size",
        runtime.verify_complete_images(&stopped, &changed),
        Some("invalid Linux vDSO mapping"),
    );
    let allowed = self::runtime(&fixture, AnonymousExecutePolicy::Allow);
    record(
        &mut observed,
        "unaccepted_anonymous_policy",
        allowed.verify_complete_images(&stopped, &kernel),
        Some("extension is not accepted"),
    );
    tamper_vdso(&stopped, id, &runtime, &kernel, &bytes, &mut observed);
    assert_eq!(sha256_hex(&fs::read(&fixture).unwrap()), disk_digest);
    stopped.resume().unwrap();
    child.terminate().unwrap();
    let mut unguarded = spawn(&fixture, &runtime, &directory.join("unguarded"), false);
    let stopped = StoppedLinuxChild::capture(&mut unguarded).unwrap();
    record(
        &mut observed,
        "unguarded_real_child",
        runtime.verify_complete_images(&stopped, &kernel),
        Some("requires the prepared process guard"),
    );
    stopped.resume().unwrap();
    unguarded.terminate().unwrap();
    earlier_exec_refusal(&fixture, &runtime, &kernel, &directory, &mut observed);
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../toolchain/linux/kernel_image_expectations.json"
    ))
    .unwrap();
    let labels = [
        "guarded_real_kernel_image",
        "kernel_fingerprint_reproducible",
        "unguarded_real_child",
        "real_vdso_wrong_digest",
        "real_vdso_wrong_size",
        "real_vdso_memory_tampered",
        "unaccepted_anonymous_policy",
        "earlier_exec_callback",
    ];
    let expected: BTreeMap<_, _> = labels
        .into_iter()
        .map(|label| (label, oracle["expected"][label].as_str().unwrap()))
        .collect();
    assert_eq!(observed, expected);
}

fn earlier_exec_refusal(
    fixture: &Path,
    runtime: &CapturedLinuxRuntime,
    kernel: &LinuxKernelPins,
    directory: &Path,
    observed: &mut BTreeMap<&'static str, &'static str>,
) {
    let earlier_state = directory.join("earlier-exec");
    fs::create_dir(&earlier_state).unwrap();
    let ready = earlier_state.join("ready");
    let executable = std::ffi::CString::new(fixture.as_os_str().as_encoded_bytes()).unwrap();
    let argument = std::ffi::CString::new(ready.as_os_str().as_encoded_bytes()).unwrap();
    let mut command = Command::new(fixture);
    // SAFETY: trusted adversarial control execs the pinned fixture using initialized arguments.
    unsafe {
        command.pre_exec(move || {
            let arguments = [
                executable.as_ptr(),
                c"wait".as_ptr(),
                argument.as_ptr(),
                std::ptr::null(),
            ];
            libc::execv(executable.as_ptr(), arguments.as_ptr());
            Err(std::io::Error::last_os_error())
        });
    }
    let mut earlier = OwnedLinuxChild::spawn(&mut command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read(&ready).ok().as_deref() != Some(b"R") {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let stopped = StoppedLinuxChild::capture(&mut earlier).unwrap();
    record(
        observed,
        "earlier_exec_callback",
        runtime.verify_complete_images(&stopped, kernel),
        Some("requires the prepared process guard"),
    );
    stopped.resume().unwrap();
    earlier.terminate().unwrap();
}

fn tamper_vdso(
    stopped: &StoppedLinuxChild<'_>,
    id: u32,
    runtime: &CapturedLinuxRuntime,
    kernel: &LinuxKernelPins,
    bytes: &[u8],
    observed: &mut BTreeMap<&'static str, &'static str>,
) {
    let address = stopped
        .maps()
        .unwrap()
        .lines()
        .find(|line| line.ends_with("[vdso]"))
        .unwrap()
        .split_once('-')
        .unwrap()
        .0
        .to_owned();
    let address = u64::from_str_radix(&address, 16).unwrap();
    let memory = OpenOptions::new()
        .write(true)
        .open(format!("/proc/{id}/mem"))
        .unwrap();
    memory.write_all_at(&[bytes[0] ^ 1], address).unwrap();
    record(
        observed,
        "real_vdso_memory_tampered",
        runtime.verify_complete_images(stopped, kernel),
        Some("operator byte pins"),
    );
    memory.write_all_at(&bytes[..1], address).unwrap();
    runtime.verify_complete_images(stopped, kernel).unwrap();
}

fn fingerprint_control(
    kernel: &LinuxKernelPins,
    runtime: &CapturedLinuxRuntime,
    observed: &mut BTreeMap<&'static str, &'static str>,
) {
    let fingerprint = kernel.fingerprint(runtime).unwrap();
    assert_eq!(fingerprint, kernel.clone().fingerprint(runtime).unwrap());
    let mut changed = kernel.clone();
    changed.vdso_sha256 = "0".repeat(64);
    assert_ne!(fingerprint, changed.fingerprint(runtime).unwrap());
    observed.insert("kernel_fingerprint_reproducible", "accept");
}

fn operator_kernel_pins(fixture: &Path, directory: &Path) -> (LinuxKernelPins, Vec<u8>) {
    let image = directory.join("operator-vdso");
    assert!(
        Command::new(fixture)
            .arg("dump")
            .arg(&image)
            .status()
            .unwrap()
            .success()
    );
    let bytes = fs::read(image).unwrap();
    let host_maps = fs::read_to_string("/proc/self/maps").unwrap();
    (
        LinuxKernelPins {
            architecture: if cfg!(target_arch = "x86_64") {
                LinuxArchitecture::X86_64
            } else {
                LinuxArchitecture::Aarch64
            },
            vdso_size: bytes.len() as u64,
            vdso_sha256: sha256_hex(&bytes),
            vsyscall: if host_maps.lines().any(|line| line.ends_with("[vsyscall]")) {
                LinuxVsyscallPolicy::FixedEmulated
            } else {
                LinuxVsyscallPolicy::Absent
            },
        },
        bytes,
    )
}
