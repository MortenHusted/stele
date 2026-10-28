//! Owned Python process groups; exit observation never reaps the group leader.

use std::fs;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};

pub struct OwnedPython(Option<Child>);

impl OwnedPython {
    pub fn new(mut child: Child) -> Result<Self> {
        let pid = libc::pid_t::try_from(child.id()).context("Python process ID")?;
        if unsafe { libc::getpgid(pid) } != pid {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Python helper must own its process group");
        }
        Ok(Self(Some(child)))
    }

    pub const fn child(&mut self) -> &mut Child {
        self.0.as_mut().expect("owned Python child before finish")
    }

    pub fn pid(&self) -> u32 {
        self.0
            .as_ref()
            .expect("owned Python child before finish")
            .id()
    }

    pub fn exited(&self) -> Result<bool> {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let status = unsafe {
            libc::waitid(
                libc::P_PID,
                self.pid(),
                &raw mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        ensure!(
            status == 0,
            "Python exit observation: {}",
            std::io::Error::last_os_error()
        );
        Ok(unsafe { info.si_pid() != 0 })
    }

    fn terminate_group(&self) {
        // A live or unreaped leader retains the group identity until cleanup.
        let pid = libc::pid_t::try_from(self.pid()).expect("validated Python process ID");
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }

    pub fn finish(mut self) -> Result<ExitStatus> {
        self.terminate_group();
        self.0
            .take()
            .expect("owned Python child before finish")
            .wait()
            .context("reap owned Python process")
    }
}

impl Drop for OwnedPython {
    fn drop(&mut self) {
        if self.0.is_some() {
            self.terminate_group();
            let _ = self.0.take().expect("owned Python child").wait();
        }
    }
}

const LIMIT: u64 = 32 * 1024 * 1024;
const ERROR_LIMIT: u64 = 1024 * 1024;

pub fn check_budget(errors: &Path, deadline: Instant) -> Result<()> {
    ensure!(
        Instant::now() < deadline && fs::metadata(errors)?.len() <= ERROR_LIMIT,
        "Python process deadline or stderr budget exceeded"
    );
    Ok(())
}

pub fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= limit,
        "bounded regular Python process output required"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Python process output grew beyond budget"
    );
    Ok(bytes)
}

pub fn await_marker(
    process: &OwnedPython,
    path: &Path,
    errors: &Path,
    deadline: Instant,
    limit: u64,
) -> Result<Vec<u8>> {
    loop {
        check_budget(errors, deadline)?;
        if path.exists() {
            return read_bounded(path, limit);
        }
        ensure!(
            !process.exited()?,
            "Python process exited before result: {}",
            String::from_utf8_lossy(&read_bounded(errors, ERROR_LIMIT)?)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn write_input(
    process: &mut OwnedPython,
    bytes: &[u8],
    errors: &Path,
    deadline: Instant,
) -> Result<()> {
    let stdin = process
        .child()
        .stdin
        .as_mut()
        .context("owned Python stdin missing")?;
    let fd = stdin.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
        "nonblocking Python input required"
    );
    let mut offset = 0;
    while offset < bytes.len() {
        check_budget(errors, deadline)?;
        ensure!(!process.exited()?, "Python process exited during request");
        match process
            .child()
            .stdin
            .as_mut()
            .unwrap()
            .write(&bytes[offset..])
        {
            Ok(0) => anyhow::bail!("Python process input closed"),
            Ok(count) => offset += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("write owned Python request"),
        }
    }
    Ok(())
}

pub fn verify_helpers(
    helpers: &[(PathBuf, &[u8])],
    config: &Path,
    config_bytes: &[u8],
    policy: &Path,
    policy_bytes: &[u8],
) -> Result<()> {
    for (path, bytes) in helpers {
        ensure!(
            read_bounded(path, LIMIT)? == *bytes,
            "Python process helper changed"
        );
    }
    ensure!(
        read_bounded(config, LIMIT)? == config_bytes
            && read_bounded(policy, LIMIT)? == policy_bytes,
        "Python process configuration or guard changed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[test]
    fn exit_observation_retains_group_leader_until_cleanup() {
        let process = OwnedPython::new(
            Command::new("/bin/sh")
                .process_group(0)
                .args(["-c", "exit 0"])
                .spawn()
                .unwrap(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !process.exited().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid = libc::pid_t::try_from(process.pid()).unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
        assert!(process.finish().unwrap().success());
    }

    #[test]
    fn dropping_process_terminates_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let heartbeat = directory.path().join("heartbeat");
        let process = OwnedPython::new(
            Command::new("/bin/sh")
                .process_group(0)
                .args([
                    "-c",
                    "(while :; do echo alive >> \"$1\"; sleep 0.1; done) & wait",
                    "owned-python",
                ])
                .arg(&heartbeat)
                .spawn()
                .unwrap(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !heartbeat.exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(process);
        std::thread::sleep(Duration::from_millis(100));
        let stopped = fs::read(&heartbeat).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(fs::read(&heartbeat).unwrap(), stopped);
    }
}
