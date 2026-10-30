//! The LSP stdio transport and definition primitives every semantic client
//! shares.
//!
//! rust-analyzer, ruby-lsp, and phpactor differ in how they are launched,
//! what they need before the first question, and which answers codemap
//! admits as edges. They do not differ in how bytes move: Content-Length
//! frames over stdin/stdout, JSON-RPC responses matched to requests by id,
//! server-initiated requests that must be answered for the protocol to stay
//! balanced, and `textDocument/definition` answers that are locations or
//! links. That layer lives here, once. Launch policy, initialization
//! parameters, the indexing wait, and edge admission stay in each language
//! module: they are the truth boundary, and they are not alike.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
#[path = "lsp_linux.rs"]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{LinuxLspConfig, LinuxLspLimits};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::schema::Span;

/// Every request waits this long for its answer before failing closed.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

// ───────────────────────── definitions ─────────────────────────

/// A resolved definition location: the URI and LSP start position of the
/// target. The end position is retained for range-based node mapping but
/// is not consumed by the current smallest-span finder.
#[derive(Debug, Clone)]
pub struct DefinitionLocation {
    pub uri: String,
    pub start_line: u32,
    pub start_character: u32,
    #[allow(dead_code)]
    pub end_line: u32,
    #[allow(dead_code)]
    pub end_character: u32,
}

/// One `textDocument/definition` question: where to ask, at 0-based
/// `line` and UTF-16 `character`.
#[derive(Debug, Clone)]
pub struct DefinitionRequest {
    pub rel_path: String,
    pub line: u32,
    pub character: u32,
}

/// Outcome of a single `textDocument/definition` request.
#[derive(Debug, Clone)]
pub enum DefinitionResult {
    /// One or more locations returned.
    Locations(Vec<DefinitionLocation>),
    /// The server returned no result (null or an empty list).
    Empty,
}

/// Parse a `textDocument/definition` result into locations. `server`
/// names the answering process in the error.
pub fn parse_definition_response(server: &str, result: &Value) -> Result<DefinitionResult> {
    if result.is_null() {
        return Ok(DefinitionResult::Empty);
    }
    let locations: Vec<DefinitionLocation> = match result {
        Value::Array(values) => values
            .iter()
            .map(loc_from_value)
            .collect::<Result<Vec<_>>>()?,
        Value::Object(_) => vec![loc_from_value(result)?],
        other => bail!("invalid {server} definition result: {other}"),
    };
    if locations.is_empty() {
        Ok(DefinitionResult::Empty)
    } else {
        Ok(DefinitionResult::Locations(locations))
    }
}

/// Extract a [`DefinitionLocation`] from a `Location` or `DefinitionLink`.
/// A link's `targetSelectionRange` (the declaration token) is preferred
/// over `targetRange`, which may begin at attributes outside the node.
pub fn loc_from_value(v: &Value) -> Result<DefinitionLocation> {
    let (uri, range) = if let Some(uri) = v.get("uri").and_then(Value::as_str) {
        (
            uri.to_string(),
            v.get("range")
                .context("definition Location omitted range")?,
        )
    } else {
        let uri = v
            .get("targetUri")
            .and_then(Value::as_str)
            .context("definition result omitted uri/targetUri")?;
        (
            uri.to_string(),
            v.get("targetSelectionRange")
                .or_else(|| v.get("targetRange"))
                .context("definition link omitted targetSelectionRange/targetRange")?,
        )
    };
    let start = range
        .get("start")
        .context("definition range omitted start")?;
    let end = range.get("end").context("definition range omitted end")?;
    Ok(DefinitionLocation {
        uri,
        start_line: lsp_field(start, "line")?,
        start_character: lsp_field(start, "character")?,
        end_line: lsp_field(end, "line")?,
        end_character: lsp_field(end, "character")?,
    })
}

/// Read a u32 field from an LSP position object.
fn lsp_field(pos: &Value, field: &str) -> Result<u32> {
    pos.get(field)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .with_context(|| format!("definition position has invalid {field}"))
}

/// A source of definition answers that several worker threads may drive.
/// A server that answers concurrently is one source shared by every lane;
/// a server that answers one request at a time is one source per lane.
pub trait DefinitionSource: Sync {
    fn definition(&self, request: &DefinitionRequest) -> Result<DefinitionResult>;
}

