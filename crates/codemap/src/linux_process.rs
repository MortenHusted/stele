//! Owned, stopped Linux child inspection. This supplies kernel witnesses, not
//! a sandbox or permission to publish semantic facts.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxProcessIdentity {
    pid: u32,
    start_time: u64,
    parent: u32,
    group: u32,
}

/// Pure launch inputs. Caller callbacks and inherited environment are excluded.
pub struct LinuxLaunch {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub directory: PathBuf,
}

/// Owned pipe handoff only. Protocol byte budgets and deadlines remain required.
pub struct LinuxChildIo {
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

/// Spawn-time ownership keeps the original child unreaped and its kernel
/// handles bound until termination. A cached/reaped std Child is never accepted.
pub struct OwnedLinuxChild {
    child: Option<Child>,
    process: File,
    pidfd: File,
    identity: LinuxProcessIdentity,
    guard_policy: Option<&'static str>,
}

/// The mutable owner borrow excludes termination during inspection.
/// Drop attempts resume; transport deadlines and guards are separate contracts.
pub struct StoppedLinuxChild<'a> {
    child: &'a mut OwnedLinuxChild,
    resume_on_drop: bool,
}

impl OwnedLinuxChild {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        let child = command.process_group(0).spawn()?;
        let bound = (|| -> Result<(File, File, LinuxProcessIdentity)> {
            let pid = child.id();
            let process = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(format!("/proc/{pid}"))?;
            // SAFETY: initialized output and an owned valid file descriptor.
            let mut filesystem: libc::statfs = unsafe { std::mem::zeroed() };
            // SAFETY: statfs writes only to its initialized output pointer.
            if unsafe { libc::fstatfs(process.as_raw_fd(), &raw mut filesystem) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            if filesystem.f_type != libc::PROC_SUPER_MAGIC {
                bail!("Linux inspection requires actual procfs");
            }
            // SAFETY: pidfd_open has no pointer arguments and flags are zero.
            let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            if descriptor < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // SAFETY: successful syscall returned a new owned descriptor.
            let pidfd = unsafe { File::from_raw_fd(i32::try_from(descriptor)?) };
            let (identity, state) = parse_stat(&read_report(&process, c"stat", 16384)?)?;
            if matches!(state, 'T' | 't' | 'Z' | 'X' | 'x') {
                bail!("Linux child must be live and initially running");
            }
            if identity.pid != pid || identity.parent != std::process::id() || identity.group != pid
            {
                bail!("Linux inspection requires an owned group-leading child");
            }
            Ok((process, pidfd, identity))
        })();
        match bound {
            Ok((process, pidfd, identity)) => Ok(Self {
                child: Some(child),
                process,
                pidfd,
                identity,
                guard_policy: None,
            }),
            Err(error) => {
                let _ = kill_and_wait(child, false);
                Err(error)
            }
        }
    }

    pub const fn id(&self) -> u32 {
        self.identity.pid
    }

    /// The guarded API cannot accept an opaque command with an earlier exec callback.
    /// ```compile_fail,E0308
    /// use codemap::linux_process::OwnedLinuxChild;
    /// use codemap::linux_guard::LinuxGuard;
    /// fn earlier_exec(command: std::process::Command, guard: LinuxGuard) {
    ///     let _ = OwnedLinuxChild::spawn_guarded(command, guard);
    /// }
    /// ```
    pub fn spawn_guarded(
        launch: LinuxLaunch,
        mut guard: crate::linux_guard::LinuxGuard,
    ) -> Result<Self> {
        let mut command = Command::new(launch.executable);
        command
            .args(launch.arguments)
            .env_clear()
            .envs(launch.environment)
            .current_dir(launch.directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: prepared guard uses only async-signal-safe syscall/stack work.
        unsafe {
            command.pre_exec(move || guard.enforce());
        }
        let mut child = Self::spawn(&mut command)?;
        child.guard_policy = Some(crate::linux_guard::LINUX_GUARD_POLICY);
        Ok(child)
    }

    pub const fn guard_policy(&self) -> Option<&'static str> {
        self.guard_policy
    }

