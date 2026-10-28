#![cfg(target_os = "linux")]

use codemap::linux_kernel_images::{LinuxKernelPins, LinuxVsyscallPolicy};
use codemap::linux_process::LinuxLaunch;
use codemap::linux_runtime::{
    AnonymousExecutePolicy, CapturedLinuxRuntime, LinuxArchitecture, LinuxRuntimeFile,
    LinuxRuntimeManifest, LinuxRuntimeRole,
};
use codemap::schema::sha256_hex;
use codemap::{LinuxLspConfig, LinuxLspLimits, LinuxLspTransport};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    os::unix::fs::{FileExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

struct Fixture {
    directory: tempfile::TempDir,
    manifest: LinuxRuntimeManifest,
    kernel: LinuxKernelPins,
}
impl Fixture {
    fn prepare() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let manifest: LinuxRuntimeManifest = serde_json::from_slice(
            &fs::read(
                std::env::var_os("CODEMAP_LINUX_DYNAMIC_RUNTIME_MANIFEST")
                    .expect("operator dynamic manifest required"),
            )
            .unwrap(),
        )
        .unwrap();
        let dump = directory.path().join("vdso");
        assert!(
            Command::new(
                std::env::var_os("CODEMAP_LINUX_KERNEL_FIXTURE")
                    .expect("operator kernel fixture required")
            )
            .arg("dump")
            .arg(&dump)
            .status()
            .unwrap()
            .success()
        );
        let bytes = fs::read(dump).unwrap();
        let kernel = LinuxKernelPins {
            architecture: manifest.architecture,
            vdso_size: u64::try_from(bytes.len()).unwrap(),
            vdso_sha256: sha256_hex(&bytes),
            vsyscall: if manifest.architecture == LinuxArchitecture::X86_64 {
                LinuxVsyscallPolicy::FixedEmulated
            } else {
                LinuxVsyscallPolicy::Absent
            },
        };
        Self {
            directory,
            manifest,
            kernel,
        }
    }
    fn start(
        &self,
        name: &str,
        mode: &str,
        manifest: &LinuxRuntimeManifest,
    ) -> anyhow::Result<LinuxLspTransport> {
        let state = self.directory.path().join(name);
        fs::create_dir(&state)?;
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
        let executable = manifest
            .files
            .values()
            .find(|pin| pin.role == LinuxRuntimeRole::Clangd)
            .unwrap()
            .path
            .clone();
        LinuxLspTransport::spawn_linux(
            "dynamic-fixture",
            LinuxLspConfig {
                launch: LinuxLaunch {
                    executable,
                    arguments: vec![mode.into(), state.join("ready").into_os_string()],
                    environment: BTreeMap::new(),
                    directory: fs::canonicalize(state)?,
                },
                runtime: CapturedLinuxRuntime::capture(manifest)?,
                kernel: self.kernel.clone(),
                reads: vec![],
                directories: vec![],
                limits: LinuxLspLimits {
                    request_millis: 10_000,
                    frame_millis: 30_000,
                    ..LinuxLspLimits::default()
                },
            },
        )
    }
    fn loader(&self) -> &Path {
        &self
            .manifest
            .files
            .values()
            .find(|pin| pin.role == LinuxRuntimeRole::Loader)
            .unwrap()
            .path
    }
}

