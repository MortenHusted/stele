//! Independent real-process observations compared with the preimplementation oracle.
use super::*;
use crate::linux_kernel_images::LinuxVsyscallPolicy;
use crate::linux_runtime::{
    AnonymousExecutePolicy, LinuxArchitecture, LinuxRuntimeFile, LinuxRuntimeManifest,
    LinuxRuntimeRole,
};
use crate::schema::sha256_hex;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture {
    directory: tempfile::TempDir,
    executable: PathBuf,
    kernel: LinuxKernelPins,
}

impl Fixture {
    fn prepare() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let executable = fs::canonicalize(
            std::env::var_os("CODEMAP_LINUX_LSP_FIXTURE").expect("operator LSP fixture required"),
        )
        .unwrap();
        let dump = directory.path().join("vdso");
        let operator = std::env::var_os("CODEMAP_LINUX_KERNEL_FIXTURE")
            .expect("separate operator kernel fixture required");
        assert!(
            Command::new(operator)
                .args([std::ffi::OsStr::new("dump"), dump.as_os_str()])
                .status()
                .unwrap()
                .success()
        );
        let bytes = fs::read(dump).unwrap();
        let kernel = LinuxKernelPins {
            architecture: if cfg!(target_arch = "x86_64") {
                LinuxArchitecture::X86_64
            } else {
                LinuxArchitecture::Aarch64
            },
            vdso_size: u64::try_from(bytes.len()).unwrap(),
            vdso_sha256: sha256_hex(&bytes),
            vsyscall: if cfg!(target_arch = "x86_64") {
                LinuxVsyscallPolicy::FixedEmulated
            } else {
                LinuxVsyscallPolicy::Absent
            },
        };
        Self {
            directory,
            executable,
            kernel,
        }
    }

    fn config(&self, name: &str, mode: &str) -> LinuxLspConfig {
        let state = self.directory.path().join(name);
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        LinuxLspConfig {
            launch: LinuxLaunch {
                executable: self.executable.clone(),
                arguments: vec![mode.into(), state.join("ready").into_os_string()],
                environment: BTreeMap::new(),
                directory: state,
            },
            runtime: runtime(&self.executable),
            kernel: self.kernel.clone(),
            reads: vec![],
            directories: vec![],
            limits: LinuxLspLimits {
                request_millis: 1000,
                frame_millis: 5000,
                ..LinuxLspLimits::default()
            },
        }
    }

    fn start(&self, name: &str, mode: &str) -> Transport {
        let transport = Transport::spawn_linux("fixture", self.config(name, mode)).unwrap();
        ready(&self.directory.path().join(name).join("ready"));
        transport
    }
}

fn runtime(executable: &Path) -> CapturedLinuxRuntime {
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
                path: executable.to_path_buf(),
                sha256: sha256_hex(&fs::read(executable).unwrap()),
                role: LinuxRuntimeRole::Clangd,
            },
        )]),
    })
    .unwrap()
}

fn ready(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !fs::read(path).is_ok_and(|bytes| bytes.starts_with(b"R ")) {
        assert!(Instant::now() < deadline, "fixture not ready");
        thread::sleep(Duration::from_millis(1));
    }
}

fn observe(
    observed: &mut BTreeMap<String, String>,
    label: &str,
    result: Result<()>,
    reason: Option<&str>,
) {
    let classification = if result.is_ok() { "accept" } else { "reject" };
    if let Some(reason) = reason {
        let error = result.unwrap_err();
        assert!(format!("{error:#}").contains(reason), "{label}: {error:#}");
    } else {
        result.unwrap();
    }
    assert!(
        observed
            .insert(label.into(), classification.into())
            .is_none()
    );
}

fn reaped(pid: u32) {
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "owned child not reaped"
    );
}