/// Resolve many definitions over `lanes`, one worker thread per lane,
/// returning results **in request order** so the caller's fact ordering,
/// and therefore the snapshot's bytes, cannot depend on which worker
/// finished first. A single failure fails the batch: a partially resolved
/// run would emit a snapshot whose missing call edges look exactly like
/// resolved-to-nothing.
pub fn definition_batch(
    requests: &[DefinitionRequest],
    lanes: &[&dyn DefinitionSource],
) -> Result<Vec<DefinitionResult>> {
    if lanes.is_empty() {
        bail!("definition batch needs at least one lane");
    }
    let mut results: Vec<Option<Result<DefinitionResult>>> = Vec::new();
    results.resize_with(requests.len(), || None);
    let next = AtomicUsize::new(0);
    let slots = Mutex::new(results);

    thread::scope(|scope| {
        for lane in lanes {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(request) = requests.get(index) else {
                        return;
                    };
                    let outcome = lane.definition(request);
                    if let Ok(mut slots) = slots.lock() {
                        slots[index] = Some(outcome);
                    }
                }
            });
        }
    });

    slots
        .into_inner()
        .map_err(|_| anyhow::anyhow!("definition batch result slots are poisoned"))?
        .into_iter()
        .enumerate()
        .map(|(index, slot)| {
            let request = &requests[index];
            slot.with_context(|| {
                format!(
                    "definition request {index} ({}:{}) produced no result",
                    request.rel_path,
                    request.line + 1
                )
            })?
            .with_context(|| {
                format!(
                    "definition request failed for {}:{}",
                    request.rel_path,
                    request.line + 1
                )
            })
        })
        .collect()
}

// ───────────────────────── transport ─────────────────────────

/// Answers a server-initiated request (a message carrying both `id` and
/// `method`) with a result, or declines to answer. Runs on the reader
/// thread, so the protocol stays balanced whatever the callers are doing.
pub type ServerRequestHook = Box<dyn Fn(&Value) -> Option<Value> + Send + Sync>;

/// Responses awaiting their caller, keyed by JSON-RPC id. The reader
/// thread owns the only view of the server's output, so it must deliver
/// each response to the thread that asked for it.
type Pending = Mutex<HashMap<i64, mpsc::Sender<Value>>>;

/// A running language server on stdio. Requests may be issued from several
/// threads at once; interior mutability is what lets [`thread::scope`] hand
/// one transport to every worker.
pub struct Transport {
    server: &'static str,
    child: ChildOwner,
    stdin: ProtocolInput,
    /// Server-initiated messages, notifications and requests alike, which
    /// no caller is waiting on by id. Indexing waits drain this.
    unsolicited: Mutex<mpsc::Receiver<Value>>,
    pending: Arc<Pending>,
    next_id: AtomicI64,
}

enum ChildOwner {
    Standard(Mutex<Child>),
    #[cfg(target_os = "linux")]
    Linux(Arc<linux::LinuxControl>),
}

enum ProtocolInput {
    Standard(Arc<Mutex<ChildStdin>>),
    #[cfg(target_os = "linux")]
    Linux(Arc<Mutex<crate::linux_io::LinuxWriter>>),
}

impl Transport {
    /// Start `command` with piped stdin and stdout and begin reading its
    /// frames. The handshake is the caller's: languages differ in what
    /// `initialize` must say.
    pub fn spawn(
        server: &'static str,
        command: &mut Command,
        hook: Option<ServerRequestHook>,
    ) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn {server}"))?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;

        let stdin = Arc::new(Mutex::new(stdin));
        let (tx, rx) = mpsc::channel::<Value>();
        let pending: Arc<Pending> = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = Arc::clone(&pending);
        let reader_stdin = Arc::clone(&stdin);
        thread::spawn(move || {
            reader_loop(stdout, &reader_pending, &tx, hook.as_ref(), &reader_stdin);
        });