#[test]
#[ignore = "requires independently prepared dynamic/static fixtures and loader pins on native ordinary Linux"]
fn frozen_native_interpreter_controls() {
    let fixture = Fixture::prepare();
    let mut observed = BTreeSet::new();
    independent_baseline(&mut observed);
    let session = fixture
        .start("accepted", "echo", &fixture.manifest)
        .unwrap();
    assert_eq!(
        session.request("initialize", &json!({})).unwrap(),
        json!(true)
    );
    observed.insert("dynamic_guarded_initialize");
    session.verify_linux_images().unwrap();
    observed.insert("dynamic_complete_image_witness");
    session.shutdown();
    missing_library(&fixture, &mut observed);
    let session = fixture
        .start("extra", "dynamic-extra", &fixture.manifest)
        .unwrap();
    assert_eq!(
        session.request("initialize", &json!({})).unwrap(),
        json!(true)
    );
    let error = session.verify_linux_images().unwrap_err();
    assert!(format!("{error:#}").contains("outside pinned closure"));
    observed.insert("dynamic_extra_image");
    memory_tamper(&fixture, &mut observed);
    refused_mutation(&fixture, &mut observed);
    disk_drift(&fixture, &mut observed);
    let session = fixture
        .start("denials", "dynamic-guard", &fixture.manifest)
        .unwrap();
    let response = session.request("initialize", &json!({})).unwrap();
    for (field, label) in [
        ("execution_denied", "dynamic_unapproved_execution_denied"),
        ("network_denied", "dynamic_network_denied"),
        ("write_denied", "dynamic_outside_write_denied"),
    ] {
        assert_eq!(response[field], json!(true), "{label}");
        observed.insert(label);
    }
    session.verify_linux_images().unwrap();
    session.shutdown();
    static_regression(&fixture, &mut observed);
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../toolchain/linux/interpreter_expectations.json"
    ))
    .unwrap();
    let expected: BTreeSet<_> = oracle["native"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(observed, expected);
}

fn independent_baseline(observed: &mut BTreeSet<&'static str>) {
    // The baseline tool must be the separately prepared version-reporting clangd,
    // because the synthetic LSP fixture has a different command-line contract.
    let tool = std::env::var_os("CODEMAP_LINUX_OPERATOR_CLANGD").expect("operator clangd required");
    let output = Command::new(
        std::env::var_os("CODEMAP_LINUX_INTERPRETER_BASELINE")
            .expect("independent baseline helper required"),
    )
    .arg(tool)
    .arg(
        std::env::var_os("CODEMAP_LINUX_OPERATOR_LOADER")
            .expect("operator clangd interpreter required"),
    )
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(
        "interpreter_execute_missing=77 exact_interpreter_execute=0 unapproved_executable=77"
    ));
    observed.extend([
        "independent_missing_execute_baseline",
        "independent_exact_execute_baseline",
        "independent_unapproved_execute_baseline",
    ]);
}

fn missing_library(fixture: &Fixture, observed: &mut BTreeSet<&'static str>) {
    let mut manifest = fixture.manifest.clone();
    let missing = manifest
        .files
        .iter()
        .find(|(_, pin)| pin.role == LinuxRuntimeRole::Library)
        .unwrap()
        .0
        .clone();
    manifest.files.remove(&missing);
    let result = fixture
        .start("missing-library", "echo", &manifest)
        .and_then(|session| session.request("initialize", &json!({})));
    assert!(result.is_err());
    observed.insert("dynamic_missing_library");
}

fn memory_tamper(fixture: &Fixture, observed: &mut BTreeSet<&'static str>) {
    let session = fixture.start("memory", "echo", &fixture.manifest).unwrap();
    session.request("initialize", &json!({})).unwrap();
    let pid = session.process_id().unwrap();
    // SAFETY: the session owns this unreaped live process throughout inspection.
    assert_eq!(
        unsafe { libc::kill(i32::try_from(pid).unwrap(), libc::SIGSTOP) },
        0
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while !fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .any(|line| line.starts_with("State:\tT"))
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let maps = fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
    let mapping = maps
        .lines()
        .find(|line| {
            let parts = line.split_whitespace().collect::<Vec<_>>();
            parts.len() == 6 && parts[1] == "r-xp" && Path::new(parts[5]) == fixture.loader()
        })
        .unwrap();
    let address = u64::from_str_radix(mapping.split_once('-').unwrap().0, 16).unwrap();
    let memory = OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("/proc/{pid}/mem"))
        .unwrap();
    let disk = sha256_hex(&fs::read(fixture.loader()).unwrap());
    let mut byte = [0];
    memory.read_exact_at(&mut byte, address).unwrap();
    byte[0] ^= 1;
    memory.write_all_at(&byte, address).unwrap();
    assert_eq!(sha256_hex(&fs::read(fixture.loader()).unwrap()), disk);
    let error = session.verify_linux_images().unwrap_err();
    assert!(format!("{error:#}").contains("memory differs"), "{error:#}");
    observed.insert("dynamic_loader_memory_tamper");
}

