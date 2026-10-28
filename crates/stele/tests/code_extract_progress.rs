#![cfg(unix)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn progress_preserves_canonical_stdout_and_reports_completion() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/lib.rs"), "pub fn example() {}\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec![
            "remote",
            "add",
            "origin",
            "https://example.invalid/progress.git",
        ],
        vec!["add", "."],
        vec!["commit", "-qm", "fixture"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(root.path())
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let logs = tempfile::tempdir().unwrap();
    let stderr_path = logs.path().join("progress.log");
    let invoke = |progress| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_stele"));
        command.args(["code", "extract", "--path"]).arg(root.path());
        if progress {
            command.arg("--progress");
            command.stderr(Stdio::from(std::fs::File::create(&stderr_path).unwrap()));
        }
        command.output().unwrap()
    };
    assert_blocked_terminal_finishes(root.path());
    let plain = invoke(false);
    let progress = invoke(true);
    assert!(plain.status.success(), "{plain:?}");
    assert!(progress.status.success(), "{progress:?}");
    assert_eq!(plain.stdout, progress.stdout);
    let piped = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(["code", "extract", "--progress", "--path"])
        .arg(root.path())
        .output()
        .unwrap();
    assert!(piped.status.success());
    assert_eq!(plain.stdout, piped.stdout);
    assert!(
        !String::from_utf8(piped.stderr)
            .unwrap()
            .contains("stele code: extraction")
    );
    let stderr = std::fs::read_to_string(&stderr_path).unwrap();
    assert!(stderr.contains("stele code: extraction started"));
    assert!(stderr.contains("stele code: extraction completed"));
    assert!(
        !String::from_utf8(plain.stderr)
            .unwrap()
            .contains("stele code: extraction")
    );
}

#[test]
fn failed_extraction_stops_progress_promptly_without_reporting_completion() {
    let root = tempfile::tempdir().unwrap();
    let stderr_path = root.path().join("failure.log");
    let stderr = std::fs::File::create(&stderr_path).unwrap();
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(["code", "extract", "--progress", "--path"])
        .arg(root.path().join("missing"))
        .stderr(Stdio::from(stderr))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(output.stdout, [] as [u8; 0]);
    let stderr = std::fs::read_to_string(stderr_path).unwrap();
    assert!(stderr.contains("stele code: extraction started"));
    assert!(!stderr.contains("stele code: extraction completed"));
}

fn assert_blocked_terminal_finishes(root: &std::path::Path) {
    use std::os::fd::{AsRawFd, FromRawFd};
    let (mut master, mut slave) = (-1, -1);
    // openpty initializes two owned descriptors; no optional buffers are supplied.
    assert_eq!(
        unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    // The successful openpty returned these owned descriptors.
    let _master = unsafe { std::fs::File::from_raw_fd(master) };
    // The successful openpty returned this owned descriptor.
    let slave = unsafe { std::fs::File::from_raw_fd(slave) };
    // These fcntl calls configure only the owned test PTY.
    let flags = unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let buffer = [b'x'; 4096];
    loop {
        // The buffer remains valid for this write and the PTY is nonblocking.
        let count = unsafe { libc::write(slave.as_raw_fd(), buffer.as_ptr().cast(), buffer.len()) };
        if count < 0 {
            assert_eq!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::WouldBlock
            );
            break;
        }
    }
    // Restore blocking mode to prove progress does not depend on inherited flags.
    assert_eq!(
        unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_SETFL, flags) },
        0
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(["code", "extract", "--progress", "--path"])
        .arg(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(5) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("progress blocked on undrained terminal");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(child.wait_with_output().unwrap().status.success());
}