        Ok(Self {
            server,
            child: ChildOwner::Standard(Mutex::new(child)),
            stdin: ProtocolInput::Standard(stdin),
            unsolicited: Mutex::new(rx),
            pending,
            next_id: AtomicI64::new(1),
        })
    }

    /// Send a request and wait for its response.
    pub fn request(&self, method: &str, params: &Value) -> Result<Value> {
        let outcome = self.request_until(method, params, self.deadline());
        self.finish(outcome)
    }

    fn request_until(
        &self,
        method: &str,
        params: &Value,
        deadline: Option<Instant>,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        // Register before sending: the server can answer faster than this
        // thread reaches the receive, and an unregistered response would be
        // delivered to the unsolicited channel and lost.
        let (tx, rx) = mpsc::channel::<Value>();
        #[cfg(target_os = "linux")]
        if let ChildOwner::Linux(control) = &self.child {
            control.verify_live(deadline.context("missing Linux request deadline")?)?;
        }
        {
            let mut pending = lock_until(&self.pending, deadline)?;
            #[cfg(target_os = "linux")]
            if let ChildOwner::Linux(control) = &self.child {
                if pending.len() >= control.limits.pending {
                    bail!("LSP pending request budget exceeded");
                }
            }
            pending.insert(id, tx);
        }

        let sent = self.send_until(
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }),
            deadline,
        );
        let outcome = match sent {
            Ok(()) => wait_for_response(
                &rx,
                self.server,
                method,
                deadline.map_or(REQUEST_TIMEOUT, |deadline| {
                    deadline.saturating_duration_since(Instant::now())
                }),
            ),
            Err(error) => Err(error),
        };
        // Always deregister, so a timed-out or failed request cannot leak
        // an entry the reader would keep trying to satisfy.
        if let Ok(mut pending) = lock_until(&self.pending, deadline) {
            pending.remove(&id);
        }
        if outcome.is_ok() && deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            bail!("LSP request deadline exceeded");
        }
        outcome
    }

    /// Send a notification.
    pub fn notify(&self, method: &str, params: &Value) -> Result<()> {
        let outcome = self.send_until(
            &json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            }),
            self.deadline(),
        );
        self.finish(outcome)
    }

    /// The channel of server-initiated messages, for waits that read the
    /// server's own progress signals. Safe to hold when no request is
    /// pending on this thread.
    pub fn unsolicited(&self) -> Result<MutexGuard<'_, mpsc::Receiver<Value>>> {
        self.unsolicited
            .lock()
            .map_err(|_| anyhow::anyhow!("{} unsolicited channel is poisoned", self.server))
    }

    pub fn process_id(&self) -> Result<u32> {
        match &self.child {
            ChildOwner::Standard(child) => {
                let mut child = lock_until(child, None)?;
                if child.try_wait()?.is_some() {
                    bail!("LSP process has exited");
                }
                Ok(child.id())
            }
            #[cfg(target_os = "linux")]
            ChildOwner::Linux(control) => {
                self.finish(control.verify_live(Instant::now() + REQUEST_TIMEOUT))
            }
        }
    }

    /// Cleanly shut the server down; the process is killed regardless.
    pub fn shutdown(&self) {
        let _ = self.request("shutdown", &json!({}));
        let _ = self.notify("exit", &json!({}));
        self.kill();
    }

    fn send_until(&self, value: &Value, deadline: Option<Instant>) -> Result<()> {
        match &self.stdin {
            ProtocolInput::Standard(stdin) => {
                let mut stdin = lock_until(stdin, deadline)?;
                write_message(&mut *stdin, value)
                    .with_context(|| format!("write to {}", self.server))
            }
            #[cfg(target_os = "linux")]
            ProtocolInput::Linux(stdin) => {
                let ChildOwner::Linux(control) = &self.child else {
                    bail!("invalid Linux transport owner");
                };
                control.check()?;
                let deadline = deadline.context("missing Linux write deadline")?;
                control.write(stdin, value, deadline)
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    const fn deadline(&self) -> Option<Instant> {
        match &self.child {
            ChildOwner::Standard(_) => None,
        }
    }

    #[cfg(target_os = "linux")]
    fn deadline(&self) -> Option<Instant> {
        match &self.child {
            ChildOwner::Standard(_) => None,
            #[cfg(target_os = "linux")]
            ChildOwner::Linux(control) => Some(Instant::now() + control.limits.request_duration()),
        }
    }

    #[cfg(not(target_os = "linux"))]
    const fn finish<T>(&self, outcome: Result<T>) -> Result<T> {
        match &self.child {
            ChildOwner::Standard(_) => outcome,
        }
    }

    #[cfg(target_os = "linux")]
    fn finish<T>(&self, outcome: Result<T>) -> Result<T> {
        #[cfg(target_os = "linux")]
        if let ChildOwner::Linux(control) = &self.child {
            match &outcome {
                Ok(_) => {
                    if let Err(error) = control.check() {
                        control.fail(&error.to_string());
                        return Err(error);
                    }
                }
                Err(error) => {
                    control.fail(&error.to_string());
                    if let Err(cause) = control.check() {
                        return outcome.context(cause);
                    }
                }
            }
        }
        outcome
    }

    fn kill(&self) {
        match &self.child {
            ChildOwner::Standard(child) => {
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            #[cfg(target_os = "linux")]
            ChildOwner::Linux(control) => control.fail("LSP transport closed"),
        }
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.kill();
        #[cfg(target_os = "linux")]
        if let ChildOwner::Linux(control) = &self.child {
            let _ = control.join();
        }
    }
}

fn lock_until<T>(mutex: &Mutex<T>, deadline: Option<Instant>) -> Result<MutexGuard<'_, T>> {
    if let Some(deadline) = deadline {
        loop {
            if Instant::now() >= deadline {
                bail!("LSP mutex deadline exceeded");
            }
            match mutex.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(std::sync::TryLockError::Poisoned(_)) => bail!("LSP mutex is poisoned"),
                Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(1)),
            }
        }
    }
    mutex
        .lock()
        .map_err(|_| anyhow::anyhow!("LSP mutex is poisoned"))
}