struct Restore {
    path: PathBuf,
    bytes: Vec<u8>,
}
impl Drop for Restore {
    fn drop(&mut self) {
        fs::write(&self.path, &self.bytes).unwrap();
    }
}
fn tamper_private_loader(loader: &Path, private: &Path) -> anyhow::Result<Restore> {
    let metadata = fs::metadata(private)?;
    // SAFETY: geteuid only reads this test process's ordinary identity.
    anyhow::ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o777 == 0o700,
        "loader test root must be owned and private"
    );
    anyhow::ensure!(
        fs::canonicalize(private)? == private
            && fs::canonicalize(loader)? == loader
            && loader.starts_with(private),
        "loader is outside the private test runtime"
    );
    let bytes = fs::read(loader)?;
    let file = OpenOptions::new().write(true).open(loader)?;
    let restore = Restore {
        path: loader.to_path_buf(),
        bytes,
    };
    file.write_all_at(&[restore.bytes[0] ^ 1], 0)?;
    Ok(restore)
}
fn refused_mutation(fixture: &Fixture, observed: &mut BTreeSet<&'static str>) {
    let private = tempfile::tempdir().unwrap();
    fs::set_permissions(private.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let before = fs::metadata(fixture.loader()).unwrap();
    let digest = sha256_hex(&fs::read(fixture.loader()).unwrap());
    assert!(
        tamper_private_loader(fixture.loader(), &fs::canonicalize(private.path()).unwrap())
            .is_err()
    );
    let after = fs::metadata(fixture.loader()).unwrap();
    assert_eq!(sha256_hex(&fs::read(fixture.loader()).unwrap()), digest);
    assert_eq!(
        (
            before.ino(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec()
        ),
        (
            after.ino(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec()
        )
    );
    observed.insert("out_of_root_loader_untouched");
}
fn disk_drift(fixture: &Fixture, observed: &mut BTreeSet<&'static str>) {
    let session = fixture.start("disk", "echo", &fixture.manifest).unwrap();
    session.request("initialize", &json!({})).unwrap();
    let private = PathBuf::from(
        std::env::var_os("CODEMAP_LINUX_PRIVATE_RUNTIME")
            .expect("private operator runtime required"),
    );
    let restore = tamper_private_loader(fixture.loader(), &private).unwrap();
    let error = session.verify_linux_images().unwrap_err();
    assert!(format!("{error:#}").contains("changed"), "{error:#}");
    observed.insert("dynamic_loader_disk_drift");
    drop(session);
    drop(restore);
}

fn static_regression(fixture: &Fixture, observed: &mut BTreeSet<&'static str>) {
    let executable = fs::canonicalize(
        std::env::var_os("CODEMAP_LINUX_LSP_FIXTURE").expect("operator static fixture required"),
    )
    .unwrap();
    let manifest = LinuxRuntimeManifest {
        architecture: fixture.manifest.architecture,
        anonymous_execute: AnonymousExecutePolicy::Deny,
        files: BTreeMap::from([(
            "fixture".into(),
            LinuxRuntimeFile {
                sha256: sha256_hex(&fs::read(&executable).unwrap()),
                path: executable,
                role: LinuxRuntimeRole::Clangd,
            },
        )]),
    };
    let session = fixture.start("static", "echo", &manifest).unwrap();
    assert_eq!(
        session.request("initialize", &json!({})).unwrap(),
        json!(true)
    );
    session.verify_linux_images().unwrap();
    session.shutdown();
    observed.insert("static_existing_guard_unchanged");
}
