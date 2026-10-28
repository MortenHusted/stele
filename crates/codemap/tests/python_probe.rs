//! U1 parser compatibility only; no Python snapshots or canonical calls.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use codemap::schema::sha256_hex;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use codemap::python_profile::NativeProfile;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use codemap::python_profile::QUERY_HELPERS as NATIVE_QUERY_HELPERS;

fn interpreter_digest(path: &Path) -> Result<String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    let budget = 128 * 1024 * 1024;
    ensure!(
        before.is_file() && before.len() <= budget,
        "bounded regular interpreter required"
    );
    let mut bytes = Vec::new();
    file.take(budget + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= budget,
        "interpreter grew beyond budget"
    );
    let after = fs::symlink_metadata(path)?;
    ensure!(
        after.is_file()
            && before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec(),
        "interpreter changed while hashing"
    );
    Ok(sha256_hex(&bytes))
}

struct ProbeProcess(Option<std::process::Child>);

impl ProbeProcess {
    fn exited(&self) -> bool {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.0.as_ref().unwrap().id(),
                &raw mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        assert_eq!(
            result,
            0,
            "exit observation: {}",
            std::io::Error::last_os_error()
        );
        unsafe { info.si_pid() != 0 }
    }

    fn terminate_group(&self) {
        let group = libc::pid_t::try_from(self.0.as_ref().unwrap().id()).expect("POSIX process ID");
        unsafe { libc::kill(-group, libc::SIGKILL) };
    }

    fn finish(mut self) -> std::process::ExitStatus {
        self.terminate_group();
        self.0.take().unwrap().wait().unwrap()
    }
}

impl Drop for ProbeProcess {
    fn drop(&mut self) {
        if self.0.is_some() {
            self.terminate_group();
            let _ = self.0.take().unwrap().wait();
        }
    }
}

fn output_within_budget(output: &Path, error: &Path) -> bool {
    fs::metadata(output).unwrap().len() <= 1024 * 1024
        && fs::metadata(error).unwrap().len() <= 1024 * 1024
}

#[test]
fn interpreter_pin_rejects_nonregular_and_oversized_files() {
    let scratch = tempfile::tempdir().unwrap();
    let fifo = scratch.path().join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(interpreter_digest(&fifo).is_err());
    let large = scratch.path().join("large");
    fs::File::create(&large)
        .unwrap()
        .set_len(128 * 1024 * 1024 + 1)
        .unwrap();
    assert!(interpreter_digest(&large).is_err());
}