/// Await the response routed to this request's private channel. The
/// reader matches by id, so nothing arrives here but this request's
/// answer.
fn wait_for_response(
    rx: &mpsc::Receiver<Value>,
    server: &str,
    method: &str,
    timeout: Duration,
) -> Result<Value> {
    match rx.recv_timeout(timeout) {
        Ok(msg) => {
            if let Some(error) = msg.get("error") {
                bail!("{server} {method} error: {error}");
            }
            msg.get("result")
                .cloned()
                .with_context(|| format!("{server} {method} response omitted result"))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            bail!("{server} request {method} timed out after {timeout:?}")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            bail!("{server} stdout reader disconnected")
        }
    }
}

/// Read the server's output forever, routing each message to whoever wants
/// it.
///
/// A *response* carries an `id` and no `method`; it belongs to the request
/// registered under that id. A *server request* carries both; the hook may
/// answer it, and the answer is written back before the message is
/// forwarded. Everything the callers are not waiting on by id goes to the
/// unsolicited channel.
pub fn reader_loop<R: Read, W: Write>(
    stdout: R,
    pending: &Pending,
    unsolicited: &mpsc::Sender<Value>,
    hook: Option<&ServerRequestHook>,
    stdin: &Mutex<W>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let Ok(Some(value)) = read_message(&mut reader) else {
            return;
        };
        let routed = route_message(
            value,
            pending,
            None,
            |request| {
                if let (Some(hook), Some(id)) = (hook, request.get("id")) {
                    if let Some(result) = hook(request) {
                        if let Ok(mut stdin) = stdin.lock() {
                            let _ = write_message(
                                &mut *stdin,
                                &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                            );
                        }
                    }
                }
                Ok(())
            },
            |value| unsolicited.send(value).map_err(Into::into),
        );
        if routed.is_err() {
            return;
        }
    }
}

fn route_message(
    value: Value,
    pending: &Pending,
    deadline: Option<Instant>,
    answer: impl FnOnce(&Value) -> Result<()>,
    enqueue: impl FnOnce(Value) -> Result<()>,
) -> Result<()> {
    let strict = deadline.is_some();
    if strict && (!value.is_object() || value.get("jsonrpc").and_then(Value::as_str) != Some("2.0"))
    {
        bail!("invalid LSP message envelope");
    }
    let is_response = value.get("method").is_none();
    if strict && !is_response && value.get("method").and_then(Value::as_str).is_none() {
        bail!("invalid LSP method");
    }
    let response_id = is_response
        .then(|| value.get("id").and_then(Value::as_i64))
        .flatten();
    if strict && is_response && response_id.is_none() {
        bail!("invalid LSP response id");
    }
    if strict && is_response && (value.get("result").is_some() == value.get("error").is_some()) {
        bail!("invalid LSP response result or error");
    }
    let waiting = if let Some(id) = response_id {
        lock_until(pending, deadline)?.remove(&id)
    } else {
        None
    };
    if let Some(sender) = waiting {
        let delivered = sender.send(value);
        if strict {
            delivered.context("LSP response caller disconnected")?;
        }
        return Ok(());
    }
    if strict && is_response {
        bail!("unknown or duplicate LSP response id");
    }
    if !is_response {
        answer(&value)?;
    }
    enqueue(value)
}

/// Optional byte limits for the shared frame decoder.
#[derive(Clone, Copy)]
pub struct FrameLimits {
    pub header_bytes: usize,
    pub body_bytes: usize,
}

/// Read one LSP frame (headers + body). Returns `Ok(None)` on clean EOF.
pub fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<Value>> {
    read_message_with_limits(reader, None)
}

