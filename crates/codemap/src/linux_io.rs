//! Nonblocking owned pipes and bounded shared LSP framing. Session routing,
//! image/source checks and publication remain the transport/adapter's concern.

use anyhow::{Result, bail};
use serde_json::Value;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use crate::linux_process::LinuxChildIo;
pub use crate::lsp::FrameLimits;

pub const MAX_HEADER_BYTES: usize = 8192;
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

pub struct LinuxPipes {
    pub stdin: LinuxWriter,
    pub stdout: LinuxFrameReader,
    pub stderr: LinuxReader,
}

impl LinuxPipes {
    pub fn new(pipes: LinuxChildIo) -> Result<Self> {
        Ok(Self {
            stdin: LinuxWriter {
                file: nonblocking(OwnedFd::from(pipes.stdin))?,
                cancelled: None,
                bytes: 0,
                byte_budget: None,
            },
            stdout: LinuxFrameReader {
                reader: BufReader::new(LinuxReader {
                    file: nonblocking(OwnedFd::from(pipes.stdout))?,
                    deadline: Instant::now(),
                    cancelled: None,
                    bytes: 0,
                    byte_budget: None,
                }),
            },
            stderr: LinuxReader {
                file: nonblocking(OwnedFd::from(pipes.stderr))?,
                deadline: Instant::now(),
                cancelled: None,
                bytes: 0,
                byte_budget: None,
            },
        })
    }

    pub fn cancel_on(&mut self, cancelled: Arc<AtomicBool>) {
        self.stdin.cancelled = Some(Arc::clone(&cancelled));
        self.stdout.reader.get_mut().cancelled = Some(Arc::clone(&cancelled));
        self.stderr.cancelled = Some(cancelled);
    }
}

pub struct LinuxFrameReader {
    reader: BufReader<LinuxReader>,
}

impl LinuxFrameReader {
    pub(crate) fn wait_frame_start(&self, deadline: Instant) -> io::Result<()> {
        let reader = self.reader.get_ref();
        check_wait(deadline, reader.cancelled.as_deref())?;
        if self.reader.buffer().is_empty() {
            ready(
                &reader.file,
                libc::POLLIN,
                deadline,
                reader.cancelled.as_deref(),
            )?;
        }
        Ok(())
    }

    pub(crate) fn set_byte_budget(&mut self, budget: u64) {
        self.reader.get_mut().byte_budget = Some(budget);
    }

    pub fn read_frame(&mut self, limits: FrameLimits, deadline: Instant) -> Result<Option<Value>> {
        validate_limits(limits)?;
        check_wait(deadline, self.reader.get_ref().cancelled.as_deref())?;
        self.reader.get_mut().deadline = deadline;
        let frame = crate::lsp::read_message_with_limits(&mut self.reader, Some(limits))?;
        check_wait(deadline, self.reader.get_ref().cancelled.as_deref())?;
        Ok(frame)
    }
}

pub struct LinuxReader {
    file: File,
    deadline: Instant,
    cancelled: Option<Arc<AtomicBool>>,
    bytes: u64,
    byte_budget: Option<u64>,
}

impl LinuxReader {
    pub(crate) fn from_pipe(
        pipe: OwnedFd,
        cancelled: Arc<AtomicBool>,
        byte_budget: u64,
    ) -> io::Result<Self> {
        Ok(Self {
            file: nonblocking(pipe)?,
            deadline: Instant::now(),
            cancelled: Some(cancelled),
            bytes: 0,
            byte_budget: Some(byte_budget),
        })
    }

    pub fn read_some(&mut self, buffer: &mut [u8], deadline: Instant) -> io::Result<usize> {
        self.deadline = deadline;
        self.read(buffer)
    }
}