fn accepted(fixture: &Fixture, observed: &mut BTreeMap<String, String>) {
    for (label, mode) in [
        ("valid_response", "echo"),
        ("fragmented_response", "fragmented"),
        ("stderr_drain", "diagnostic"),
    ] {
        let transport = fixture.start(label, mode);
        assert_eq!(
            transport.request("fixture/query", &json!({})).unwrap(),
            json!(true)
        );
        observe(observed, label, Ok(()), None);
    }
    for (label, mode) in [
        ("out_of_order_responses", "reverse"),
        ("buffered_responses", "buffered"),
    ] {
        let transport = fixture.start(label, mode);
        thread::scope(|scope| {
            let first = scope.spawn(|| transport.request("fixture/first", &json!({})));
            let second = scope.spawn(|| transport.request("fixture/second", &json!({})));
            assert_eq!(first.join().unwrap().unwrap(), json!(true));
            assert_eq!(second.join().unwrap().unwrap(), json!(true));
        });
        observe(observed, label, Ok(()), None);
    }
    for (label, mode, method) in [
        ("notification_delivery", "notify", "$/progress"),
        ("server_request_balanced", "server", "fixture/unsupported"),
    ] {
        let transport = fixture.start(label, mode);
        assert_eq!(
            transport.request("fixture/query", &json!({})).unwrap(),
            json!(true)
        );
        assert_eq!(
            transport
                .unsolicited()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .unwrap()["method"],
            json!(method)
        );
        observe(observed, label, Ok(()), None);
    }
    let transport = fixture.start("identity", "echo");
    let pid = transport.process_id().unwrap();
    assert_eq!(transport.process_id().unwrap(), pid);
    observe(observed, "nonreaping_live_identity", Ok(()), None);
    observe(
        observed,
        "image_checkpoint",
        transport.verify_linux_images(),
        None,
    );
    assert_eq!(transport.linux_policy_fingerprint().unwrap().len(), 64);
    transport.shutdown();
    reaped(pid);
    observe(observed, "shutdown_reaps", Ok(()), None);
    let transport = fixture.start("drop", "echo");
    let pid = transport.process_id().unwrap();
    drop(transport);
    reaped(pid);
    observe(observed, "drop_reaps", Ok(()), None);
}

fn idle_and_partial_frames(fixture: &Fixture, observed: &mut BTreeMap<String, String>) {
    let mut config = fixture.config("supported_bounded_frame_policy", "echo");
    config.limits.session_frames = 32_768;
    let transport = Transport::spawn_linux("fixture", config).unwrap();
    assert_eq!(
        transport.request("fixture/query", &json!({})).unwrap(),
        json!(true)
    );
    observe(observed, "supported_bounded_frame_policy", Ok(()), None);
    for label in ["idle_before_request", "idle_between_replies"] {
        let mut config = fixture.config(label, "echo");
        config.limits.frame_millis = 100;
        let transport = Transport::spawn_linux("fixture", config).unwrap();
        ready(&fixture.directory.path().join(label).join("ready"));
        if label == "idle_between_replies" {
            assert_eq!(
                transport.request("fixture/first", &json!({})).unwrap(),
                json!(true)
            );
        }
        thread::sleep(Duration::from_millis(250));
        assert_eq!(
            transport.request("fixture/query", &json!({})).unwrap(),
            json!(true)
        );
        observe(observed, label, Ok(()), None);
    }
    for (label, mode) in [
        ("partial_header_deadline", "partial-header"),
        ("partial_body_deadline", "partial-body"),
    ] {
        let mut config = fixture.config(label, mode);
        config.limits.frame_millis = 100;
        config.limits.request_millis = 1000;
        let transport = Transport::spawn_linux("fixture", config).unwrap();
        let pid = transport.process_id().unwrap();
        ready(&fixture.directory.path().join(label).join("ready"));
        let start = Instant::now();
        let result = transport.request("fixture/query", &json!({}));
        assert!(start.elapsed() < Duration::from_millis(800));
        observe(observed, label, result.map(|_| ()), Some("timed out"));
        drop(transport);
        reaped(pid);
    }
}

#[test]
#[ignore = "requires native ordinary Linux and two operator-compiled static fixtures"]
fn idle_and_partial_frame_controls() {
    idle_and_partial_frames(&Fixture::prepare(), &mut BTreeMap::new());
}

