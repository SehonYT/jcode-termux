//! Hard-timeout subprocess execution.
//!
//! Everything Android-related here is a subprocess (`termux-clipboard-get`,
//! `wl-paste`, `xclip`, `curl`, ...), and on Android those subprocesses hang
//! in ways a desktop never sees: the Termux:API helper blocks on an Android
//! permission dialog, or on an `am broadcast` that nobody answers, or the
//! addon is missing and the stub waits on a socket. A clipboard copy runs on
//! the UI thread, so an unbounded `wait()` freezes the whole TUI.
//!
//! This module therefore runs every child through
//!
//! ```text
//!   stdin: writer thread -> child -> (stdout, stderr): consumer threads
//!   main:  poll try_wait() until exit or deadline; on deadline kill + reap
//! ```
//!
//! Design notes that are easy to get wrong:
//!
//! * `Stdio::null()` for stdout/stderr would let a helper that prints a huge
//!   amount block forever on a full pipe, *before* the timeout could fire. We
//!   must read both pipes - but we must also bound what we keep, so both
//!   readers stop *storing* past [BoundedBuffer]'s limit while continuing to
//!   drain, which is what keeps the child unblocked.
//! * Polling beats `wait_with_output` because `wait_with_output` has no
//!   deadline: a hung child returns only when the child exits.
//! * On timeout the child is killed and reaped. An orphaned
//!   `termux-clipboard-get` would keep a wakelock and keep the CPU awake.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::{ClipboardError, Result};

/// Default hard timeout for a single subprocess call.
///
/// 2s is generous for a local IPC round trip (Termux:API's measured latency is
/// tens of milliseconds) and short enough that even two of them stacked in the
/// fallback chain stay close to one animation frame budget of user-visible lag.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a *successful* child is re-polled after the deadline before we
/// accept it as a clipboard owner that outlives us (`wl-copy`).
///
/// Only the Wayland/X11 owners use this; everything Android-related must exit.
const OWNER_SETTLE_GRACE: Duration = Duration::from_millis(300);

/// Upper bound on stderr we keep for diagnostics.
const STDERR_LIMIT: usize = 16 * 1024;

/// [STDERR_LIMIT] exposed so tests can assert the bound actually holds.
pub const STDERR_LIMIT_FOR_TESTS: usize = STDERR_LIMIT;

/// Upper bound on stdout we buffer.
///
/// A malicious or broken helper that writes endlessly must not OOM the phone
/// (Termux routinely runs with ~1.4 GB free). 16 MiB is far past any real
/// clipboard text payload, so overflow is recorded and reported rather than
/// silently truncating a legitimate paste.
const STDOUT_LIMIT: usize = 16 * 1024 * 1024;

/// Sleep between `try_wait` polls. Short enough that a fast helper (the
/// common case on Android, ~10-30ms) is not visibly delayed.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// A stdio channel for a child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioChannel {
    /// Child's standard input.
    Stdin,
    /// Child's standard output.
    Stdout,
    /// Child's standard error.
    Stderr,
}

impl StdioChannel {
    /// Stable name, for diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            StdioChannel::Stdin => "stdin",
            StdioChannel::Stdout => "stdout",
            StdioChannel::Stderr => "stderr",
        }
    }
}

/// What a child process did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutput {
    /// The child exited on its own within the timeout.
    Exited {
        /// Bytes captured from stdout, capped at [STDOUT_LIMIT].
        stdout: Vec<u8>,
        /// True when stdout exceeded [STDOUT_LIMIT] and was truncated.
        stdout_truncated: bool,
        /// Captured stderr, capped at [STDERR_LIMIT].
        stderr: String,
    },
    /// The child was still alive at the deadline, so it was killed and reaped.
    TimedOut {
        /// The budget that was exceeded.
        limit_ms: u64,
        /// What was written to stderr before the kill.
        stderr: String,
    },
    /// The child was still alive at the deadline but is a *clipboard owner*:
    /// `wl-copy`/`xclip` fork and stay resident to serve paste requests, so
    /// killing them would drop the selection. It is left running and detached.
    StillRunning {
        /// How long we watched before deciding it was an owner.
        observed_ms: u64,
        /// What was written to stderr before we detached.
        stderr: String,
    },
    /// The child exited non-zero.
    Failed {
        /// Raw `ExitStatus::code()`, or None if it died from a signal.
        code: Option<i32>,
        /// Captured stderr (usually the actual reason, e.g. "no DISPLAY").
        stderr: String,
    },
}