pub fn read_message_with_limits<R: BufRead>(
    reader: &mut R,
    limits: Option<FrameLimits>,
) -> Result<Option<Value>> {
    if limits.is_some_and(|limits| limits.header_bytes == 0 || limits.body_bytes == 0) {
        bail!("invalid LSP frame limits");
    }
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    let mut header_bytes = 0;
    loop {
        line.clear();
        let n = if let Some(limits) = limits {
            let remaining = limits.header_bytes.saturating_sub(header_bytes);
            if remaining == 0 {
                bail!("LSP header byte budget exceeded");
            }
            let n = reader
                .by_ref()
                .take(u64::try_from(remaining)? + 1)
                .read_line(&mut line)?;
            if n > remaining {
                bail!("LSP header byte budget exceeded");
            }
            n
        } else {
            reader.read_line(&mut line)?
        };
        if n == 0 {
            if limits.is_some() && header_bytes != 0 {
                bail!("truncated LSP header");
            }
            return Ok(None);
        }
        if limits.is_some() {
            header_bytes += n;
        }
        if limits.is_some() && !line.ends_with('\n') {
            bail!("truncated LSP header");
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            if limits.is_some() && content_length.is_some() {
                bail!("duplicate LSP Content-Length");
            }
            content_length = rest.trim().parse().ok();
            if limits.is_some() && content_length.is_none() {
                bail!("invalid LSP Content-Length");
            }
        }
    }
    let len = content_length.context("missing Content-Length")?;
    if limits.is_some_and(|limits| len > limits.body_bytes) {
        bail!("LSP body byte budget exceeded");
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let value: Value = serde_json::from_slice(&body)?;
    Ok(Some(value))
}

/// Serialize one LSP frame: `Content-Length: <bytes>\r\n\r\n<body>`.
pub fn write_message<W: Write>(writer: &mut W, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value).context("serialize LSP message")?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len()).context("write LSP header")?;
    writer.write_all(&body).context("write LSP body")?;
    writer.flush().context("flush LSP message")
}

// ───────────────────────── probes ─────────────────────────

/// Run a probe with a hard deadline: a stalled tool must not hang
/// `--calls=required`. The command names the program to run directly.
pub fn bounded_probe(
    mut command: Command,
    timeout: Duration,
) -> std::io::Result<std::process::Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let description = format!("{command:?}");

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output(),
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{description} exceeded {timeout:?}"),
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
    }
}

// ───────────────────────── URIs and positions ─────────────────────────

/// The URI of a workspace document, from the root URI and a repository
/// relative path.
pub fn document_uri(root_uri: &str, rel_path: &str) -> String {
    format!("{root_uri}/{}", encode_uri_path(rel_path))
}

/// Convert a `file://` URI to a filesystem path. Percent-escapes are
/// decoded so a `#` or space in a real path (`file:///tmp/app%231`) maps
/// back to the actual filename rather than a URI-fragment parse.
pub fn path_from_uri(uri: &str) -> String {
    let path = uri
        .strip_prefix("file://")
        .map_or_else(|| uri.to_string(), str::to_string);
    decode_uri_path(&path)
}

/// Convert a filesystem path to a `file://` URI. Reserved characters
/// (`#`, `?`, `%`, space) are percent-encoded so the URI is unambiguous
/// when concatenated with a relative path or parsed by an LSP server.
pub fn uri_from_path(path: &Path) -> String {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    format!("file://{}", encode_uri_path(&abs.to_string_lossy()))
}

/// Percent-encode a relative path for use as the tail of a `file://` URI.
/// `/` and `.` stay literal; `#`, `?`, `%`, and space are escaped so the
/// document URI never collides with a fragment/query delimiter.
pub fn encode_uri_path(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(path.len());
    for &b in path.as_bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'/' | b'.'
                    | b'-'
                    | b'_'
                    | b'~'
                    | b':'
                    | b'@'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
            );
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0F) as usize] as char);
        }
    }
    out
}

/// Decode percent-escapes (`%XX`) in a URI path back to bytes. Invalid or
/// truncated escapes are left literal rather than guessed.
fn decode_uri_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Convert a byte offset in `source` to a 0-based (line, `utf16_character`)
/// LSP position.
pub fn byte_offset_to_lsp_pos(source: &[u8], byte_offset: usize) -> (u32, u32) {
    let mut line = 0u32;
    let mut last_nl = 0usize;
    let end = byte_offset.min(source.len());
    for (i, &b) in source.iter().take(end).enumerate() {
        if b == b'\n' {
            line += 1;
            last_nl = i + 1;
        }
    }
    let line_bytes = &source[last_nl..end];
    let character = utf16_len(line_bytes) as u32;
    (line, character)
}

