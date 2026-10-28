//! Owned Linux supervision for the existing two-message compiler helper.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use crate::linux_guard::LinuxGuard;
use crate::linux_io::{LinuxReader, LinuxWriter};
use crate::linux_kernel_images::LinuxKernelPins;
use crate::linux_process::{LinuxLaunch, OwnedLinuxChild, StoppedLinuxChild};
use crate::linux_runtime::CapturedLinuxRuntime;

use crate::js_ts_semantic::{ERROR_LIMIT, OUTPUT_LIMIT};
pub const DEADLINE: Duration = Duration::from_secs(400);
pub const FLAGS: [&str; 3] = ["--jitless", "--no-addons", "--max-old-space-size=4096"];

pub struct HelperSession {
    child: Option<OwnedLinuxChild>,
    stdin: LinuxWriter,
    stdout: LinuxReader,
    errors: Option<JoinHandle<Result<Vec<u8>>>>,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
    output_bytes: u64,
    error_bytes: Vec<u8>,
}

impl HelperSession {
    pub(crate) fn spawn(launch: LinuxLaunch, guard: LinuxGuard) -> Result<Self> {
        let mut child = OwnedLinuxChild::spawn_guarded(launch, guard)?;
        let deadline = Instant::now() + DEADLINE;
        let pipes = child.take_stdio()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let stdin = LinuxWriter::from_pipe(pipes.stdin.into(), Arc::clone(&cancelled), 2)?;
        let stdout = LinuxReader::from_pipe(
            pipes.stdout.into(),
            Arc::clone(&cancelled),
            OUTPUT_LIMIT + 1,
        )?;
        let mut stderr =
            LinuxReader::from_pipe(pipes.stderr.into(), Arc::clone(&cancelled), ERROR_LIMIT + 1)?;
        let error_cancelled = Arc::clone(&cancelled);
        let errors = std::thread::Builder::new()
            .name("js-ts-helper-errors".into())
            .spawn(move || {
                let result = (|| -> Result<Vec<u8>> {
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    loop {
                        let count = stderr.read_some(&mut buffer, deadline)?;
                        if count == 0 {
                            return Ok(bytes);
                        }
                        ensure!(
                            bytes.len() as u64 + count as u64 <= ERROR_LIMIT,
                            "helper error budget exceeded"
                        );
                        bytes.extend_from_slice(&buffer[..count]);
                    }
                })();
                if result.is_err() {
                    error_cancelled.store(true, Ordering::Release);
                }
                result
            })?;
        Ok(Self {
            child: Some(child),
            stdin,
            stdout,
            errors: Some(errors),
            cancelled,
            deadline,
            output_bytes: 0,
            error_bytes: Vec::new(),
        })
    }

    fn check_errors(&mut self) -> Result<()> {
        if self.errors.as_ref().is_some_and(JoinHandle::is_finished)
            || self.cancelled.load(Ordering::Acquire)
        {
            if let Some(worker) = self.errors.take() {
                self.error_bytes = worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("helper error worker panicked"))??;
            }
        }
        ensure!(
            Instant::now() < self.deadline,
            "guarded helper deadline exceeded"
        );
        Ok(())
    }

    pub(crate) fn read_frame(&mut self, limit: u64) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            self.check_errors()?;
            let count = match self.stdout.read_some(&mut buffer, self.deadline) {
                Ok(count) => count,
                Err(error) => {
                    self.check_errors()?;
                    return Err(error.into());
                }
            };
            if count == 0 {
                let status = self
                    .child
                    .take()
                    .context("helper ownership missing")?
                    .terminate()?;
                if let Some(worker) = self.errors.take() {
                    self.error_bytes = worker
                        .join()
                        .map_err(|_| anyhow::anyhow!("helper error worker panicked"))??;
                }
                bail!(
                    "helper exited before frame ({status}): {}",
                    String::from_utf8_lossy(&self.error_bytes)
                );
            }
            self.output_bytes += count as u64;
            ensure!(
                self.output_bytes <= OUTPUT_LIMIT && bytes.len() as u64 + count as u64 <= limit,
                "helper output/frame budget exceeded"
            );
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = buffer[..count].iter().position(|byte| *byte == b'\n') {
                ensure!(end + 1 == count, "unexpected helper frame/output");
                bytes.pop();
                self.check_errors()?;
                return Ok(bytes);
            }
        }
    }

    pub(crate) fn control(&mut self, byte: u8) -> Result<()> {
        self.check_errors()?;
        self.stdin.write_control(&[byte], self.deadline)?;
        self.check_errors()
    }

    pub(crate) fn images(
        &mut self,
        runtime: &CapturedLinuxRuntime,
        kernel: &LinuxKernelPins,
    ) -> Result<Vec<String>> {
        self.check_errors()?;
        let stopped =
            StoppedLinuxChild::capture(self.child.as_mut().context("helper ownership missing")?)?;
        runtime.verify_complete_images(&stopped, kernel)?;
        let report = stopped.maps()?;
        let mappings = crate::linux_maps::parse_maps(&report)?;
        let images = mappings
            .iter()
            .zip(report.lines())
            .filter(|(map, _)| map.path.is_some() || map.execute)
            .map(|(_, line)| line.to_owned())
            .collect();
        stopped.resume()?;
        self.check_errors()?;
        Ok(images)
    }

    pub(crate) fn finish(mut self) -> Result<()> {
        self.control(b'2')?;
        let mut extra = [0; 1];
        let count = match self.stdout.read_some(&mut extra, self.deadline) {
            Ok(count) => count,
            Err(error) => {
                self.check_errors()?;
                return Err(error.into());
            }
        };
        ensure!(count == 0, "extra helper output");
        self.child
            .as_ref()
            .context("helper ownership missing")?
            .wait_for_exit(self.deadline)?;
        let status = self
            .child
            .take()
            .context("helper ownership missing")?
            .terminate()?;
        if let Some(worker) = self.errors.take() {
            self.error_bytes = worker
                .join()
                .map_err(|_| anyhow::anyhow!("helper error worker panicked"))??;
        }
        ensure!(
            status.success(),
            "guarded helper failed ({status}): {}",
            String::from_utf8_lossy(&self.error_bytes)
        );
        self.check_errors()
    }
}

impl Drop for HelperSession {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        drop(self.child.take());
        if let Some(worker) = self.errors.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
#[path = "js_ts_linux_tests.rs"]
mod tests;
