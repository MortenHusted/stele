#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use codemap::linux_guard::LinuxGuard;
use codemap::linux_process::{LinuxLaunch, OwnedLinuxChild, StoppedLinuxChild};
use codemap::linux_runtime::{
    AnonymousExecutePolicy, CapturedLinuxRuntime, LinuxArchitecture, LinuxRuntimeFile,
    LinuxRuntimeManifest, LinuxRuntimeRole,
};
use codemap::schema::sha256_hex;

fn runtime(fixture: &Path) -> CapturedLinuxRuntime {
    CapturedLinuxRuntime::capture(&LinuxRuntimeManifest {
        architecture: if cfg!(target_arch = "x86_64") {
            LinuxArchitecture::X86_64
        } else {
            LinuxArchitecture::Aarch64
        },
        anonymous_execute: AnonymousExecutePolicy::Deny,
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
    mode: &str,
    guarded: bool,
) -> OwnedLinuxChild {
    fs::create_dir(directory).unwrap();
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    let ready = directory.join("ready");
    let launch = LinuxLaunch {
        executable: fixture.to_path_buf(),
        arguments: vec![mode.into(), ready.as_os_str().to_owned()],
        environment: BTreeMap::new(),
        directory: directory.to_path_buf(),
    };
    let child = if guarded {
        OwnedLinuxChild::spawn_guarded(
            launch,
            LinuxGuard::prepare(runtime, &[], &[], directory).unwrap(),
        )
        .unwrap()
    } else {
        OwnedLinuxChild::spawn(Command::new(fixture).args([mode.as_ref(), ready.as_os_str()]))
            .unwrap()
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read(&ready).ok().as_deref() != Some(b"R") {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    child
}

fn reaped(id: u32) {
    let mut status = 0;
    // SAFETY: wait applies only to a child of this parent, never signals or adopts another task.
    assert_eq!(
        unsafe { libc::waitpid(i32::try_from(id).unwrap(), &raw mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
#[ignore = "requires ordinary native Linux and an operator-compiled static process I/O fixture"]
fn frozen_owned_stdio_and_cleanup_controls() {
    let fixture: PathBuf = fs::canonicalize(
        std::env::var_os("CODEMAP_LINUX_IO_FIXTURE").expect("operator fixture required"),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let directory = fs::canonicalize(root.path()).unwrap();
    let runtime = runtime(&fixture);
    let mut observed = BTreeMap::new();
    let mut child = spawn(&fixture, &runtime, &directory.join("echo"), "echo", true);
    child.verify_live().unwrap();
    observed.insert("live_nonreaping_probe", "accept");
    let mut io = child.take_stdio().unwrap();
    io.stdin.write_all(b"Q").unwrap();
    let mut answer = [0; 6];
    io.stdout.read_exact(&mut answer).unwrap();
    assert_eq!(&answer, b"answer");
    let mut error = [0; 5];
    io.stderr.read_exact(&mut error).unwrap();
    assert_eq!(&error, b"error");
    observed.insert("guarded_stdio_handoff", "accept");
    assert!(
        child
            .take_stdio()
            .err()
            .unwrap()
            .to_string()
            .contains("already taken")
    );
    observed.insert("repeated_handoff", "reject");
    let id = child.id();
    let start = Instant::now();
    child.terminate().unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    reaped(id);
    observed.insert("explicit_cleanup_reaps", "accept");
    exited_controls(&fixture, &runtime, &directory, &mut observed);
    blocked_and_stopped_controls(&fixture, &runtime, &directory, &mut observed);
    let mut unguarded = spawn(
        &fixture,
        &runtime,
        &directory.join("unguarded"),
        "pause",
        false,
    );
    assert!(
        unguarded
            .take_stdio()
            .err()
            .unwrap()
            .to_string()
            .contains("requires a guarded child")
    );
    observed.insert("unguarded_handoff", "reject");
    unguarded.terminate().unwrap();
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../toolchain/linux/process_io_expectations.json"
    ))
    .unwrap();
    let expected: BTreeMap<_, _> = oracle["expected"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(label, _)| label.as_str() != "expired_wait_budget_background_reaper")
        .map(|(label, value)| (label.as_str(), value.as_str().unwrap()))
        .collect();
    assert_eq!(observed, expected);
}

fn exited_controls(
    fixture: &Path,
    runtime: &CapturedLinuxRuntime,
    directory: &Path,
    observed: &mut BTreeMap<&'static str, &'static str>,
) {
    let mut child = spawn(fixture, runtime, &directory.join("exit"), "exit", true);
    let mut io = child.take_stdio().unwrap();
    io.stdin.write_all(b"Q").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.verify_live().is_ok() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    observed.insert("exited_probe", "reject");
    let id = child.id();
    let report = fs::read_to_string(format!("/proc/{id}/stat")).unwrap();
    assert_eq!(
        report
            .rsplit_once(") ")
            .unwrap()
            .1
            .split_whitespace()
            .next(),
        Some("Z")
    );
    observed.insert("exited_probe_retains_unreaped_child", "accept");
    assert!(child.terminate().unwrap().success());
    reaped(id);
}

fn blocked_and_stopped_controls(
    fixture: &Path,
    runtime: &CapturedLinuxRuntime,
    directory: &Path,
    observed: &mut BTreeMap<&'static str, &'static str>,
) {
    let child = spawn(fixture, runtime, &directory.join("flood"), "flood", true);
    let id = child.id();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fs::read_to_string(format!("/proc/{id}/wchan"))
        .unwrap()
        .contains("pipe_write")
    {
        assert!(
            Instant::now() < deadline,
            "fixture never blocked on full pipe"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let start = Instant::now();
    drop(child);
    assert!(start.elapsed() < Duration::from_secs(1));
    reaped(id);
    observed.insert("blocked_pipe_cleanup", "accept");
    observed.insert("dropped_owner_cleanup_reaps", "accept");
    let mut child = spawn(fixture, runtime, &directory.join("stopped"), "pause", true);
    let stopped = StoppedLinuxChild::capture(&mut child).unwrap();
    // Drop resumes the stopped child; stop it again explicitly to test kill/reap while stopped.
    drop(stopped);
    let id = child.id();
    // SAFETY: selected sealed owner retains the unreaped child.
    assert_eq!(
        unsafe { libc::kill(i32::try_from(id).unwrap(), libc::SIGSTOP) },
        0
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let report = fs::read_to_string(format!("/proc/{id}/stat")).unwrap();
        if report.rsplit_once(") ").unwrap().1.starts_with("T ") {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let start = Instant::now();
    child.terminate().unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    reaped(id);
    observed.insert("stopped_child_cleanup", "accept");
}