impl RunOutput {
    /// Whether the child exited zero.
    pub fn is_success(&self) -> bool {
        matches!(self, RunOutput::Exited { .. })
    }

    /// Bytes captured from stdout (empty unless the child exited).
    pub fn stdout(&self) -> &[u8] {
        match self {
            RunOutput::Exited { stdout, .. } => stdout,
            _ => &[],
        }
    }

    /// Human-readable stderr, with a synthetic note when it was empty.
    pub fn stderr_text(&self) -> String {
        let raw = match self {
            RunOutput::Exited { stderr, .. }
            | RunOutput::TimedOut { stderr, .. }
            | RunOutput::StillRunning { stderr, .. }
            | RunOutput::Failed { stderr, .. } => stderr.clone(),
        };
        let trimmed = raw.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
        match self {
            RunOutput::TimedOut { limit_ms, .. } => format!("timed out after {limit_ms}ms"),
            RunOutput::StillRunning { observed_ms, .. } => {
                format!("still running after {observed_ms}ms")
            }
            RunOutput::Failed { code: Some(c), .. } => format!("exit code {c}"),
            RunOutput::Failed { code: None, .. } => "killed by signal".to_string(),
            RunOutput::Exited { .. } => String::new(),
        }
    }
}

/// A process invocation, expressed independently of [std::process] so tests can
/// inspect and simulate it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Program name, resolved against PATH.
    pub program: String,
    /// Arguments, passed verbatim (no shell, so no quoting bug is possible).
    pub args: Vec<String>,
    /// Bytes to write to the child's stdin.
    pub stdin: Vec<u8>,
    /// Hard timeout for this call.
    pub timeout: Duration,
    /// Whether a child still alive at (timeout + grace) should be treated as a
    /// clipboard owner and detached instead of killed.
    pub allow_detach: bool,
}

impl CommandSpec {
    /// A spec for `program` with the given args and a 2s timeout.
    pub fn new(program: impl Into<String>, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            stdin: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            allow_detach: false,
        }
    }

    /// Attach stdin bytes.
    pub fn with_stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = bytes.into();
        self
    }

    /// Override the timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Mark this spec as one that may legitimately outlive the call.
    pub fn with_detach(mut self) -> Self {
        self.allow_detach = true;
        self
    }

    /// Milliseconds form of the timeout, for error payloads.
    pub fn timeout_ms(&self) -> u64 {
        u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX)
    }
}

/// How many bytes a caller wants captured from one stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundedRead {
    /// Maximum bytes retained. Further bytes are drained but discarded.
    pub limit: usize,
}

impl BoundedRead {
    /// Capture up to `limit` bytes.
    pub fn new(limit: usize) -> Self {
        Self { limit }
    }
}

/// The seam that makes this crate testable.
///
/// Production code uses [ProcessRunner]; tests use the scripted fakes in
/// `crate::tests`. Anything the selection logic needs from the outside world
/// (running a command, reading a file) goes through here, so a test can
/// simulate success, failure, timeout, hang, non-UTF8 output and a
/// megabyte-to-stderr process without spawning anything.
pub trait CommandRunner: Send + Sync {
    /// Run `spec` to completion, or kill it at its timeout.
    fn run(&self, spec: &CommandSpec) -> Result<RunOutput>;

    /// Read a file (used for `--file`-style helpers). Default: unsupported.
    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        let _ = path;
        Err(ClipboardError::Unsupported("file reads not supported"))
    }

    /// Whether a program exists on PATH. Default: assume it does not, so a
    /// runner that does not implement probing never claims a mechanism works.
    fn has_program(&self, program: &str) -> bool {
        let _ = program;
        false
    }

    /// The value of an environment variable. Default: assume unset.
    fn env_var(&self, key: &str) -> Option<String> {
        let _ = key;
        None
    }
}