#[test]
fn dropping_runner_terminates_its_helper_group() {
    let scratch = tempfile::tempdir().unwrap();
    let heartbeat = scratch.path().join("heartbeat");
    let runner = ProbeProcess(Some(
        Command::new("/bin/sh")
            .env_clear()
            .process_group(0)
            .args([
                "-c",
                "(while :; do echo alive >> \"$1\"; sleep 0.1; done) & wait",
                "probe",
            ])
            .arg(&heartbeat)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    ));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !heartbeat.exists() {
        assert!(Instant::now() < deadline, "helper did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(runner);
    std::thread::sleep(Duration::from_millis(100));
    let stopped = fs::read(&heartbeat).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        fs::read(&heartbeat).unwrap(),
        stopped,
        "helper survived runner cleanup"
    );
}

#[test]
fn exit_observation_retains_group_leader_until_cleanup() {
    let runner = ProbeProcess(Some(
        Command::new("/bin/sh")
            .env_clear()
            .process_group(0)
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap(),
    ));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !runner.exited() {
        assert!(Instant::now() < deadline, "runner did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = libc::pid_t::try_from(runner.0.as_ref().unwrap().id()).unwrap();
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        0,
        "leader was reaped before cleanup"
    );
    assert!(runner.finish().success());
}

#[test]
#[ignore = "requires an explicitly pinned operator CPython 3.14.7"]
fn isolated_parser_matches_source_and_scope_expectations() {
    let interpreter = fs::canonicalize(
        std::env::var_os("CODEMAP_PYTHON_PROBE_INTERPRETER")
            .expect("explicit operator interpreter"),
    )
    .unwrap();
    let expected = std::env::var("CODEMAP_PYTHON_PROBE_INTERPRETER_SHA256")
        .expect("explicit operator interpreter content pin");
    assert_eq!(interpreter_digest(&interpreter).unwrap(), expected);
    let scratch = tempfile::tempdir().unwrap();
    let helper = scratch.path().join("probe.py");
    let tests = scratch.path().join("test_probe.py");
    fs::write(
        &helper,
        include_bytes!("../../../toolchain/python/probe.py"),
    )
    .unwrap();
    fs::write(
        &tests,
        include_bytes!("../../../toolchain/python/test_probe.py"),
    )
    .unwrap();
    run_isolated_tests(&interpreter, &tests, &["-v"], scratch.path(), 120);
    assert_eq!(interpreter_digest(&interpreter).unwrap(), expected);
    assert_eq!(
        fs::read(&helper).unwrap(),
        include_bytes!("../../../toolchain/python/probe.py")
    );
    assert_eq!(
        fs::read(&tests).unwrap(),
        include_bytes!("../../../toolchain/python/test_probe.py")
    );
}

#[test]
#[ignore = "requires an explicitly pinned operator CPython 3.14.7"]
fn isolated_captured_filesystem_matches_owned_expectations() {
    let interpreter = fs::canonicalize(
        std::env::var_os("CODEMAP_PYTHON_PROBE_INTERPRETER")
            .expect("explicit operator interpreter"),
    )
    .unwrap();
    let expected = std::env::var("CODEMAP_PYTHON_PROBE_INTERPRETER_SHA256")
        .expect("explicit operator interpreter content pin");
    assert_eq!(interpreter_digest(&interpreter).unwrap(), expected);
    let scratch = tempfile::tempdir().unwrap();
    let files: &[(&str, &[u8])] = &[
        (
            "captured_fs.py",
            include_bytes!("../../../toolchain/python/captured_fs.py"),
        ),
        (
            "test_captured_fs.py",
            include_bytes!("../../../toolchain/python/test_captured_fs.py"),
        ),
        (
            "compiler_read_scope.py",
            include_bytes!("../../../toolchain/python/compiler_read_scope.py"),
        ),
        (
            "test_compiler_read_scope.py",
            include_bytes!("../../../toolchain/python/test_compiler_read_scope.py"),
        ),
        (
            "package_profile.py",
            include_bytes!("../../../toolchain/python/package_profile.py"),
        ),
        (
            "test_package_profile.py",
            include_bytes!("../../../toolchain/python/test_package_profile.py"),
        ),
    ];
    for (name, content) in files {
        fs::write(scratch.path().join(name), content).unwrap();
    }
    run_isolated_tests(
        &interpreter,
        &scratch.path().join("test_captured_fs.py"),
        &["-v"],
        scratch.path(),
        30,
    );
    run_isolated_tests(
        &interpreter,
        &scratch.path().join("test_compiler_read_scope.py"),
        &["-v"],
        scratch.path(),
        30,
    );
    run_isolated_tests(
        &interpreter,
        &scratch.path().join("test_package_profile.py"),
        &["-v"],
        scratch.path(),
        30,
    );
    assert_eq!(interpreter_digest(&interpreter).unwrap(), expected);
    for (name, content) in files {
        assert_eq!(fs::read(scratch.path().join(name)).unwrap(), *content);
    }
}

fn run_isolated_tests(
    interpreter: &Path,
    tests: &Path,
    arguments: &[&str],
    scratch: &Path,
    timeout: u64,
) {
    let output = scratch.join("stdout");
    let error = scratch.join("stderr");
    let child = ProbeProcess(Some(
        Command::new(interpreter)
            .process_group(0)
            .env_clear()
            .args(["-I", "-S", "-B"])
            .arg(tests)
            .args(arguments)
            .current_dir(scratch)
            .stdin(std::process::Stdio::null())
            .stdout(fs::File::create(&output).unwrap())
            .stderr(fs::File::create(&error).unwrap())
            .spawn()
            .unwrap(),
    ));
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        assert!(
            output_within_budget(&output, &error) && Instant::now() < deadline,
            "Python source probe exceeded its budget"
        );
        if child.exited() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.finish();
    assert!(
        output_within_budget(&output, &error),
        "oversized final probe response"
    );
    assert!(status.success(), "{}", fs::read_to_string(&error).unwrap());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "requires explicit pinned Darwin Python runtime"]
#[allow(clippy::too_many_lines)] // One live process binds runtime inspection to all OS guard controls.
fn isolated_python_images_and_os_guard() {
    use codemap::{
        CapturedRuntime, DarwinImageCatalog, RuntimeManifest, RuntimeRole, guard_policy,
    };
    use std::io::{ErrorKind, Write};
    use std::net::TcpListener;

    let manifest: RuntimeManifest = serde_json::from_slice(
        &fs::read(
            std::env::var_os("CODEMAP_PYTHON_RUNTIME_MANIFEST")
                .expect("explicit Python runtime manifest"),
        )
        .unwrap(),
    )
    .unwrap();
    let runtime = CapturedRuntime::capture_python(&manifest).unwrap();
    let catalog = DarwinImageCatalog::capture(&runtime).unwrap();
    let executable = manifest
        .files
        .values()
        .find(|file| file.role == RuntimeRole::Python)
        .unwrap();
    let sandboxes = manifest
        .files
        .values()
        .filter(|file| file.role == RuntimeRole::Sandbox)
        .collect::<Vec<_>>();
    assert_eq!(sandboxes.len(), 1);
    let scratch = tempfile::Builder::new()
        .prefix("codemap python guard é ")
        .tempdir()
        .unwrap();
    let private = fs::canonicalize(scratch.path()).unwrap();
    let state = private.join("state");
    fs::create_dir(&state).unwrap();
    let outside = private.join("outside");
    fs::write(&outside, "unchanged").unwrap();
    let ready = state.join("ready");
    let done = state.join("done");
    let helper = state.join("guard.py");
    let parser = state.join("probe.py");
    fs::write(
        &helper,
        include_bytes!("../../../tests/fixtures/python_runtime/guard.py"),
    )
    .unwrap();
    fs::write(
        &parser,
        include_bytes!("../../../toolchain/python/probe.py"),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let request = state.join("request.json");
    fs::write(&request, serde_json::json!({"ready":ready,"done":done,"outside":outside,"port":listener.local_addr().unwrap().port()}).to_string()).unwrap();
    let error = state.join("stderr");
    let mut process = ProbeProcess(Some(
        Command::new(&sandboxes[0].path)
            .process_group(0)
            .env_clear()
            .env("HOME", &state)
            .env("TMPDIR", &state)
            .current_dir(&state)
            .args(["-p", &guard_policy(&state, &executable.path).unwrap()])
            .arg(&executable.path)
            .args(["-I", "-S", "-B"])
            .arg(&helper)
            .arg(&request)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(fs::File::create(&error).unwrap())
            .spawn()
            .unwrap(),
    ));
    wait_probe_marker(&process, &ready, &error);
    let before = catalog.inspect(process.0.as_ref().unwrap().id()).unwrap();
    process
        .0
        .as_mut()
        .unwrap()
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"1")
        .unwrap();
    wait_probe_marker(&process, &done, &error);
    assert_eq!(
        before,
        catalog.inspect(process.0.as_ref().unwrap().id()).unwrap()
    );
    assert!(fs::metadata(&done).unwrap().len() <= 4096);
    let result: serde_json::Value = serde_json::from_slice(&fs::read(&done).unwrap()).unwrap();
    assert_eq!(
        result,
        serde_json::json!({"network":true,"write":true,"execute":true,"source_parser":true})
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "unchanged");
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    runtime.verify_unchanged().unwrap();
    assert_eq!(
        fs::read(&helper).unwrap(),
        include_bytes!("../../../tests/fixtures/python_runtime/guard.py")
    );
    assert_eq!(
        fs::read(&parser).unwrap(),
        include_bytes!("../../../toolchain/python/probe.py")
    );
    process
        .0
        .as_mut()
        .unwrap()
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"2")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !process.exited() {
        assert!(
            fs::metadata(&error).unwrap().len() <= 1024 * 1024 && Instant::now() < deadline,
            "Python guard exit budget exceeded"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(fs::metadata(&error).unwrap().len() <= 1024 * 1024);
    assert!(process.finish().success());
    runtime.verify_unchanged().unwrap();
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn wait_probe_marker(process: &ProbeProcess, marker: &Path, error: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            fs::metadata(error).unwrap().len() <= 1024 * 1024 && Instant::now() < deadline,
            "Python guard marker budget exceeded"
        );
        if marker.is_file() {
            break;
        }
        if process.exited() {
            let mut stderr = Vec::new();
            fs::File::open(error)
                .unwrap()
                .take(1024 * 1024 + 1)
                .read_to_end(&mut stderr)
                .unwrap();
            assert!(stderr.len() <= 1024 * 1024, "Python stderr budget exceeded");
            panic!("Python exited: {}", String::from_utf8_lossy(&stderr));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "requires explicit pinned Darwin Python runtime and complete operator bundle"]
fn isolated_captured_compiler_bootstrap() {
    assert!(guarded_native_queries(None).is_some());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "requires explicit pinned native workload and complete Darwin operator/runtime"]
fn isolated_captured_native_workload() {
    use base64::Engine as _;
    use std::io::Write as _;
    let path = std::path::PathBuf::from(
        std::env::var_os("CODEMAP_PYTHON_NATIVE_QUERY").expect("pinned source query"),
    );
    let pin = std::env::var("CODEMAP_PYTHON_NATIVE_QUERY_SHA256").expect("query pin");
    assert_eq!(interpreter_digest(&path).unwrap(), pin);
    let mut bytes = Vec::new();
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .unwrap()
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 32 * 1024 * 1024);
    assert_eq!(sha256_hex(&bytes), pin);
    let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let keys = request
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert!(keys == ["files"] || keys == ["files", "package_profile"]);
    let mut files = serde_json::Map::new();
    for entry in request["files"].as_array().unwrap() {
        assert_eq!(
            entry.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["path", "sha256", "source_base64"]
        );
        let raw = base64::engine::general_purpose::STANDARD
            .decode(entry["source_base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(sha256_hex(&raw), entry["sha256"].as_str().unwrap());
        assert!(
            files
                .insert(
                    entry["path"].as_str().unwrap().to_owned(),
                    serde_json::Value::String(String::from_utf8(raw).unwrap())
                )
                .is_none()
        );
    }
    let output = std::path::PathBuf::from(
        std::env::var_os("CODEMAP_PYTHON_NATIVE_QUERY_OUTPUT")
            .expect("exclusive local result path"),
    );
    assert!(!output.exists());
    let mut workload = serde_json::json!({"files":files});
    if let Some(profile) = request.get("package_profile") {
        workload["package_profile"] = profile.clone();
    }
    let result = guarded_native_queries(Some(workload));
    assert_eq!(interpreter_digest(&path).unwrap(), pin);
    let bytes = serde_json::to_vec(&result.unwrap()).unwrap();
    assert!(bytes.len() <= 32 * 1024 * 1024);
    let mut destination = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(output)
        .unwrap();
    destination.write_all(&bytes).unwrap();
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[allow(clippy::too_many_lines)] // One owned process binds copied inputs, module origins, images and OS guard controls.
fn guarded_native_queries(workload: Option<serde_json::Value>) -> Option<serde_json::Value> {
    use base64::Engine as _;
    use codemap::python_inputs::{CapturedOperator, OperatorManifest, OperatorRoots};
    use codemap::{CapturedRuntime, DarwinImageCatalog, RuntimeFile, RuntimeManifest, RuntimeRole};
    use std::io::Write;

    let manifest_path =
        std::env::var_os("CODEMAP_PYTHON_OPERATOR_MANIFEST").expect("operator manifest");
    let pin = std::env::var("CODEMAP_PYTHON_OPERATOR_MANIFEST_SHA256").expect("manifest pin");
    assert_eq!(interpreter_digest(Path::new(&manifest_path)).unwrap(), pin);
    let mut bytes = Vec::new();
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&manifest_path)
        .unwrap()
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 32 * 1024 * 1024);
    assert_eq!(sha256_hex(&bytes), pin);
    let operator_manifest: OperatorManifest = serde_json::from_slice(&bytes).unwrap();
    let roots = OperatorRoots {
        runtime: std::env::var_os("CODEMAP_PYTHON_OPERATOR_RUNTIME")
            .expect("runtime root")
            .into(),
        compiler: Some(
            std::env::var_os("CODEMAP_PYTHON_OPERATOR_COMPILER")
                .expect("compiler root")
                .into(),
        ),
    };
    let scratch = tempfile::Builder::new()
        .prefix("codemap python bootstrap é ")
        .tempdir()
        .unwrap();
    let private = fs::canonicalize(scratch.path()).unwrap();
    let source = private.join("source");
    fs::create_dir(&source).unwrap();
    let operator = CapturedOperator::capture(&operator_manifest, &roots, &source).unwrap();
    assert_eq!(
        operator.fingerprint(),
        std::env::var("CODEMAP_PYTHON_OPERATOR_FINGERPRINT").expect("independent fingerprint")
    );
    let copied = operator.materialize(&private.join("operator")).unwrap();
    let copied_capture = CapturedOperator::capture(&operator_manifest, &copied, &source).unwrap();
    let mut manifest: RuntimeManifest = serde_json::from_slice(
        &fs::read(std::env::var_os("CODEMAP_PYTHON_RUNTIME_MANIFEST").expect("runtime manifest"))
            .unwrap(),
    )
    .unwrap();
    for file in manifest
        .files
        .values_mut()
        .filter(|file| file.role == RuntimeRole::Python)
    {
        assert_eq!(
            file.sha256,
            operator_manifest.files["{runtime}/bin/python3.14"]
        );
        file.path = copied.runtime.join("bin/python3.14");
    }
    for module in ["base64", "internal"] {
        let relative = format!("dependencies/librt/{module}.cpython-314-darwin.so");
        let logical = format!("{{compiler}}/{relative}");
        assert!(
            manifest
                .files
                .insert(
                    format!("python/compiler/{relative}"),
                    RuntimeFile {
                        path: copied.compiler.as_ref().unwrap().join(&relative),
                        sha256: operator_manifest.files[&logical].clone(),
                        role: RuntimeRole::Library,
                    }
                )
                .is_none()
        );
    }
    let captured_runtime = CapturedRuntime::capture_python(&manifest).unwrap();
    let catalog = DarwinImageCatalog::capture(&captured_runtime).unwrap();
    let sandboxes = manifest
        .files
        .values()
        .filter(|file| file.role == RuntimeRole::Sandbox)
        .collect::<Vec<_>>();
    assert_eq!(sandboxes.len(), 1);
    let oracle: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_oracle.json"
    ))
    .unwrap();
    let locations: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_evidence_oracle.json"
    ))
    .unwrap();
    let private_workload = workload.is_some();
    let observation_oracle: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../toolchain/python/native_observation_oracle.json"
    ))
    .unwrap();
    let cases = workload.map_or_else(
        || oracle["cases"].as_array().unwrap().clone(),
        |case| vec![case],
    );
    let mut last_result = None;
    for (index, case) in cases.iter().enumerate() {
        let state = private.join(format!("state-{index}"));
        fs::create_dir(&state).unwrap();
        let bootstrap_script = state.join("bootstrap.py");
        let ready = state.join("ready");
        let done = state.join("done");
        let error = state.join("stderr");
        let request = state.join("request.json");
        let outside = source.join("outside-read-control");
        fs::write(&outside, b"unapproved").unwrap();
        fs::write(
            &bootstrap_script,
            include_bytes!("../../../toolchain/python/native_bootstrap.py"),
        )
        .unwrap();
        let helper_pins = NATIVE_QUERY_HELPERS
            .iter()
            .map(|(name, bytes)| {
                fs::write(state.join(name), bytes).unwrap();
                (name.to_string(), sha256_hex(bytes))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let request_bytes = serde_json::json!({"runtime":copied.runtime,"compiler":copied.compiler,"files":operator_manifest.files,"directories":operator_manifest.directories,"ready":ready,"done":done,"outside":outside,"helpers":helper_pins}).to_string();
        assert!(request_bytes.len() <= 32 * 1024 * 1024);
        fs::write(&request, &request_bytes).unwrap();
        let policy = copied_capture
            .guard_policy(&state, &captured_runtime)
            .unwrap();
        let policy_path = state.join("guard.sb");
        fs::write(&policy_path, &policy).unwrap();
        let executable = copied.runtime.join("bin/python3.14");
        let mut process = ProbeProcess(Some(
            Command::new(&sandboxes[0].path)
                .process_group(0)
                .env_clear()
                .env("HOME", &state)
                .env("TMPDIR", &state)
                .current_dir(&state)
                .arg("-f")
                .arg(&policy_path)
                .arg(&executable)
                .args(["-I", "-S", "-B"])
                .arg(&bootstrap_script)
                .arg(&request)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(fs::File::create(&error).unwrap())
                .spawn()
                .unwrap(),
        ));
        wait_probe_marker(&process, &ready, &error);
        assert!(fs::metadata(&ready).unwrap().len() <= 1024 * 1024);
        let mut ready_bytes = Vec::new();
        fs::File::open(&ready)
            .unwrap()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut ready_bytes)
            .unwrap();
        assert!(ready_bytes.len() <= 1024 * 1024);
        let report: serde_json::Value = serde_json::from_slice(&ready_bytes).unwrap();
        assert_eq!(report["version"], serde_json::json!([3, 14, 7]));
        assert_eq!(report["compiler"], "2.3.1");
        let origins = report["origins"].as_object().unwrap();
        assert!(origins.contains_key("mypyc.codegen.emitmodule"));
        assert!(origins.contains_key("librt.base64") && origins.contains_key("librt.internal"));
        for origin in origins.values().filter_map(serde_json::Value::as_str) {
            assert!(
                origin == "built-in"
                    || origin == "frozen"
                    || operator_manifest.files.contains_key(origin)
                    || origin
                        .strip_prefix("{helper}/")
                        .is_some_and(|name| helper_pins.contains_key(name))
            );
        }
        let images = catalog.inspect(process.0.as_ref().unwrap().id()).unwrap();
        process
            .0
            .as_mut()
            .unwrap()
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"1")
            .unwrap();
        let entries = case["files"].as_object().unwrap().iter().map(|(path, source)| {
            let bytes = source.as_str().unwrap().as_bytes();
            serde_json::json!({"path":path,"source_base64":base64::engine::general_purpose::STANDARD.encode(bytes),"sha256":sha256_hex(bytes)})
        }).collect::<Vec<_>>();
        let profile = case.get("package_profile").cloned().unwrap_or_else(|| {
            serde_json::json!({"source_roots":["."],"namespace_packages":true,"external_stub_roots":[]})
        });
        let source_bytes = case["files"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(path, source)| (path.clone(), source.as_str().unwrap().as_bytes().to_vec()))
            .collect();
        let identity =
            NativeProfile::for_query(&copied_capture, &captured_runtime, &profile, &source_bytes)
                .unwrap();
        let original_identity =
            NativeProfile::for_query(&operator, &captured_runtime, &profile, &source_bytes)
                .unwrap();
        assert_eq!(
            identity.fingerprint().unwrap(),
            original_identity.fingerprint().unwrap()
        );
        let query =
            serde_json::json!({"files":entries,"package_profile":profile}).to_string() + "\n";
        process
            .0
            .as_mut()
            .unwrap()
            .stdin
            .as_mut()
            .unwrap()
            .write_all(query.as_bytes())
            .unwrap();
        wait_probe_marker(&process, &done, &error);
        let mut done_bytes = Vec::new();
        fs::File::open(&done)
            .unwrap()
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut done_bytes)
            .unwrap();
        assert!(done_bytes.len() <= 32 * 1024 * 1024);
        let mut result: serde_json::Value = serde_json::from_slice(&done_bytes).unwrap();
        assert_eq!(result["canonical_admission"], false);
        let candidates = result["observations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["status"] == "eligible_candidate")
            .map(|item| serde_json::json!({"path":item["path"],"call":item["call"]}))
            .collect::<Vec<_>>();
        let evidence_sites = result["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| serde_json::json!({"path":item["path"],"call":item["call"]}))
            .collect::<Vec<_>>();
        assert_eq!(candidates, evidence_sites);
        if !private_workload {
            let observed = result["observations"].as_array().unwrap().iter().map(|item| {
                serde_json::json!({"path":item["path"],"span":item["call"]["span"],"callee_span":item["call"]["callee_span"],"status":item["status"],"reason":item["reason"]})
            }).collect::<Vec<_>>();
            assert_eq!(
                serde_json::json!(observed),
                observation_oracle["cases"][case["name"].as_str().unwrap()]
            );
            assert_eq!(result["calls"], case["expected_calls"], "{}", case["name"]);
            assert_eq!(result["canonical_admission"], false);
            assert_eq!(
                result["evidence"].as_array().unwrap().len(),
                case["expected_calls"].as_array().unwrap().len()
            );
            let expected_locations =
                locations["locations"][case["name"].as_str().unwrap()].as_array();
            assert_eq!(
                result["evidence"].as_array().unwrap().len(),
                expected_locations.map_or(0, Vec::len)
            );
            for (item, location) in result["evidence"].as_array().unwrap().iter().zip(
                locations["locations"][case["name"].as_str().unwrap()]
                    .as_array()
                    .into_iter()
                    .flatten(),
            ) {
                let coordinates = location.as_array().unwrap();
                let number =
                    |index: usize| usize::try_from(coordinates[index].as_u64().unwrap()).unwrap();
                let raw = case["files"][item["path"].as_str().unwrap()]
                    .as_str()
                    .unwrap();
                assert_eq!(
                    item["call"]["span"],
                    fixture_byte_span(raw, number(0), number(1), number(0), number(2))
                );
                assert_eq!(item["caller"]["qualified_name"], coordinates[3]);
                assert_eq!(
                    item["caller"]["span"],
                    fixture_byte_span(raw, number(4), number(5), number(6), number(7))
                );
                assert_eq!(item["target_path"], coordinates[8]);
                let target_raw = case["files"][coordinates[8].as_str().unwrap()]
                    .as_str()
                    .unwrap();
                assert_eq!(
                    item["target"]["span"],
                    fixture_byte_span(target_raw, number(9), 0, number(10), number(11))
                );
            }
        }
        for entry in &entries {
            let address = format!("{{source}}/{}", entry["path"].as_str().unwrap());
            assert!(
                result["read_ledger"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|observation| {
                        observation[0] == "read"
                            && observation[1] == address
                            && observation[2] == entry["sha256"]
                    })
            );
        }
        assert!(!copied.runtime.join("write-control").exists());
        assert_eq!(
            images,
            catalog.inspect(process.0.as_ref().unwrap().id()).unwrap()
        );
        copied_capture.verify_unchanged().unwrap();
        captured_runtime.verify_unchanged().unwrap();
        assert_eq!(
            fs::read(&bootstrap_script).unwrap(),
            include_bytes!("../../../toolchain/python/native_bootstrap.py")
        );
        process
            .0
            .as_mut()
            .unwrap()
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"2")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !process.exited() {
            assert!(
                Instant::now() < deadline && fs::metadata(&error).unwrap().len() <= 1024 * 1024,
                "bootstrap exit budget exceeded"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(fs::metadata(&error).unwrap().len() <= 1024 * 1024);
        assert!(process.finish().success());
        let mut after_request = Vec::new();
        fs::File::open(&request)
            .unwrap()
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut after_request)
            .unwrap();
        assert_eq!(after_request, request_bytes.as_bytes());
        assert_eq!(fs::read(&policy_path).unwrap(), policy.as_bytes());
        for (name, bytes) in NATIVE_QUERY_HELPERS {
            assert_eq!(fs::read(state.join(name)).unwrap(), *bytes);
        }
        operator.verify_unchanged().unwrap();
        copied_capture.verify_unchanged().unwrap();
        captured_runtime.verify_unchanged().unwrap();
        assert_eq!(interpreter_digest(Path::new(&manifest_path)).unwrap(), pin);
        result["execution_profile"] = serde_json::to_value(&identity).unwrap();
        result["execution_profile_fingerprint"] = identity.fingerprint().unwrap().into();
        last_result = Some(result);
        eprintln!(
            "Guarded query {} completed with input/image/ledger proof",
            index + 1
        );
    }
    scratch.close().unwrap();
    last_result
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn fixture_byte_span(
    raw: &str,
    first: usize,
    column: usize,
    last: usize,
    end_column: usize,
) -> serde_json::Value {
    let lines = raw
        .as_bytes()
        .split_inclusive(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    serde_json::json!({"start_line":first,"end_line":last,
        "start_byte":lines.iter().take(first - 1).map(|line| line.len()).sum::<usize>() + column,
        "end_byte":lines.iter().take(last - 1).map(|line| line.len()).sum::<usize>() + end_column})
}
