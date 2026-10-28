//! Owned Linux execution policy for the canonical LSP transport.

use super::{ChildOwner, Pending, ProtocolInput, Transport, lock_until, route_message};
use crate::linux_guard::LinuxGuard;
use crate::linux_io::{
    FrameLimits, LinuxFrameReader, LinuxPipes, LinuxReader, MAX_BODY_BYTES, MAX_HEADER_BYTES,
};
use crate::linux_kernel_images::LinuxKernelPins;
use crate::linux_process::{LinuxLaunch, OwnedLinuxChild, StoppedLinuxChild};
use crate::linux_runtime::CapturedLinuxRuntime;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex, MutexGuard, OnceLock,
    atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Smaller limits may be selected; production maxima cannot be enlarged.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxLspLimits {
    pub request_millis: u64,
    pub frame_millis: u64,
    pub header_bytes: usize,
    pub body_bytes: usize,
    pub pending: usize,
    pub unsolicited: usize,
    pub session_frames: usize,
    pub stream_bytes: u64,
    pub stderr_bytes: usize,
}

impl Default for LinuxLspLimits {
    fn default() -> Self {
        Self {
            request_millis: 120_000,
            frame_millis: 120_000,
            header_bytes: MAX_HEADER_BYTES,
            body_bytes: MAX_BODY_BYTES,
            pending: 64,
            unsolicited: 32,
            session_frames: 32_768,
            stream_bytes: 64 * 1024 * 1024,
            stderr_bytes: 1024 * 1024,
        }
    }
}

impl LinuxLspLimits {
    pub fn validate(&self) -> Result<()> {
        let maximum = Self::default();
        for (value, limit) in [
            (self.request_millis, maximum.request_millis),
            (self.frame_millis, maximum.frame_millis),
            (
                u64::try_from(self.header_bytes)?,
                u64::try_from(maximum.header_bytes)?,
            ),
            (
                u64::try_from(self.body_bytes)?,
                u64::try_from(maximum.body_bytes)?,
            ),
            (
                u64::try_from(self.pending)?,
                u64::try_from(maximum.pending)?,
            ),
            (
                u64::try_from(self.unsolicited)?,
                u64::try_from(maximum.unsolicited)?,
            ),
            (
                u64::try_from(self.session_frames)?,
                u64::try_from(maximum.session_frames)?,
            ),
            (self.stream_bytes, maximum.stream_bytes),
            (
                u64::try_from(self.stderr_bytes)?,
                u64::try_from(maximum.stderr_bytes)?,
            ),
        ] {
            if value == 0 || value > limit {
                bail!("invalid Linux LSP limits");
            }
        }
        Ok(())
    }

    pub(super) const fn frames(self) -> FrameLimits {
        FrameLimits {
            header_bytes: self.header_bytes,
            body_bytes: self.body_bytes,
        }
    }

    pub(super) const fn request_duration(self) -> Duration {
        Duration::from_millis(self.request_millis)
    }
}

pub struct LinuxLspConfig {
    pub launch: LinuxLaunch,
    pub runtime: CapturedLinuxRuntime,
    pub kernel: LinuxKernelPins,
    pub reads: Vec<PathBuf>,
    pub directories: Vec<PathBuf>,
    pub limits: LinuxLspLimits,
}