/// The real runner: [std::process], with the timeout enforced by polling.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(&self, spec: &CommandSpec) -> Result<RunOutput> {
        use std::process::{Command, Stdio};

        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .stdin(if spec.stdin.is_empty() {
                Stdio::null()
            } else {
                Stdio::piped()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match command.spawn() {
            Ok(child) => child,
            // A missing helper (the normal case on Android without
            // termux-api) is reported as Failed with the OS error as the
            // reason, which is exactly what the caller wants to log once.
            Err(err) => {
                return Ok(RunOutput::Failed {
                    code: None,
                    stderr: format!("spawn {}: {err}", spec.program),
                });
            }
        };

        let stdin_handle = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Writer/readers own their handles so a hung child can never wedge the
        // poll loop: we never block on a pipe in the calling thread.
        let stdin_bytes = Arc::new(spec.stdin.clone());
        let stdin_thread = stdin_handle.map(|mut handle| {
            let bytes = Arc::clone(&stdin_bytes);
            std::thread::spawn(move || {
                let _ = handle.write_all(&bytes);
                // Dropping the handle closes the pipe, which is what tells a
                // filter-style helper (wl-copy, xclip -i) that input is done.
            })
        });
        let stdout_buf = spawn_bounded_reader(stdout, BoundedRead::new(STDOUT_LIMIT));
        let stderr_buf = spawn_bounded_reader(stderr, BoundedRead::new(STDERR_LIMIT));

        let deadline = Instant::now() + spec.timeout;
        let grace_deadline = deadline + OWNER_SETTLE_GRACE;

        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                // Losing the ability to reap is not worth panicking over.
                Err(_) => break None,
            }
            let now = Instant::now();
            if now >= deadline {
                // A clipboard owner (wl-copy/xclip) is *supposed* to still be
                // alive: it holds the selection until the next paste. Keep
                // watching for a short grace period; if it outlives that, leave
                // it running and report ownership.
                if spec.allow_detach {
                    if now >= grace_deadline {
                        break None;
                    }
                } else {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Some(thread) = stdin_thread {
                        let _ = thread.join();
                    }
                    let stderr = take_text(stderr_buf.snapshot());
                    return Ok(RunOutput::TimedOut {
                        limit_ms: spec.timeout_ms(),
                        stderr,
                    });
                }
            }
            std::thread::sleep(POLL_INTERVAL);
        };

        if let Some(thread) = stdin_thread {
            let _ = thread.join();
        }

        // Join the pipe readers *before* reading their buffers. `try_wait`
        // returning Some means the child is gone, but the reader threads may
        // still be draining what the kernel buffered; sampling the buffer
        // first is a race that intermittently drops a failing helper's stderr
        // (the actual reason, e.g. "no DISPLAY").
        match status {
            Some(status) if status.success() => {
                let (stdout, stdout_truncated) = stdout_buf.finish();
                let (stderr, _) = stderr_buf.finish();
                Ok(RunOutput::Exited {
                    stdout,
                    stdout_truncated,
                    stderr: lossy_text(stderr),
                })
            }
            Some(status) => {
                let (stderr, _) = stderr_buf.finish();
                Ok(RunOutput::Failed {
                    code: status.code(),
                    stderr: lossy_text(stderr),
                })
            }
            None => {
                // Either the child outlived its grace period as an owner, or
                // try_wait failed. Both mean "we stopped watching a live
                // process"; the caller only uses this for helpers where that
                // is success.
                if !spec.allow_detach {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                Ok(RunOutput::StillRunning {
                    observed_ms: spec.timeout_ms() + OWNER_SETTLE_GRACE.as_millis() as u64,
                    stderr: take_text(stderr_buf.snapshot()),
                })
            }
        }
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        std::fs::read(path).map_err(|err| ClipboardError::Failed(format!("read {path:?}: {err}")))
    }

    fn has_program(&self, program: &str) -> bool {
        which(program).is_some()
    }

    fn env_var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Resolve `program` on PATH, the way a shell would.