fn rejected_replies(fixture: &Fixture, observed: &mut BTreeMap<String, String>) {
    for (label, mode, reason) in [
        ("unknown_response_id", "unknown", "unknown or duplicate"),
        ("noninteger_response_id", "noninteger", "response id"),
        ("malformed_envelope", "envelope", "envelope"),
        ("missing_result", "missing", "response result or error"),
        ("server_error", "error", "fixture error"),
        ("oversized_header", "header", "header byte budget"),
        ("oversized_body", "body", "body byte budget"),
        (
            "truncated_frame",
            "truncated",
            "failed to fill whole buffer",
        ),
        ("invalid_json", "json", "EOF"),
        ("closed_stdin", "closed-input", "Broken pipe"),
        ("receive_deadline", "receive", "timed out"),
        ("exited_child", "exit", "stdout closed"),
    ] {
        let transport = fixture.start(label, mode);
        let pid = transport.process_id().unwrap();
        let start = Instant::now();
        let result = transport.request("fixture/query", &json!({})).map(|_| ());
        // Reader failure closes the pending channel; preserve its independent reason.
        let result = result
            .map_err(|error| anyhow::anyhow!("{error:#}; {}", transport.process_id().unwrap_err()));
        observe(observed, label, result, Some(reason));
        assert!(start.elapsed() < Duration::from_secs(4));
        drop(transport);
        reaped(pid);
    }
    observe(observed, "failure_reaps", Ok(()), None);
    let config = fixture.config("closed_stdout", "closed-output");
    let outcome = Transport::spawn_linux("fixture", config)
        .and_then(|transport| transport.request("fixture/query", &json!({})))
        .map(|_| ());
    observe(observed, "closed_stdout", outcome, Some("closed"));
    let transport = fixture.start("duplicate", "duplicate");
    let _ = transport.request("fixture/query", &json!({}));
    let deadline = Instant::now() + Duration::from_secs(2);
    while transport.process_id().is_ok() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    observe(
        observed,
        "duplicate_response_id",
        transport.process_id().map(|_| ()),
        Some("unknown or duplicate"),
    );
}

fn budget_controls(fixture: &Fixture, observed: &mut BTreeMap<String, String>) {
    for (label, mode) in [
        ("unsolicited_capacity", "queue"),
        ("session_frame_budget", "frames"),
        ("session_body_budget", "wire"),
        ("stderr_budget", "stderr"),
    ] {
        let mut config = fixture.config(label, mode);
        match label {
            "unsolicited_capacity" => config.limits.unsolicited = 1,
            "session_frame_budget" => config.limits.session_frames = 2,
            "session_body_budget" => config.limits.stream_bytes = 128,
            "stderr_budget" => config.limits.stderr_bytes = 1024,
            _ => unreachable!(),
        }
        let transport = Transport::spawn_linux("fixture", config).unwrap();
        ready(&fixture.directory.path().join(label).join("ready"));
        let result = transport.request("fixture/query", &json!({})).map(|_| ());
        assert!(result.is_err());
        let error = transport.process_id().unwrap_err();
        let reason = match label {
            "unsolicited_capacity" => "queue",
            "session_frame_budget" => "frame budget",
            "session_body_budget" => "stream byte budget",
            _ => "stderr byte budget",
        };
        observe(observed, label, Err(error), Some(reason));
    }
    let mut config = fixture.config("pending_capacity", "receive");
    config.limits.pending = 1;
    let transport = Transport::spawn_linux("fixture", config).unwrap();
    ready(&fixture.directory.path().join("pending_capacity/ready"));
    thread::scope(|scope| {
        let waiting = scope.spawn(|| transport.request("fixture/first", &json!({})));
        let deadline = Instant::now() + Duration::from_secs(1);
        while transport.pending.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        observe(
            observed,
            "pending_capacity",
            transport.request("fixture/second", &json!({})).map(|_| ()),
            Some("pending request budget"),
        );
        assert!(waiting.join().unwrap().is_err());
    });
}