impl Read for LinuxReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            check_wait(self.deadline, self.cancelled.as_deref())?;
            let count = if let Some(budget) = self.byte_budget {
                let remaining = budget.saturating_sub(self.bytes);
                if remaining == 0 {
                    return Err(io::Error::other("LSP stream byte budget exceeded"));
                }
                buffer
                    .len()
                    .min(usize::try_from(remaining).unwrap_or(usize::MAX))
            } else {
                buffer.len()
            };
            match self.file.read(&mut buffer[..count]) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    ready(
                        &self.file,
                        libc::POLLIN,
                        self.deadline,
                        self.cancelled.as_deref(),
                    )?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Ok(count) => {
                    self.bytes += u64::try_from(count).unwrap_or(u64::MAX);
                    return Ok(count);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

pub struct LinuxWriter {
    file: File,
    cancelled: Option<Arc<AtomicBool>>,
    bytes: u64,
    byte_budget: Option<u64>,
}

impl LinuxWriter {
    pub(crate) fn from_pipe(
        pipe: OwnedFd,
        cancelled: Arc<AtomicBool>,
        byte_budget: u64,
    ) -> io::Result<Self> {
        Ok(Self {
            file: nonblocking(pipe)?,
            cancelled: Some(cancelled),
            bytes: 0,
            byte_budget: Some(byte_budget),
        })
    }

    pub(crate) fn write_control(&mut self, bytes: &[u8], deadline: Instant) -> io::Result<()> {
        if self
            .byte_budget
            .is_some_and(|budget| bytes.len() as u64 > budget.saturating_sub(self.bytes))
        {
            return Err(io::Error::other("helper control byte budget exceeded"));
        }
        self.bytes += bytes.len() as u64;
        self.write_bytes(bytes, deadline)
    }

    pub(crate) const fn set_byte_budget(&mut self, budget: u64) {
        self.byte_budget = Some(budget);
    }

    pub fn write_frame(
        &mut self,
        value: &Value,
        limits: FrameLimits,
        deadline: Instant,
    ) -> Result<()> {
        validate_limits(limits)?;
        check_wait(deadline, self.cancelled.as_deref())?;
        let mut body = LimitedBody {
            bytes: Vec::with_capacity(limits.body_bytes),
            limit: limits.body_bytes,
        };
        serde_json::to_writer(&mut body, value)?;
        check_wait(deadline, self.cancelled.as_deref())?;
        let header = format!("Content-Length: {}\r\n\r\n", body.bytes.len());
        if header.len() > limits.header_bytes {
            bail!("LSP header byte budget exceeded");
        }
        let frame_bytes = u64::try_from(header.len())? + u64::try_from(body.bytes.len())?;
        if self
            .byte_budget
            .is_some_and(|budget| frame_bytes > budget.saturating_sub(self.bytes))
        {
            bail!("LSP outbound stream byte budget exceeded");
        }
        self.bytes += frame_bytes;
        self.write_bytes(header.as_bytes(), deadline)?;
        self.write_bytes(&body.bytes, deadline)?;
        check_wait(deadline, self.cancelled.as_deref())?;
        Ok(())
    }

    fn write_bytes(&mut self, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
        while !bytes.is_empty() {
            check_wait(deadline, self.cancelled.as_deref())?;
            match self.file.write(bytes) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    ready(
                        &self.file,
                        libc::POLLOUT,
                        deadline,
                        self.cancelled.as_deref(),
                    )?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

struct LimitedBody {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for LimitedBody {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit - self.bytes.len() {
            return Err(io::Error::other("LSP outbound body byte budget exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn validate_limits(limits: FrameLimits) -> Result<()> {
    if limits.header_bytes == 0
        || limits.header_bytes > MAX_HEADER_BYTES
        || limits.body_bytes == 0
        || limits.body_bytes > MAX_BODY_BYTES
    {
        bail!("invalid Linux protocol limits");
    }
    Ok(())
}

fn nonblocking(descriptor: OwnedFd) -> io::Result<File> {
    let file = File::from(descriptor);
    // SAFETY: owned valid descriptor and command/argument match fcntl ABI.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

fn check_wait(deadline: Instant, cancelled: Option<&AtomicBool>) -> io::Result<()> {
    if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
        return Err(io::ErrorKind::ConnectionAborted.into());
    }
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(())
}

fn ready(
    file: &File,
    events: i16,
    deadline: Instant,
    cancelled: Option<&AtomicBool>,
) -> io::Result<()> {
    loop {
        check_wait(deadline, cancelled)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX);
        let timeout = if cancelled.is_some() {
            timeout.min(10)
        } else {
            timeout
        };
        let mut descriptor = libc::pollfd {
            fd: file.as_raw_fd(),
            events,
            revents: 0,
        };
        // SAFETY: initialized single descriptor; bounded timeout.
        let result = unsafe { libc::poll(&raw mut descriptor, 1, timeout) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result > 0 {
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::from_raw_os_error(libc::EBADF));
            }
            if events == libc::POLLOUT && descriptor.revents & (libc::POLLERR | libc::POLLHUP) != 0
            {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            return Ok(());
        }
    }
}
