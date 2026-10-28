#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use codemap::linux_guard::{LINUX_GUARD_POLICY, LinuxGuard};
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

fn inherited_descriptors(outside: &Path) -> (File, File) {
    let input = File::open(outside).unwrap();
    // SAFETY: duplicate owned input into an unused high descriptor, without CLOEXEC.
    let descriptor = unsafe { libc::fcntl(input.as_raw_fd(), libc::F_DUPFD, 100) };
    assert!(descriptor >= 100);
    // SAFETY: successful duplication returned an independently owned descriptor.
    let inherited_file = unsafe { File::from_raw_fd(descriptor) };
    let mut sockets = [-1; 2];
    // SAFETY: initialized two-element output and no communication is performed.
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sockets.as_mut_ptr()) },
        0
    );
    // SAFETY: returned socket descriptors are independently owned.
    let first = unsafe { File::from_raw_fd(sockets[0]) };
    let second = unsafe { File::from_raw_fd(sockets[1]) };
    // SAFETY: duplicate socket into another unused high descriptor without CLOEXEC.
    let descriptor = unsafe { libc::fcntl(first.as_raw_fd(), libc::F_DUPFD, 200) };
    assert!(descriptor >= 200);
    drop((first, second));
    // SAFETY: successful duplicate is independently owned.
    (inherited_file, unsafe { File::from_raw_fd(descriptor) })
}

#[test]
#[ignore = "requires native ordinary-parent Linux and operator-compiled static guard fixture"]
fn frozen_guard_launch_controls() {
    let fixture = fs::canonicalize(
        std::env::var_os("CODEMAP_LINUX_GUARD_FIXTURE").expect("operator fixture required"),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let directory = fs::canonicalize(root.path()).unwrap();
    let state = directory.join("state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let pinned = directory.join("pinned");
    let outside = directory.join("outside");
    fs::write(&pinned, b"original").unwrap();
    fs::write(&outside, b"original").unwrap();
    let runtime = runtime(&fixture);
    let guard = LinuxGuard::prepare(&runtime, std::slice::from_ref(&pinned), &[], &state).unwrap();
    let (inherited_file, inherited_socket) = inherited_descriptors(&outside);
    let launch = LinuxLaunch {
        executable: fixture.clone(),
        arguments: vec![
            pinned.as_os_str().to_owned(),
            outside.as_os_str().to_owned(),
            state.as_os_str().to_owned(),
            inherited_file.as_raw_fd().to_string().into(),
            inherited_socket.as_raw_fd().to_string().into(),
            fixture.as_os_str().to_owned(),
        ],
        environment: BTreeMap::new(),
        directory: state.clone(),
    };
    let mut child = OwnedLinuxChild::spawn_guarded(launch, guard).unwrap();
    assert_eq!(child.guard_policy(), Some(LINUX_GUARD_POLICY));
    let deadline = Instant::now() + Duration::from_secs(5);
    let report = loop {
        if let Ok(report) = fs::read_to_string(state.join("report")) {
            if report.ends_with("DONE\n") {
                break report;
            }
        }
        assert!(
            Instant::now() < deadline,
            "guard fixture report deadline exceeded"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    let stopped = StoppedLinuxChild::capture(&mut child).unwrap();
    runtime.verify_file_backed_images(&stopped).unwrap();
    stopped.resume().unwrap();
    child.terminate().unwrap();
    assert_eq!(fs::read(&pinned).unwrap(), b"original");
    assert_eq!(fs::read(&outside).unwrap(), b"original");
    assert_eq!(fs::read(state.join("private")).unwrap(), b"private");
    assert!(!directory.join("outside.new").exists());
    assert!(!state.join("link").exists());
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../toolchain/linux/process_guard_expectations.json"
    ))
    .unwrap();
    let mut observed = BTreeMap::new();
    for line in report.lines().filter(|line| *line != "DONE") {
        let (label, result) = line.split_once('=').unwrap();
        let (passed, kind) = result.split_once(':').unwrap();
        assert_eq!(passed, "1", "guard control {label} failed");
        assert!(observed.insert(label.to_owned(), kind.to_owned()).is_none());
    }
    let baseline = Command::new("/usr/bin/python3")
        .args(["-I", "-S", "-B"])
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../toolchain/linux/auxv_rewrite_probe.py"),
        )
        .output()
        .unwrap();
    assert!(
        baseline.status.success(),
        "auxv baseline failed: {}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    let proof: serde_json::Value = serde_json::from_slice(&baseline.stdout).unwrap();
    assert_eq!(proof["saved_auxv_rewritten"], true);
    assert_eq!(proof["saved_auxv_restored"], true);
    observed.insert("auxv_map_rewrite_baseline".into(), "accept".into());
    failure_cleanup(&fixture);
    observed.insert("guard_failure_cleanup".into(), "accept".into());
    assert_eq!(serde_json::to_value(observed).unwrap(), oracle["expected"]);
}

fn failure_cleanup(fixture: &Path) {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "restricted_guard_failure_helper",
            "--include-ignored",
        ])
        .env("CODEMAP_GUARD_FAILURE_HELPER", "1")
        .env("CODEMAP_LINUX_GUARD_FIXTURE", fixture);
    // SAFETY: trusted test callback uses only initialized stack and libc calls.
    unsafe {
        command.pre_exec(|| {
            let mut filter = [
                libc::sock_filter {
                    code: 0x20,
                    jt: 0,
                    jf: 0,
                    k: 0,
                },
                libc::sock_filter {
                    code: 0x15,
                    jt: 0,
                    jf: 1,
                    k: libc::SYS_landlock_restrict_self as u32,
                },
                libc::sock_filter {
                    code: 0x06,
                    jt: 0,
                    jf: 0,
                    k: 0x0005_0001,
                },
                libc::sock_filter {
                    code: 0x06,
                    jt: 0,
                    jf: 0,
                    k: 0x7fff_0000,
                },
            ];
            let program = libc::sock_fprog {
                len: 4,
                filter: filter.as_mut_ptr(),
            };
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_SECCOMP, 2, &raw const program, 0, 0) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "failure helper: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "trusted isolated helper; invoked by frozen guard cleanup control"]
fn restricted_guard_failure_helper() {
    if std::env::var("CODEMAP_GUARD_FAILURE_HELPER").as_deref() != Ok("1") {
        return;
    }
    let fixture =
        fs::canonicalize(std::env::var_os("CODEMAP_LINUX_GUARD_FIXTURE").unwrap()).unwrap();
    let runtime = runtime(&fixture);
    let root = tempfile::tempdir().unwrap();
    let state = fs::canonicalize(root.path()).unwrap().join("state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let guard = LinuxGuard::prepare(&runtime, &[], &[], &state).unwrap();
    let launch = LinuxLaunch {
        executable: fixture,
        arguments: vec![],
        environment: BTreeMap::new(),
        directory: state,
    };
    let error = OwnedLinuxChild::spawn_guarded(launch, guard)
        .err()
        .expect("missing restriction must reject launch");
    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .unwrap()
            .raw_os_error(),
        Some(libc::EPERM)
    );
    let mut status = 0;
    // SAFETY: no child remains in this isolated ignored test; nonblocking wait.
    assert_eq!(
        unsafe { libc::waitpid(-1, &raw mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}