/// Count UTF-16 code units in a byte slice (treating it as UTF-8).
fn utf16_len(bytes: &[u8]) -> usize {
    let s = std::str::from_utf8(bytes).unwrap_or("");
    s.chars().map(char::len_utf16).sum()
}

/// Convert an LSP (line, character) back to a byte offset using `source`.
pub fn lsp_pos_to_byte(source: &[u8], line: u32, character: u32) -> usize {
    let mut current_line = 0u32;
    let mut line_start = 0usize;
    for (i, &b) in source.iter().enumerate() {
        if current_line == line {
            let mut utf16 = 0u32;
            let mut j = line_start;
            while j < source.len() && utf16 < character {
                let s = &source[j..];
                if let Ok(sub) = std::str::from_utf8(s) {
                    if let Some(ch) = sub.chars().next() {
                        utf16 += ch.len_utf16() as u32;
                        j += ch.len_utf8();
                        continue;
                    }
                }
                break;
            }
            return line_start + j - line_start;
        }
        if b == b'\n' {
            current_line += 1;
            line_start = i + 1;
        }
    }
    source.len()
}

/// A spatial index mapping (`rel_path`, byte offset) → node id, for
/// resolving a definition location to an emitted in-repository node.
pub struct NodeIndex {
    by_path: HashMap<String, Vec<(usize, usize, String)>>,
}

impl NodeIndex {
    /// Build from node provenance. `entries` is (`rel_path`, span, `node_id`).
    pub fn build(entries: impl IntoIterator<Item = (String, Span, String)>) -> Self {
        let mut by_path: HashMap<String, Vec<(usize, usize, String)>> = HashMap::new();
        for (rel_path, span, node_id) in entries {
            by_path
                .entry(rel_path)
                .or_default()
                .push((span.start_byte, span.end_byte, node_id));
        }
        for v in by_path.values_mut() {
            v.sort_by_key(|(start, _, _)| *start);
        }
        Self { by_path }
    }