    pub fn verify_live(&self) -> Result<()> {
        let mut poll = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd; zero timeout does not block.
        let ready = unsafe { libc::poll(&raw mut poll, 1, 0) };
        if ready < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if ready != 0 || process_identity(&self.process)? != self.identity {
            bail!("Linux process exited or changed identity");
        }
        Ok(())
    }

    /// Hand off pipes once without exposing a reusable Child or wait operation.
    pub fn take_stdio(&mut self) -> Result<LinuxChildIo> {
        if self.guard_policy != Some(crate::linux_guard::LINUX_GUARD_POLICY) {
            bail!("Linux stdio handoff requires a guarded child");
        }
        self.verify_live()?;
        let child = self.child.as_mut().expect("live owner retains its child");
        if child.stdin.is_none() || child.stdout.is_none() || child.stderr.is_none() {
            bail!("Linux guarded stdio was already taken");
        }
        Ok(LinuxChildIo {
            stdin: child.stdin.take().expect("checked owned stdin"),
            stdout: child.stdout.take().expect("checked owned stdout"),
            stderr: child.stderr.take().expect("checked owned stderr"),
        })
    }

    pub(crate) fn wait_for_exit(&self, deadline: Instant) -> Result<()> {
        loop {
            if Instant::now() >= deadline {
                bail!("Linux helper exit deadline exceeded");
            }
            let mut poll = libc::pollfd {
                fd: self.pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one initialized pollfd bound to the retained, unreaped child.
            let ready = unsafe { libc::poll(&raw mut poll, 1, 10) };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if ready > 0 {
                if poll.revents & libc::POLLIN == 0 {
                    bail!("invalid Linux helper exit witness");
                }
                if Instant::now() >= deadline {
                    bail!("Linux helper exit deadline exceeded");
                }
                return Ok(());
            }
        }
    }

    pub fn terminate(mut self) -> Result<ExitStatus> {
        let child = self.child.take().expect("live owner retains its child");
        kill_and_wait(child, true)
    }
}

impl Drop for OwnedLinuxChild {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            let _ = kill_and_wait(child, true);
        }
    }
}

