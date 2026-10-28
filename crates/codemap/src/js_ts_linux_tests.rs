//! Independent real helper protocol producers, with explicitly versioned fixed limits.
use super::*;
use crate::linux_kernel_images::LinuxVsyscallPolicy;
use crate::linux_runtime::{
    AnonymousExecutePolicy, LinuxArchitecture, LinuxRuntimeFile, LinuxRuntimeManifest,
    LinuxRuntimeRole,
};
use crate::schema::sha256_hex;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn runtime(executable: &Path) -> CapturedLinuxRuntime {
    CapturedLinuxRuntime::capture(&LinuxRuntimeManifest {
        architecture: architecture(),
        anonymous_execute: AnonymousExecutePolicy::Deny,
        files: BTreeMap::from([(
            "fixture".into(),
            LinuxRuntimeFile {
                path: executable.to_path_buf(),
                sha256: sha256_hex(&fs::read(executable).unwrap()),
                role: LinuxRuntimeRole::Node,
            },
        )]),
    })
    .unwrap()
}

const fn architecture() -> LinuxArchitecture {
    if cfg!(target_arch = "x86_64") {
        LinuxArchitecture::X86_64
    } else {
        LinuxArchitecture::Aarch64
    }
}

fn kernel() -> LinuxKernelPins {
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("vdso");
    let operator = std::env::var_os("CODEMAP_LINUX_KERNEL_FIXTURE").unwrap();
    assert!(
        Command::new(operator)
            .arg("dump")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let bytes = fs::read(path).unwrap();
    LinuxKernelPins {
        architecture: architecture(),
        vdso_size: bytes.len() as u64,
        vdso_sha256: sha256_hex(&bytes),
        vsyscall: if cfg!(target_arch = "x86_64") {
            LinuxVsyscallPolicy::FixedEmulated
        } else {
            LinuxVsyscallPolicy::Absent
        },
    }
}

fn exchange(mode: &str, executable: &Path, kernel: &LinuxKernelPins) -> Result<()> {
    let state = tempfile::tempdir()?;
    fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700))?;
    let runtime = runtime(executable);
    let guard = LinuxGuard::prepare(&runtime, &[], &[], state.path())?;
    let launch = LinuxLaunch {
        executable: executable.to_path_buf(),
        arguments: vec![mode.into()],
        environment: BTreeMap::new(),
        directory: state.path().to_path_buf(),
    };
    let mut session = HelperSession::spawn(launch, guard)?;
    let pid = session.child.as_ref().unwrap().id();
    let result = (|| -> Result<()> {
        ensure!(
            session.read_frame(4096)? == b"{\"phase\":\"ready\"}",
            "fixture ready mismatch"
        );
        let before = session.images(&runtime, kernel)?;
        session.control(b'1')?;
        ensure!(
            session.read_frame(OUTPUT_LIMIT)? == b"{\"result\":true}",
            "fixture result mismatch"
        );
        ensure!(
            before == session.images(&runtime, kernel)?,
            "fixture images changed"
        );
        session.finish()
    })();
    // Drop (or finish) has killed the owned group before its only reap.
    assert!(
        !PathBuf::from(format!("/proc/{pid}")).exists(),
        "fixture survived cleanup"
    );
    result
}

#[test]
#[ignore = "requires native Linux static protocol and independent kernel fixtures"]
fn frozen_helper_protocol_controls() {
    let executable =
        fs::canonicalize(std::env::var_os("CODEMAP_JS_TS_LINUX_FIXTURE").unwrap()).unwrap();
    let kernel = kernel();
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../toolchain/linux/js_ts_helper_protocol_expectations_v2.json"
    ))
    .unwrap();
    assert_eq!(oracle["deadline_seconds"], DEADLINE.as_secs());
    assert_eq!(oracle["output_bytes"], OUTPUT_LIMIT);
    assert_eq!(oracle["stderr_bytes"], ERROR_LIMIT);
    for mode in oracle["accept"].as_array().unwrap() {
        exchange(mode.as_str().unwrap(), &executable, &kernel).unwrap();
    }
    for mode in oracle["reject"].as_array().unwrap() {
        let start = Instant::now();
        assert!(
            exchange(mode.as_str().unwrap(), &executable, &kernel).is_err(),
            "{mode}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "unbounded refusal {mode}"
        );
    }
    std::thread::scope(|scope| {
        let workers: Vec<_> = oracle["deadline_reject"]
            .as_array()
            .unwrap()
            .iter()
            .map(|mode| {
                let executable = &executable;
                let kernel = &kernel;
                scope.spawn(move || {
                    let start = Instant::now();
                    let error = exchange(mode.as_str().unwrap(), executable, kernel).unwrap_err();
                    assert!(
                        error.chain().any(|cause| cause
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|error| error.kind() == std::io::ErrorKind::TimedOut))
                            || format!("{error:#}").contains("deadline"),
                        "{mode}: {error:#}"
                    );
                    assert!(
                        start.elapsed() >= DEADLINE
                            && start.elapsed() < DEADLINE + Duration::from_secs(5),
                        "{mode}"
                    );
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });
}