///
/// Deliberately does not spawn `command -v`: on Android that is a bash
/// builtin, so there is no binary to exec, and probing by execution would cost
/// a subprocess per copy. PATH is cached after the first lookup.
pub fn which(program: &str) -> Option<PathBuf> {
    use std::sync::OnceLock;
    static PATH_ENTRIES: OnceLock<Vec<PathBuf>> = OnceLock::new();

    if program.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(program);
        return path.is_file().then_some(path);
    }
    let entries = PATH_ENTRIES.get_or_init(|| {
        std::env::var_os("PATH")
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or_default()
    });
    for dir in entries {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Spawn a thread that drains `stream` into a bounded buffer.
fn spawn_bounded_reader<R: Read + Send + 'static>(
    stream: Option<R>,
    bound: BoundedRead,
) -> CapturedStream {
    let buffer = Arc::new(Mutex::new(BoundedBuffer::new(bound.limit)));
    let Some(mut stream) = stream else {
        return CapturedStream {
            buffer,
            reader: None,
        };
    };
    let sink = Arc::clone(&buffer);
    let reader = std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    // Lock scope is one chunk: the poll loop never contends
                    // with a long read, and a partial buffer is always
                    // readable for diagnostics.
                    if let Ok(mut guard) = sink.lock() {
                        guard.extend(&chunk[..read]);
                    }
                }
                Err(_) => break,
            }
        }
    });
    CapturedStream {
        buffer,
        reader: Some(reader),
    }
}

/// A bounded pipe reader plus the thread draining it.
///
/// The handle matters: after the child exits its pipe may still hold unread
/// bytes (the OS flushes the pipe on exit, but the *reader thread* has to
/// observe EOF and copy them into the buffer). Reading the buffer without
/// joining first is a race that shows up as intermittently empty stderr - i.e.
/// exactly the "boom" a failing helper printed being lost.
struct CapturedStream {
    buffer: Arc<Mutex<BoundedBuffer>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl CapturedStream {
    /// Wait for the reader thread to reach EOF, then take what it stored.
    fn finish(mut self) -> (Vec<u8>, bool) {
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        take_bytes(&self.buffer)
    }

    /// Take whatever has arrived so far, without waiting (used after a kill,
    /// where the reader may still be draining a pipe the child no longer
    /// holds).
    fn snapshot(&self) -> (Vec<u8>, bool) {
        take_bytes(&self.buffer)
    }
}

/// A byte sink that stops storing past its limit but keeps accepting input, so
/// the child never blocks on a full pipe.
#[derive(Debug)]
pub struct BoundedBuffer {
    bytes: Vec<u8>,
    truncated: bool,
    limit: usize,
}

impl BoundedBuffer {
    /// An empty buffer that retains at most `limit` bytes.
    pub fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            truncated: false,
            limit,
        }
    }

    /// Accept bytes, discarding anything past the limit.
    pub fn extend(&mut self, chunk: &[u8]) {
        let room = self.limit.saturating_sub(self.bytes.len());
        if room == 0 {
            self.truncated = self.truncated || !chunk.is_empty();
            return;
        }
        let keep = room.min(chunk.len());
        self.bytes.extend_from_slice(&chunk[..keep]);
        if keep < chunk.len() {
            self.truncated = true;
        }
    }

    /// The retained bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Whether anything was dropped.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Take the bytes captured by a reader thread (empty if it is still busy).
fn take_bytes(buffer: &Arc<Mutex<BoundedBuffer>>) -> (Vec<u8>, bool) {
    // A poisoned lock means a reader thread panicked mid-read; the bytes it
    // already stored are still valid, so recover rather than propagate.
    let guard = match buffer.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    (guard.bytes.clone(), guard.truncated)
}

/// Render diagnostics bytes as text (never fails, never panics on bad UTF-8).
fn lossy_text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Append the truncation marker when the capture hit its bound.
fn take_text(snapshot: (Vec<u8>, bool)) -> String {
    let (bytes, truncated) = snapshot;
    let mut text = lossy_text(bytes);
    if truncated {
        text.push_str(" [truncated]");
    }
    text
}
