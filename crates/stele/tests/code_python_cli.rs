//! Python configuration refusal must complete before extraction or output creation.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn refuse_configuration(command: &str, selector: &str, path: &Path, expected: &str) {
    let out = tempfile::tempdir().unwrap();
    let output_path = out.path().join("must-not-create");
    let mut invocation = Command::new(env!("CARGO_BIN_EXE_stele"));
    invocation
        .args(["code", command, "--language", "python", selector])
        .arg(path);
    if selector == "--python-native" {
        invocation.args(["--calls", "required"]);
    }
    let mut child = invocation
        .arg("--out")
        .arg(&output_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() >= Duration::from_secs(2) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Python configuration refusal blocked");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains(expected));
    assert!(!output_path.exists());
}

#[test]
fn python_cli_refuses_fifo_links_oversized_and_malformed_configuration() {
    let root = tempfile::tempdir().unwrap();
    let fifo = root.path().join("fifo.json");
    let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // The fixture creates only this owned path, with permissions limited to the test user.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let large = root.path().join("large.json");
    std::fs::File::create(&large)
        .unwrap()
        .set_len(32 * 1024 * 1024 + 1)
        .unwrap();
    let malformed = root.path().join("malformed.json");
    std::fs::write(&malformed, b"{}").unwrap();
    let linked = root.path().join("linked.json");
    std::os::unix::fs::symlink(&malformed, &linked).unwrap();
    for (command, selector, kind) in [
        ("extract", "--python-parser", "parser"),
        ("history", "--python-parser", "parser"),
        ("extract", "--python-native", "native"),
    ] {
        for path in [&fifo, &large] {
            refuse_configuration(command, selector, path, "bounded regular file");
        }
        refuse_configuration(
            command,
            selector,
            &malformed,
            &format!("parse Python {kind} configuration"),
        );
        refuse_configuration(
            command,
            selector,
            &linked,
            &format!("open Python {kind} configuration"),
        );
    }
}