pub(super) struct LinuxControl {
    child: Mutex<Option<OwnedLinuxChild>>,
    runtime: CapturedLinuxRuntime,
    kernel: LinuxKernelPins,
    pub(super) limits: LinuxLspLimits,
    cancelled: Arc<AtomicBool>,
    error: OnceLock<String>,
    outgoing_frames: AtomicUsize,
    pending: Arc<Pending>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl LinuxControl {
    pub(super) fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            bail!(
                "Linux LSP session failed: {}",
                self.error.get().map_or("closed", String::as_str)
            );
        }
        Ok(())
    }

    fn lease(&self, deadline: Instant) -> Result<ChildLease<'_>> {
        Ok(ChildLease {
            owner: Some(lock_until(&self.child, Some(deadline))?),
            control: self,
        })
    }

    pub(super) fn verify_live(&self, deadline: Instant) -> Result<u32> {
        self.check()?;
        let mut lease = self.lease(deadline)?;
        let child = lease
            .owner()
            .as_ref()
            .context("Linux LSP child is closed")?;
        child.verify_live()?;
        let pid = child.id();
        drop(lease);
        self.check()?;
        Ok(pid)
    }

    pub(super) fn write(
        &self,
        stdin: &Mutex<crate::linux_io::LinuxWriter>,
        value: &Value,
        deadline: Instant,
    ) -> Result<()> {
        let mut writer = lock_until(stdin, Some(deadline))?;
        self.check()?;
        if self.outgoing_frames.fetch_add(1, Ordering::Relaxed) >= self.limits.session_frames {
            bail!("Linux LSP outbound frame budget exceeded");
        }
        writer.write_frame(value, self.limits.frames(), deadline)
    }

    pub(super) fn fail(&self, reason: &str) {
        let _ = self.error.set(reason.to_owned());
        self.cancelled.store(true, Ordering::Release);
        if let Ok(mut pending) = self.pending.try_lock() {
            pending.clear();
        }
        self.try_cleanup_child();
        // An active lease retains cleanup responsibility until it releases the owner.
    }

    fn try_cleanup_child(&self) {
        if let Ok(mut owner) = self.child.try_lock() {
            let child = owner.take();
            drop(owner);
            if let Some(child) = child {
                let _ = child.terminate();
            }
        }
    }

    pub(super) fn join(&self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let workers = {
            let mut handles = lock_until(&self.workers, Some(deadline))?;
            std::mem::take(&mut *handles)
        };
        while workers.iter().any(|worker| !worker.is_finished()) {
            if Instant::now() >= deadline {
                bail!("Linux LSP worker cleanup deadline exceeded");
            }
            thread::sleep(Duration::from_millis(1));
        }
        for worker in workers {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("Linux LSP worker panicked"))?;
        }
        Ok(())
    }
}

struct ChildLease<'a> {
    owner: Option<MutexGuard<'a, Option<OwnedLinuxChild>>>,
    control: &'a LinuxControl,
}

impl ChildLease<'_> {
    fn owner(&mut self) -> &mut Option<OwnedLinuxChild> {
        self.owner.as_mut().expect("active process lease")
    }
}

impl Drop for ChildLease<'_> {
    fn drop(&mut self) {
        drop(self.owner.take());
        if self.control.cancelled.load(Ordering::Acquire) {
            self.control.try_cleanup_child();
        }
    }
}