    /// Find the smallest-span node in `rel_path` whose span contains
    /// `byte_offset`. Returns the node id, or `None` if no node contains it.
    pub fn find(&self, rel_path: &str, byte_offset: usize) -> Option<&str> {
        let entries = self.by_path.get(rel_path)?;
        let mut best: Option<(&str, usize)> = None;
        for (start, end, id) in entries {
            if *start <= byte_offset && byte_offset < *end {
                let span_len = *end - *start;
                match best {
                    None => best = Some((id.as_str(), span_len)),
                    Some((_, bl)) if span_len < bl => best = Some((id.as_str(), span_len)),
                    _ => {}
                }
            }
        }
        best.map(|(id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(value: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(value).expect("serialize");
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend(body);
        out
    }

    fn location(uri: &str, line: u32, character: u32) -> Value {
        json!({
            "uri": uri,
            "range": {
                "start": { "line": line, "character": character },
                "end": { "line": line, "character": character + 1 }
            }
        })
    }

    #[test]
    fn write_message_frames_with_content_length() {
        let mut buf: Vec<u8> = Vec::new();
        write_message(
            &mut buf,
            &json!({"jsonrpc": "2.0", "id": 1, "result": null}),
        )
        .expect("write");
        let text = String::from_utf8_lossy(&buf);
        let (header, body) = text.split_once("\r\n\r\n").expect("frame split");
        assert_eq!(header, format!("Content-Length: {}", body.len()));
    }

    #[test]
    fn read_message_round_trips_framed_payloads_and_rejects_bad_frames() {
        let mut stream: Vec<u8> = Vec::new();
        stream.extend(frame(&json!({"jsonrpc": "2.0", "id": 1, "result": [1, 2]})));
        stream.extend(frame(&json!({"jsonrpc": "2.0", "method": "$/progress"})));
        let mut reader = BufReader::new(&stream[..]);
        assert_eq!(
            read_message(&mut reader)
                .expect("frame 1")
                .expect("payload 1")["result"],
            json!([1, 2])
        );
        assert_eq!(
            read_message(&mut reader)
                .expect("frame 2")
                .expect("payload 2")["method"],
            json!("$/progress")
        );
        assert_eq!(read_message(&mut reader).expect("clean EOF"), None);

        let mut reader = BufReader::new(&b"X-Other: 1\r\n\r\n{}"[..]);
        assert!(
            read_message(&mut reader).is_err(),
            "a frame without Content-Length is refused"
        );
        let mut reader = BufReader::new(&b"Content-Length: 50\r\n\r\n{}"[..]);
        assert!(
            read_message(&mut reader).is_err(),
            "a truncated body is refused"
        );
    }

    /// The routing property concurrency depends on: every response reaches
    /// the caller that asked for it, whatever order the server answers in,
    /// nothing that is not a response is mistaken for one, and a server
    /// request is answered by the hook before anyone else sees it.
    #[test]
    fn the_reader_routes_responses_answers_server_requests_and_forwards_the_rest() {
        let stream = [
            frame(&json!({"id": 2, "result": "second"})),
            frame(&json!({"method": "$/progress", "params": {}})),
            frame(&json!({"id": 99, "method": "window/workDoneProgress/create", "params": {"token": "t"}})),
            frame(&json!({"id": 1, "result": "first"})),
        ]
        .concat();

        let (tx1, rx1) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();
        let pending: Pending = Mutex::new(HashMap::from([(1, tx1), (2, tx2)]));
        let (unsolicited_tx, unsolicited_rx) = mpsc::channel();
        let stdin: Mutex<Vec<u8>> = Mutex::new(Vec::new());
        let hook: ServerRequestHook = Box::new(|request| {
            (request["method"] == "window/workDoneProgress/create").then(Value::default)
        });

        reader_loop(
            std::io::Cursor::new(stream),
            &pending,
            &unsolicited_tx,
            Some(&hook),
            &stdin,
        );

        assert_eq!(
            rx1.recv().expect("caller 1 answered")["result"],
            json!("first")
        );
        assert_eq!(
            rx2.recv().expect("caller 2 answered")["result"],
            json!("second")
        );
        let forwarded: Vec<Value> = unsolicited_rx.try_iter().collect();
        assert_eq!(
            forwarded.len(),
            2,
            "the notification and the server request are forwarded: {forwarded:?}"
        );
        assert!(
            pending.lock().expect("pending").is_empty(),
            "answered requests are deregistered"
        );

        let sent = stdin.into_inner().expect("stdin");
        let mut reader = BufReader::new(&sent[..]);
        let reply = read_message(&mut reader).expect("reply").expect("payload");
        assert_eq!(reply["id"], json!(99));
        assert_eq!(reply["result"], Value::Null);
        assert!(
            read_message(&mut reader).expect("eof").is_none(),
            "one reply only"
        );
    }

    #[test]
    fn a_response_with_an_error_a_missing_result_or_no_reader_is_a_failure() {
        let (tx, rx) = mpsc::channel();
        tx.send(json!({"id": 7, "error": {"code": -1, "message": "boom"}}))
            .expect("send");
        assert!(wait_for_response(&rx, "server", "definition", Duration::from_millis(10)).is_err());

        let (tx, rx) = mpsc::channel();
        tx.send(json!({"id": 8})).expect("send");
        let error = wait_for_response(&rx, "server", "definition", Duration::from_millis(10))
            .expect_err("no result");
        assert!(error.to_string().contains("omitted result"), "{error:#}");

        let (tx, rx) = mpsc::channel::<Value>();
        drop(tx);
        assert!(wait_for_response(&rx, "server", "definition", Duration::from_millis(10)).is_err());

        let (_tx, rx) = mpsc::channel::<Value>();
        let error = wait_for_response(&rx, "server", "definition", Duration::from_millis(20))
            .expect_err("nothing arrives");
        assert!(error.to_string().contains("timed out"), "{error:#}");
    }

    #[test]
    fn definition_response_preserves_empty_and_ambiguous_results() {
        assert!(matches!(
            parse_definition_response("server", &Value::Null).expect("null response"),
            DefinitionResult::Empty
        ));
        let result = parse_definition_response(
            "server",
            &json!([
                location("file:///repo/a.rs", 1, 2),
                location("file:///repo/b.rs", 3, 4)
            ]),
        )
        .expect("multiple definitions");
        assert!(
            matches!(result, DefinitionResult::Locations(ref locations) if locations.len() == 2)
        );
    }

    #[test]
    fn definition_link_uses_target_selection_range() {
        let location = loc_from_value(&json!({
            "targetUri": "file:///repo/a.rs",
            "targetRange": {
                "start": { "line": 1, "character": 0 },
                "end": { "line": 5, "character": 1 }
            },
            "targetSelectionRange": {
                "start": { "line": 3, "character": 8 },
                "end": { "line": 3, "character": 14 }
            }
        }))
        .expect("definition link");
        assert_eq!((location.start_line, location.start_character), (3, 8));
    }

    #[test]
    fn malformed_definition_response_fails_closed() {
        let error = parse_definition_response(
            "server",
            &json!({ "uri": "file:///repo/a.rs", "range": { "start": { "line": 0, "character": 0 } } }),
        )
        .expect_err("missing range end must fail");
        assert!(error.to_string().contains("omitted end"), "{error:#}");
    }

    struct Echo;

    impl DefinitionSource for Echo {
        fn definition(&self, request: &DefinitionRequest) -> Result<DefinitionResult> {
            if request.rel_path == "boom" {
                bail!("no such file");
            }
            Ok(DefinitionResult::Locations(vec![DefinitionLocation {
                uri: format!("file:///{}", request.rel_path),
                start_line: request.line,
                start_character: request.character,
                end_line: request.line,
                end_character: request.character,
            }]))
        }
    }

    #[test]
    fn a_batch_answers_in_request_order_over_every_lane_and_fails_as_a_whole() {
        let requests: Vec<DefinitionRequest> = (0..50)
            .map(|i| DefinitionRequest {
                rel_path: format!("f{i}.rs"),
                line: i,
                character: 0,
            })
            .collect();
        let lanes: Vec<&dyn DefinitionSource> = vec![&Echo, &Echo, &Echo];
        let answers = definition_batch(&requests, &lanes).expect("batch");
        assert_eq!(answers.len(), 50);
        for (i, answer) in answers.iter().enumerate() {
            let DefinitionResult::Locations(locations) = answer else {
                panic!("answer {i} is empty");
            };
            assert_eq!(locations[0].uri, format!("file:///f{i}.rs"));
        }

        let mut failing = requests;
        failing[7].rel_path = "boom".into();
        let error = definition_batch(&failing, &lanes).expect_err("one failure fails the batch");
        assert!(error.to_string().contains("boom:8"), "{error:#}");

        assert!(definition_batch(&[], &[]).is_err(), "no lanes is an error");
    }

    #[test]
    fn hung_probe_is_killed_within_the_deadline() {
        let start = std::time::Instant::now();
        let mut command = Command::new("sleep");
        command.arg("30");
        let error = bounded_probe(command, Duration::from_millis(200)).expect_err("times out");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn byte_offset_to_lsp_pos_ascii() {
        let src = b"fn main() {\n    foo();\n}\n";
        assert_eq!(byte_offset_to_lsp_pos(src, 16), (1, 4));
        assert_eq!(lsp_pos_to_byte(src, 1, 4), 16);
    }

    #[test]
    fn node_index_finds_smallest_containing() {
        let span = Span::from_ts(0, 20, (0, 0), (0, 20));
        let idx = NodeIndex::build([("a.rs".to_string(), span, "rust:type:a.rs#Foo".to_string())]);
        assert_eq!(idx.find("a.rs", 5), Some("rust:type:a.rs#Foo"));
        assert_eq!(idx.find("a.rs", 25), None);
    }

    #[test]
    fn uri_paths_with_special_characters_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let special = dir.path().join("app#1 x");
        std::fs::create_dir_all(&special).expect("mkdir");
        let uri = uri_from_path(&special);
        assert!(uri.ends_with("app%231%20x"), "{uri}");
        assert_eq!(
            path_from_uri(&uri),
            special.canonicalize().expect("canonical").to_string_lossy()
        );
        assert_eq!(
            document_uri("file:///r", "lib/a b.rb"),
            "file:///r/lib/a%20b.rb"
        );
    }
}

#[cfg(test)]
mod bounded_framing_compatibility {
    use super::*;
    #[test]
    fn legacy_decoder_behavior_stays_separate_from_bounded_policy() {
        let bytes = b"Content-Length: 3\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(read_message(&mut &bytes[..]).unwrap(), Some(json!({})));
        assert!(
            read_message_with_limits(
                &mut &bytes[..],
                Some(FrameLimits {
                    header_bytes: 128,
                    body_bytes: 1024
                })
            )
            .is_err()
        );
        let incomplete = b"Content-Length: 2";
        assert_eq!(read_message(&mut &incomplete[..]).unwrap(), None);
        assert!(
            read_message_with_limits(
                &mut &incomplete[..],
                Some(FrameLimits {
                    header_bytes: 128,
                    body_bytes: 1024
                })
            )
            .is_err()
        );
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../toolchain/linux/protocol_expectations.json"
        ))
        .unwrap();
        assert_eq!(oracle["expected"]["legacy_framing_unchanged"], "accept");
    }
}