fn deadline_controls(fixture: &Fixture, observed: &mut BTreeMap<String, String>) {
    let transport = fixture.start("expired", "echo");
    let result = transport
        .finish(transport.request_until("fixture/query", &json!({}), Some(Instant::now())))
        .map(|_| ());
    observe(
        observed,
        "expired_request_deadline",
        result,
        Some("deadline"),
    );
    let transport = fixture.start("writer_lock", "echo");
    let ProtocolInput::Linux(writer) = &transport.stdin else {
        unreachable!()
    };
    let lock = writer.lock().unwrap();
    thread::scope(|scope| {
        let waiting = scope.spawn(|| transport.request("fixture/query", &json!({})));
        observe(
            observed,
            "writer_lock_deadline",
            waiting.join().unwrap().map(|_| ()),
            Some("mutex deadline"),
        );
    });
    drop(lock);
    let transport = fixture.start("write_backpressure", "silent");
    let pid = transport.process_id().unwrap();
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(format!("/proc/{pid}/fd/0"))
        .unwrap();
    let start = Instant::now();
    let result = transport
        .request("fixture/query", &json!("x".repeat(256 * 1024)))
        .map(|_| ());
    observe(
        observed,
        "write_backpressure_deadline",
        result,
        Some("timed out"),
    );
    assert!(start.elapsed() < Duration::from_secs(4));
    let mut bytes = [0; 4096];
    let mut count = 0;
    loop {
        match input.read(&mut bytes) {
            Ok(0) => break,
            Ok(size) => count += size,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            result => panic!("unexpected backpressure witness: {result:?}"),
        }
    }
    assert!(count > 4096, "timeout preceded bulk pipe writes");
}

fn contention_and_outbound(fixture: &Fixture, observed: &mut BTreeMap<String, String>) {
    let transport = fixture.start("first_failure", "echo");
    let ChildOwner::Linux(control) = &transport.child else {
        unreachable!()
    };
    control.fail("operator first failure");
    let result: Result<()> = transport.finish(Err(std::io::Error::from(
        std::io::ErrorKind::ConnectionAborted,
    )
    .into()));
    assert!(format!("{:#}", result.as_ref().unwrap_err()).contains("connection aborted"));
    observe(
        observed,
        "retained_first_failure",
        result,
        Some("operator first failure"),
    );
    let transport = fixture.start("owner_lock", "echo");
    let ChildOwner::Linux(control) = &transport.child else {
        unreachable!()
    };
    let mut lease = control
        .lease(Instant::now() + Duration::from_secs(2))
        .unwrap();
    let pid = lease.owner().as_ref().unwrap().id();
    let start = Instant::now();
    thread::scope(|scope| {
        let request = scope.spawn(|| transport.request("fixture/query", &json!({})));
        observe(
            observed,
            "owner_lock_deadline",
            request.join().unwrap().map(|_| ()),
            Some("mutex deadline"),
        );
    });
    assert!(start.elapsed() < Duration::from_millis(1500));
    // Failure returns while the selected owned child remains with this lease.
    lease.owner().as_ref().unwrap().verify_live().unwrap();
    drop(lease);
    reaped(pid);
    observe(observed, "deferred_owner_cleanup", Ok(()), None);
    for (label, frame_limit) in [
        ("outbound_frame_budget", true),
        ("outbound_byte_budget", false),
    ] {
        let mut config = fixture.config(label, "silent");
        if frame_limit {
            config.limits.session_frames = 1;
        } else {
            config.limits.stream_bytes = 96;
        }
        let transport = Transport::spawn_linux("fixture", config).unwrap();
        ready(&fixture.directory.path().join(label).join("ready"));
        transport.notify("exit", &json!({})).unwrap();
        let reason = if frame_limit {
            "outbound frame budget"
        } else {
            "outbound stream byte budget"
        };
        observe(
            observed,
            label,
            transport.notify("exit", &json!({})),
            Some(reason),
        );
    }
}