fn kill_and_wait(mut child: Child, owned_group: bool) -> Result<ExitStatus> {
    let pid = i32::try_from(child.id())?;
    // SAFETY: freshly spawned or sealed owned child has not been reaped.
    let group = unsafe { libc::getpgid(pid) };
    if owned_group || group == pid {
        // SAFETY: this group was created for the selected unreaped child.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    child.kill()?;
    reap_until(child, Instant::now() + Duration::from_secs(2))
}

fn reap_until(mut child: Child, deadline: Instant) -> Result<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            // Ownership leaves the signaling path permanently. This worker can
            // only wait; it never signals a cached PID or process group.
            std::thread::Builder::new()
                .name("codemap-linux-reaper".into())
                .spawn(move || {
                    let _ = child.wait();
                })?;
            bail!("Linux child cleanup deadline exceeded");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

impl<'a> StoppedLinuxChild<'a> {
    pub fn capture(child: &'a mut OwnedLinuxChild) -> Result<Self> {
        let pid = child.id();
        let mut stopped = Self {
            child,
            resume_on_drop: false,
        };
        stopped.verify_identity()?;
        let pid = i32::try_from(pid)?;
        // SAFETY: positive PID belongs to the selected unreaped child.
        if unsafe { libc::kill(pid, libc::SIGSTOP) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        stopped.resume_on_drop = true;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            // SAFETY: initialized siginfo output for a non-reaping child wait.
            let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: valid output; WNOWAIT preserves supervisor ownership.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    stopped.child.id(),
                    &raw mut information,
                    libc::WSTOPPED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // SAFETY: successful waitid initialized siginfo, including pid.
            if unsafe { information.si_pid() } == pid && information.si_code == libc::CLD_STOPPED {
                break;
            }
            stopped.verify_identity()?;
            if Instant::now() >= deadline {
                bail!("Linux child stop deadline exceeded");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        stopped.verify_identity()?;
        stopped.verify_stopped_threads()?;
        Ok(stopped)
    }

    pub const fn identity(&self) -> &LinuxProcessIdentity {
        &self.child.identity
    }

    pub fn resume(mut self) -> Result<()> {
        self.verify_identity()?;
        let pid = i32::try_from(self.child.id())?;
        // SAFETY: selected child is still borrowed and unreaped.
        if unsafe { libc::kill(pid, libc::SIGCONT) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            self.verify_identity()?;
            let (_, state) = parse_stat(&read_report(&self.child.process, c"stat", 16384)?)?;
            if !matches!(state, 'T' | 't') {
                self.resume_on_drop = false;
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("Linux child resume deadline exceeded");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub fn verify_identity(&self) -> Result<()> {
        self.child.verify_live()
    }

    pub fn executable(&self) -> Result<PathBuf> {
        self.verify_stopped_threads()?;
        // The proc directory handle remains bound to the original task even if
        // the numerical PID is subsequently reused. No new /proc/PID lookup.
        let path = fs::read_link(format!(
            "/proc/self/fd/{}/exe",
            self.child.process.as_raw_fd()
        ))?;
        if !path.is_absolute() || fs::canonicalize(&path)? != path {
            bail!("Linux executable witness is deleted or noncanonical");
        }
        self.verify_stopped_threads()?;
        Ok(path)
    }

    pub const fn guard_policy(&self) -> Option<&'static str> {
        self.child.guard_policy
    }

    pub fn auxiliary_vector(&self) -> Result<Vec<u8>> {
        self.verify_stopped_threads()?;
        let bytes = read_binary_report(&self.child.process, c"auxv", 4096)?;
        self.verify_stopped_threads()?;
        Ok(bytes)
    }

    pub fn maps(&self) -> Result<String> {
        self.verify_stopped_threads()?;
        let maps = read_report(&self.child.process, c"maps", 8 * 1024 * 1024)?;
        if maps.is_empty() || !maps.ends_with('\n') {
            bail!("incomplete Linux maps witness");
        }
        self.verify_stopped_threads()?;
        Ok(maps)
    }

    pub(crate) fn mapped_file(&self, start: u64, end: u64) -> Result<PathBuf> {
        self.verify_stopped_threads()?;
        let path = fs::read_link(format!(
            "/proc/self/fd/{}/map_files/{start:x}-{end:x}",
            self.child.process.as_raw_fd()
        ))?;
        self.verify_stopped_threads()?;
        Ok(path)
    }

    /// Callers must independently admit ranges against pinned images and ELF
    /// segments. Reading memory here establishes no such admission.
    pub fn read_memory(&self, address: u64, buffer: &mut [u8]) -> Result<()> {
        self.verify_stopped_threads()?;
        let memory = open_report(&self.child.process, c"mem")?;
        memory
            .read_exact_at(buffer, address)
            .context("Linux executable-memory witness unavailable")?;
        self.verify_stopped_threads()
    }

    fn verify_stopped_threads(&self) -> Result<()> {
        self.verify_identity()?;
        let task = open_report(&self.child.process, c"task")?;
        let mut count = 0;
        for entry in fs::read_dir(format!("/proc/self/fd/{}", task.as_raw_fd()))? {
            let name = entry?.file_name();
            let name = name.to_str().context("invalid Linux thread ID")?;
            if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_digit()) {
                bail!("invalid Linux thread ID");
            }
            let relative = std::ffi::CString::new(format!("{name}/stat"))?;
            let report = read_report(&task, &relative, 16384)?;
            let (_, state) = parse_stat(&report)?;
            if state != 'T' {
                bail!("Linux thread is not kernel-confirmed stopped");
            }
            count += 1;
            if count > 4096 {
                bail!("Linux thread witness exceeds bound");
            }
        }
        if count == 0 {
            bail!("Linux child has no stopped thread witness");
        }
        self.verify_identity()
    }
}

impl Drop for StoppedLinuxChild<'_> {
    fn drop(&mut self) {
        // SAFETY: Child is still borrowed/unreaped; signal only this selected PID.
        if !self.resume_on_drop {
            return;
        }
        if let Ok(pid) = i32::try_from(self.child.id()) {
            // SAFETY: sealed owner remains borrowed and unreaped.
            unsafe {
                libc::kill(pid, libc::SIGCONT);
            }
        }
    }
}

fn open_report(directory: &File, name: &std::ffi::CStr) -> Result<File> {
    // SAFETY: owned directory, NUL-terminated name and no variadic mode required.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: openat returned a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn read_binary_report(directory: &File, name: &std::ffi::CStr, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_report(directory, name)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("Linux proc witness exceeds bound");
    }
    Ok(bytes)
}

fn read_report(directory: &File, name: &std::ffi::CStr, limit: u64) -> Result<String> {
    Ok(String::from_utf8(read_binary_report(
        directory, name, limit,
    )?)?)
}

fn process_identity(directory: &File) -> Result<LinuxProcessIdentity> {
    let report = read_report(directory, c"stat", 16384)?;
    let (identity, state) = parse_stat(&report)?;
    if matches!(state, 'Z' | 'X' | 'x') {
        bail!("Linux process is no longer live");
    }
    Ok(identity)
}

fn parse_stat(report: &str) -> Result<(LinuxProcessIdentity, char)> {
    let open = report.find(" (").context("malformed Linux process stat")?;
    let close = report.rfind(") ").context("malformed Linux process stat")?;
    if close <= open {
        bail!("malformed Linux process stat");
    }
    let pid = report[..open].parse()?;
    let fields = report[close + 2..].split_whitespace().collect::<Vec<_>>();
    if fields.len() < 20 || fields[0].len() != 1 {
        bail!("incomplete Linux process stat");
    }
    let state = fields[0]
        .chars()
        .next()
        .context("missing Linux process state")?;
    if !"RSDTtXZPIWKx".contains(state) {
        bail!("unknown Linux process state");
    }
    Ok((
        LinuxProcessIdentity {
            pid,
            parent: fields[1].parse()?,
            group: fields[2].parse()?,
            start_time: fields[19].parse()?,
        },
        state,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    #[test]
    fn stat_parser_binds_birth_parent_group_with_parentheses_in_name() {
        let report = "42 (complex ) name) S 7 42 42 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 987 0 0\n";
        let (identity, state) = parse_stat(report).unwrap();
        assert_eq!(
            identity,
            LinuxProcessIdentity {
                pid: 42,
                parent: 7,
                group: 42,
                start_time: 987
            }
        );
        assert_eq!(state, 'S');
        for invalid in [
            "",
            "42 no-name S",
            "42 (name) S 7 42",
            "42 (name) QQ 7 42",
            "not-a-pid (name) S 7 42",
        ] {
            assert!(parse_stat(invalid).is_err(), "{invalid}");
        }
        assert!(parse_stat(&report.replace("987", "bad-time")).is_err());
    }

    #[test]
    #[ignore = "requires ordinary native Linux wait-only reaper control"]
    fn expired_wait_budget_hands_off_to_wait_only_reaper() {
        let child = Command::new("/usr/bin/sleep").arg("15").spawn().unwrap();
        let id = child.id();
        // SAFETY: pidfd_open binds the unreaped child before wait ownership moves.
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, id, 0) };
        assert!(descriptor >= 0);
        // SAFETY: syscall returned a fresh owned descriptor.
        let pidfd = unsafe { File::from_raw_fd(i32::try_from(descriptor).unwrap()) };
        let start = Instant::now();
        assert!(
            reap_until(child, Instant::now())
                .unwrap_err()
                .to_string()
                .contains("cleanup deadline exceeded")
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        // SAFETY: retained pidfd selects this exact child; no cached PID signal.
        assert_eq!(
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    pidfd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            },
            0
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            // SAFETY: initialized output, retained pidfd, observation never reaps.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let status = unsafe {
                libc::waitid(
                    libc::P_PIDFD,
                    u32::try_from(pidfd.as_raw_fd()).unwrap(),
                    &raw mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if status == -1 {
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ECHILD)
                );
                break;
            }
            assert_eq!(status, 0);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../toolchain/linux/process_io_expectations.json"
        ))
        .unwrap();
        assert_eq!(
            oracle["expected"]["expired_wait_budget_background_reaper"],
            "accept"
        );
    }

    fn fixture() -> OwnedLinuxChild {
        let mut command = Command::new("/usr/bin/sleep");
        command
            .arg("15")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        OwnedLinuxChild::spawn(&mut command).unwrap()
    }

    #[test]
    #[ignore = "requires explicit native Linux ordinary-parent kernel controls"]
    fn owned_stop_memory_resume_and_failure_controls() {
        // Operator execution must confirm ordinary uid/capabilities separately.
        // SAFETY: geteuid has no pointer arguments or side effects.
        assert_ne!(unsafe { libc::geteuid() }, 0);
        let mut selected = fixture();
        let first = {
            let stopped = StoppedLinuxChild::capture(&mut selected).unwrap();
            assert_eq!(
                stopped.executable().unwrap(),
                fs::canonicalize("/usr/bin/sleep").unwrap()
            );
            let maps = stopped.maps().unwrap();
            let image = maps
                .lines()
                .find(|line| {
                    line.contains("/usr/bin/sleep")
                        && line.split_whitespace().nth(2) == Some("00000000")
                })
                .unwrap();
            let address = u64::from_str_radix(image.split_once('-').unwrap().0, 16).unwrap();
            let mut magic = [0; 4];
            stopped.read_memory(address, &mut magic).unwrap();
            assert_eq!(&magic, b"\x7fELF");
            assert!(stopped.read_memory(0, &mut magic).is_err());
            let identity = stopped.identity().clone();
            stopped.resume().unwrap();
            identity
        };
        {
            let stopped = StoppedLinuxChild::capture(&mut selected).unwrap();
            assert_eq!(stopped.identity(), &first);
            // Dropping after an inspection failure must attempt resume.
            let mut byte = [0];
            assert!(stopped.read_memory(0, &mut byte).is_err());
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let report = fs::read_to_string(format!("/proc/{}/stat", selected.id())).unwrap();
            let (_, state) = parse_stat(&report).unwrap();
            if state != 'T' {
                break;
            }
            assert!(Instant::now() < deadline, "selected child remained stopped");
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut other = fixture();
        let stopped = StoppedLinuxChild::capture(&mut other).unwrap();
        assert_ne!(stopped.identity(), &first);
        stopped.resume().unwrap();
        foreign_group_and_exited_controls();

        let mut resumed = fixture();
        let pid = i32::try_from(resumed.id()).unwrap();
        let stopped = StoppedLinuxChild::capture(&mut resumed).unwrap();
        // SAFETY: positive PID is the still-borrowed, unreaped fixture child.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGCONT) }, 0);
        let deadline = Instant::now() + Duration::from_secs(2);
        while stopped.verify_stopped_threads().is_ok() {
            assert!(
                Instant::now() < deadline,
                "external resume was not observed"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(stopped.maps().is_err());
        assert!(stopped.executable().is_err());
        assert!(stopped.read_memory(0, &mut [0]).is_err());
        drop(stopped);
    }

    fn foreign_group_and_exited_controls() {
        let mut unowned_group = Command::new("/usr/bin/sleep");
        unowned_group
            .arg("15")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: controlled fixture resets only its own group before exec.
        let parent_group = unsafe { libc::getpgrp() };
        // SAFETY: callback invokes only an async-signal-safe libc syscall.
        unsafe {
            unowned_group.pre_exec(move || {
                if libc::setpgid(0, parent_group) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        assert!(OwnedLinuxChild::spawn(&mut unowned_group).is_err());
        let mut exited = fixture();
        let pid = i32::try_from(exited.id()).unwrap();
        // SAFETY: selected sealed child is still unreaped.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let deadline = Instant::now() + Duration::from_secs(2);
        // pidfd reports exit without reaping the original selected process.
        loop {
            let mut poll = libc::pollfd {
                fd: exited.pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one initialized pollfd with nonblocking timeout.
            if unsafe { libc::poll(&raw mut poll, 1, 0) } == 1 {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(StoppedLinuxChild::capture(&mut exited).is_err());
        exited.terminate().unwrap();
    }
}
