#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use codemap::linux_guard::LinuxGuard;
use codemap::linux_io::{FrameLimits, LinuxPipes, MAX_BODY_BYTES};
use codemap::linux_process::{LinuxLaunch, OwnedLinuxChild};
use codemap::linux_runtime::{
    AnonymousExecutePolicy, CapturedLinuxRuntime, LinuxArchitecture, LinuxRuntimeFile,
    LinuxRuntimeManifest, LinuxRuntimeRole,
};
use codemap::schema::sha256_hex;
use serde_json::json;

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
) -> (OwnedLinuxChild, LinuxPipes) {
    fs::create_dir(directory).unwrap();
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    let ready = directory.join("ready");
    let launch = LinuxLaunch {
        executable: fixture.to_path_buf(),
        arguments: vec![mode.into(), ready.as_os_str().to_owned()],
        environment: BTreeMap::new(),
        directory: directory.to_path_buf(),
    };
    let guard = LinuxGuard::prepare(runtime, &[], &[], directory).unwrap();
    let mut child = OwnedLinuxChild::spawn_guarded(launch, guard).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read(&ready).ok().as_deref() != Some(b"R") {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let pipes = LinuxPipes::new(child.take_stdio().unwrap()).unwrap();
    (child, pipes)
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

const fn limits() -> FrameLimits {
    FrameLimits {
        header_bytes: 128,
        body_bytes: 1024,
    }
}

#[test]
#[ignore = "requires ordinary native Linux and an operator-compiled static protocol fixture"]
fn frozen_real_bounded_protocol_controls() {
    let fixture: PathBuf = fs::canonicalize(
        std::env::var_os("CODEMAP_LINUX_PROTOCOL_FIXTURE").expect("operator fixture required"),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let directory = fs::canonicalize(root.path()).unwrap();
    let runtime = runtime(&fixture);
    let mut observed = BTreeMap::new();
    read_controls(&fixture, &runtime, &directory, &mut observed);
    rejected_frames(&fixture, &runtime, &directory, &mut observed);
    deadline_controls(&fixture, &runtime, &directory, &mut observed);
    write_controls(&fixture, &runtime, &directory, &mut observed);
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../toolchain/linux/protocol_expectations.json"
    ))
    .unwrap();
    let expected: BTreeMap<_, _> = oracle["expected"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(label, _)| label.as_str() != "legacy_framing_unchanged")
        .map(|(label, value)| (label.as_str(), value.as_str().unwrap()))
        .collect();
    assert_eq!(observed, expected);
}

fn read_controls(
    fixture: &Path,
    runtime: &CapturedLinuxRuntime,
    directory: &Path,
    observed: &mut BTreeMap<&'static str, &'static str>,
) {
    for (label, mode) in [
        ("valid_frame", "valid"),
        ("buffered_multiple_frames", "multiple"),
        ("fragmented_frame", "fragmented"),
    ] {
        let (child, mut pipes) = spawn(fixture, runtime, &directory.join(label), mode);
        let value = pipes
            .stdout
            .read_frame(limits(), Instant::now() + Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(value, json!({"id":1,"result":true}));
        if mode == "multiple" {
            assert_eq!(
                pipes
                    .stdout
                    .read_frame(limits(), Instant::now() + Duration::from_secs(2))
                    .unwrap(),
                Some(value)
            );
        }
        record(observed, label, Ok(()), None);
        if mode == "valid" {
            let mut bytes = [0; 10];
            let count = pipes
                .stderr
                .read_some(&mut bytes, Instant::now() + Duration::from_secs(2))
                .unwrap();
            assert_eq!(count, 10);
            assert_eq!(&bytes, b"diagnostic");
            observed.insert("stderr_read", "accept");
        }
        child.terminate().unwrap();
    }
}

fn rejected_frames(
    fixture: &Path,
    runtime: &CapturedLinuxRuntime,
    directory: &Path,
    observed: &mut BTreeMap<&'static str, &'static str>,
) {
    for (label, mode, reason) in [
        ("header_budget", "header", "header byte budget"),
        ("body_budget", "body", "body byte budget"),
        ("duplicate_length", "duplicate", "duplicate LSP"),
        ("invalid_length", "length", "invalid LSP"),
        ("missing_length", "missing", "missing Content-Length"),
        ("truncated_header", "partial-header", "truncated LSP header"),
        (
            "truncated_body",
            "partial-body",
            "failed to fill whole buffer",
        ),
        ("invalid_json", "json", "expected value"),
        (
            "non_utf8_header",
            "utf8",
            "stream did not contain valid UTF-8",
        ),
        (
            "closed_output_partial_frame",
            "partial-body",
            "failed to fill whole buffer",
        ),
    ] {
        let (child, mut pipes) = spawn(fixture, runtime, &directory.join(label), mode);
        if mode.starts_with("partial-") {
            pipes
                .stdin
                .write_frame(
                    &json!(null),
                    limits(),
                    Instant::now() + Duration::from_secs(2),
                )
                .unwrap();
        }
        let result = pipes
            .stdout
            .read_frame(limits(), Instant::now() + Duration::from_secs(2))
            .map(|_| ());
        record(observed, label, result, Some(reason));
        child.terminate().unwrap();
    }
}

fn deadline_controls(
    fixture: &Path,
    runtime: &CapturedLinuxRuntime,
    directory: &Path,
    observed: &mut BTreeMap<&'static str, &'static str>,
) {
    for (label, mode, duration) in [
        ("expired_read_deadline", "valid", Duration::ZERO),
        (
            "stalled_header",
            "stalled-header",
            Duration::from_millis(50),
        ),
        ("stalled_body", "stalled-body", Duration::from_millis(50)),
    ] {
        let (child, mut pipes) = spawn(fixture, runtime, &directory.join(label), mode);
        let start = Instant::now();
        let result = pipes
            .stdout
            .read_frame(limits(), start + duration)
            .map(|_| ());
        record(observed, label, result, Some("timed out"));
        assert!(start.elapsed() < Duration::from_secs(1));
        child.terminate().unwrap();
    }
    let (child, mut pipes) = spawn(
        fixture,
        runtime,
        &directory.join("limit-validation"),
        "valid",
    );
    for (label, limit) in [
        (
            "zero_limits",
            FrameLimits {
                header_bytes: 0,
                body_bytes: 1024,
            },
        ),
        (
            "limits_above_hard_cap",
            FrameLimits {
                header_bytes: 128,
                body_bytes: MAX_BODY_BYTES + 1,
            },
        ),
    ] {
        record(
            observed,
            label,
            pipes
                .stdout
                .read_frame(limit, Instant::now() + Duration::from_secs(2))
                .map(|_| ()),
            Some("invalid Linux protocol limits"),
        );
    }
    child.terminate().unwrap();
}

fn write_controls(
    fixture: &Path,
    runtime: &CapturedLinuxRuntime,
    directory: &Path,
    observed: &mut BTreeMap<&'static str, &'static str>,
) {
    let (child, mut pipes) = spawn(fixture, runtime, &directory.join("write"), "write");
    let value = json!({"result":"query"});
    pipes
        .stdin
        .write_frame(&value, limits(), Instant::now() + Duration::from_secs(2))
        .unwrap();
    assert_eq!(
        pipes
            .stdout
            .read_frame(limits(), Instant::now() + Duration::from_secs(2))
            .unwrap(),
        Some(value)
    );
    observed.insert("bounded_write", "accept");
    child.terminate().unwrap();
    for (label, mode, value, duration, limit, reason) in [
        (
            "expired_write_deadline",
            "silent",
            json!(null),
            Duration::ZERO,
            limits(),
            "timed out",
        ),
        (
            "write_backpressure_deadline",
            "silent",
            json!("x".repeat(256 * 1024)),
            Duration::from_millis(500),
            FrameLimits {
                header_bytes: 128,
                body_bytes: MAX_BODY_BYTES,
            },
            "timed out",
        ),
        (
            "closed_input_pipe",
            "closed-input",
            json!(null),
            Duration::from_secs(2),
            limits(),
            "Broken pipe",
        ),
        (
            "oversized_outbound_frame",
            "silent",
            json!("x".repeat(2048)),
            Duration::from_secs(2),
            limits(),
            "outbound body byte budget",
        ),
    ] {
        let (child, mut pipes) = spawn(fixture, runtime, &directory.join(label), mode);
        let start = Instant::now();
        record(
            observed,
            label,
            pipes.stdin.write_frame(&value, limit, start + duration),
            Some(reason),
        );
        if label == "write_backpressure_deadline" {
            child.verify_live().unwrap();
            let mut input = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(format!("/proc/{}/fd/0", child.id()))
                .unwrap();
            let mut bytes = [0; 4096];
            let mut count = 0;
            loop {
                match input.read(&mut bytes) {
                    Ok(size) if size > 0 => count += size,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    result => panic!("unexpected non-reading pipe witness: {result:?}"),
                }
            }
            assert!(count > 4096, "timeout did not reach bulk pipe writes");
        }
        assert!(start.elapsed() < Duration::from_secs(1));
        child.terminate().unwrap();
    }
}