impl Transport {
    /// Launch with a guard prepared from these exact retained runtime pins.
    /// Source/read closure and semantic admission remain the adapter's concern.
    pub fn spawn_linux(server: &'static str, config: LinuxLspConfig) -> Result<Self> {
        config.limits.validate()?;
        config.kernel.fingerprint(&config.runtime)?;
        let guard = LinuxGuard::prepare(
            &config.runtime,
            &config.reads,
            &config.directories,
            &config.launch.directory,
        )?;
        let mut child = OwnedLinuxChild::spawn_guarded(config.launch, guard)?;
        let mut pipes = LinuxPipes::new(child.take_stdio()?)?;
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let cancelled = Arc::new(AtomicBool::new(false));
        pipes.cancel_on(Arc::clone(&cancelled));
        pipes.stdout.set_byte_budget(config.limits.stream_bytes);
        pipes.stdin.set_byte_budget(config.limits.stream_bytes);
        let stdin = Arc::new(Mutex::new(pipes.stdin));
        let control = Arc::new(LinuxControl {
            child: Mutex::new(Some(child)),
            runtime: config.runtime,
            kernel: config.kernel,
            limits: config.limits,
            cancelled,
            error: OnceLock::new(),
            outgoing_frames: AtomicUsize::new(0),
            pending: Arc::clone(&pending),
            workers: Mutex::new(Vec::new()),
        });
        let (tx, rx) = mpsc::sync_channel(config.limits.unsolicited);
        let reader_control = Arc::clone(&control);
        let reader_stdin = Arc::clone(&stdin);
        let reader = thread::Builder::new()
            .name("codemap-linux-lsp-stdout".into())
            .spawn(move || {
                let result = read_frames(pipes.stdout, &reader_control, &reader_stdin, &tx);
                if let Err(error) = result {
                    reader_control.fail(&error.to_string());
                }
            })?;
        control
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("LSP worker state poisoned"))?
            .push(reader);
        let stderr_control = Arc::clone(&control);
        let stderr = thread::Builder::new()
            .name("codemap-linux-lsp-stderr".into())
            .spawn(move || {
                let result = drain_stderr(pipes.stderr, &stderr_control);
                if let Err(error) = result {
                    stderr_control.fail(&error.to_string());
                }
            });
        match stderr {
            Ok(worker) => control
                .workers
                .lock()
                .map_err(|_| anyhow::anyhow!("LSP worker state poisoned"))?
                .push(worker),
            Err(error) => {
                control.fail("failed to spawn stderr reader");
                let _ = control.join();
                return Err(error.into());
            }
        }
        let transport = Self {
            server,
            child: ChildOwner::Linux(control),
            stdin: ProtocolInput::Linux(stdin),
            unsolicited: Mutex::new(rx),
            pending,
            next_id: AtomicI64::new(1),
        };
        transport.finish(Ok(()))?;
        Ok(transport)
    }

    /// Explicit protocol-phase checkpoint; no image pins are learned from the child.
    pub fn verify_linux_images(&self) -> Result<()> {
        let ChildOwner::Linux(control) = &self.child else {
            bail!("Linux image checkpoint requires an owned Linux transport");
        };
        let result = (|| {
            control.check()?;
            let deadline = Instant::now() + control.limits.request_duration();
            let mut owner = control.lease(deadline)?;
            let child = owner
                .owner()
                .as_mut()
                .context("Linux LSP child is closed")?;
            let stopped = StoppedLinuxChild::capture(child)?;
            control
                .runtime
                .verify_complete_images(&stopped, &control.kernel)?;
            stopped.resume()?;
            drop(owner);
            if Instant::now() >= deadline {
                bail!("Linux image checkpoint deadline exceeded");
            }
            Ok(())
        })();
        self.finish(result)
    }

    pub fn linux_policy_fingerprint(&self) -> Result<String> {
        let ChildOwner::Linux(control) = &self.child else {
            bail!("Linux policy identity requires an owned Linux transport");
        };
        Ok(crate::schema::sha256_hex(&serde_json::to_vec(&(
            "linux-owned-lsp/2",
            control.kernel.fingerprint(&control.runtime)?,
            control.limits,
        ))?))
    }
}

fn read_frames(
    mut stdout: LinuxFrameReader,
    control: &LinuxControl,
    stdin: &Mutex<crate::linux_io::LinuxWriter>,
    unsolicited: &mpsc::SyncSender<Value>,
) -> Result<()> {
    for index in 0..=control.limits.session_frames {
        control.check()?;
        loop {
            let deadline = Instant::now() + Duration::from_millis(control.limits.frame_millis);
            match stdout.wait_frame_start(deadline) {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => control.check()?,
                Err(error) => return Err(error.into()),
            }
        }
        let deadline = Instant::now() + Duration::from_millis(control.limits.frame_millis);
        let value = stdout
            .read_frame(control.limits.frames(), deadline)?
            .context("Linux LSP stdout closed")?;
        if index == control.limits.session_frames {
            bail!("Linux LSP session frame budget exceeded");
        }
        route_message(
            value,
            &control.pending,
            Some(deadline),
            |request| {
                if let Some(id) = request.get("id") {
                    if !id.is_i64() && !id.is_string() {
                        bail!("invalid LSP server request id");
                    }
                    control.write(
                        stdin,
                        &json!({
                            "jsonrpc":"2.0", "id":id,
                            "error":{"code":-32601,"message":"Unsupported server request"}
                        }),
                        deadline,
                    )?;
                }
                Ok(())
            },
            |value| {
                unsolicited
                    .try_send(value)
                    .context("Linux LSP unsolicited queue exhausted or closed")
            },
        )?;
    }
    bail!("Linux LSP session frame budget exceeded")
}

fn drain_stderr(mut stderr: LinuxReader, control: &LinuxControl) -> Result<()> {
    let mut buffer = [0; 8192];
    let mut total = 0;
    loop {
        control.check()?;
        let deadline = Instant::now() + Duration::from_millis(control.limits.frame_millis);
        match stderr.read_some(&mut buffer, deadline) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                total += count;
                if total > control.limits.stderr_bytes {
                    bail!("Linux LSP stderr byte budget exceeded");
                }
            }
            // A quiet stderr is normal while stdout remains live.
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
#[path = "lsp_linux_tests.rs"]
mod tests;