fn pin_controls(fixture: &Fixture, observed: &mut BTreeMap<String, String>) {
    for (label, larger) in [("zero_policy", false), ("enlarged_policy", true)] {
        let mut config = fixture.config(label, "echo");
        config.limits.pending = if larger { 65 } else { 0 };
        observe(
            observed,
            label,
            Transport::spawn_linux("fixture", config).map(|_| ()),
            Some("invalid Linux LSP limits"),
        );
    }
    let mut config = fixture.config("wrong_kernel", "echo");
    config.kernel.vdso_sha256 = "0".repeat(64);
    let transport = Transport::spawn_linux("fixture", config).unwrap();
    ready(&fixture.directory.path().join("wrong_kernel/ready"));
    observe(
        observed,
        "wrong_kernel_pins",
        transport.verify_linux_images(),
        Some("operator byte pins"),
    );
    let executable = fixture.directory.path().join("drift-fixture");
    fs::copy(&fixture.executable, &executable).unwrap();
    let mut config = fixture.config("runtime_drift", "echo");
    config.runtime = runtime(&executable);
    config.launch.executable = executable.clone();
    let file = OpenOptions::new().write(true).open(executable).unwrap();
    file.write_all_at(b"X", 100).unwrap();
    drop(file);
    observe(
        observed,
        "runtime_drift",
        Transport::spawn_linux("fixture", config).map(|_| ()),
        Some("runtime"),
    );
    let transport = fixture.start("image_tamper", "echo");
    transport.verify_linux_images().unwrap();
    let pid = transport.process_id().unwrap();
    let maps = fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
    let vdso = maps.lines().find(|line| line.ends_with("[vdso]")).unwrap();
    let address = u64::from_str_radix(vdso.split('-').next().unwrap(), 16).unwrap();
    let disk = sha256_hex(&fs::read(&fixture.executable).unwrap());
    let memory = OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("/proc/{pid}/mem"))
        .unwrap();
    let mut byte = [0];
    memory.read_exact_at(&mut byte, address + 64).unwrap();
    byte[0] ^= 1;
    memory.write_all_at(&byte, address + 64).unwrap();
    assert_eq!(disk, sha256_hex(&fs::read(&fixture.executable).unwrap()));
    observe(
        observed,
        "image_tamper",
        transport.verify_linux_images(),
        Some("operator byte pins"),
    );
    drop(transport);
    reaped(pid);
}

fn cancellation_controls(fixture: &Fixture, observed: &mut BTreeMap<String, String>) {
    let config = fixture.config("cancelled", "silent");
    let guard = LinuxGuard::prepare(&config.runtime, &[], &[], &config.launch.directory).unwrap();
    let mut child = OwnedLinuxChild::spawn_guarded(config.launch, guard).unwrap();
    ready(&fixture.directory.path().join("cancelled/ready"));
    let mut pipes = LinuxPipes::new(child.take_stdio().unwrap()).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    pipes.cancel_on(Arc::clone(&cancelled));
    thread::scope(|scope| {
        let read = scope.spawn(|| {
            pipes.stdout.read_frame(
                LinuxLspLimits::default().frames(),
                Instant::now() + Duration::from_secs(2),
            )
        });
        thread::sleep(Duration::from_millis(30));
        cancelled.store(true, Ordering::Release);
        observe(
            observed,
            "cancelled_read",
            read.join().unwrap().map(|_| ()),
            Some("connection aborted"),
        );
    });
    observe(
        observed,
        "cancelled_write",
        pipes.stdin.write_frame(
            &json!(null),
            LinuxLspLimits::default().frames(),
            Instant::now() + Duration::from_secs(2),
        ),
        Some("connection aborted"),
    );
    child.terminate().unwrap();
}

#[test]
#[ignore = "requires native ordinary Linux and two operator-compiled static fixtures"]
fn frozen_owned_linux_lsp_controls() {
    let fixture = Fixture::prepare();
    let mut observed = BTreeMap::new();
    accepted(&fixture, &mut observed);
    idle_and_partial_frames(&fixture, &mut observed);
    rejected_replies(&fixture, &mut observed);
    budget_controls(&fixture, &mut observed);
    deadline_controls(&fixture, &mut observed);
    contention_and_outbound(&fixture, &mut observed);
    pin_controls(&fixture, &mut observed);
    cancellation_controls(&fixture, &mut observed);
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../toolchain/linux/lsp_transport_expectations.json"
    ))
    .unwrap();
    let expected: BTreeMap<String, String> =
        serde_json::from_value(oracle["expected"].clone()).unwrap();
    assert_eq!(observed, expected);
}
