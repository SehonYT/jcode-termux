//! Shared support for the mobile/adversarial test suite.
//!
//! Everything here is designed so that a test can never hang the suite: every
//! subprocess wait goes through `run_command_with_timeout`, which kills the child
//! and fails the test instead of blocking forever. That matters because the very
//! behaviours under test (hung subprocesses, unbounded reads) are ones that would
//! otherwise wedge CI.

#![allow(dead_code)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Default wall-clock budget for one subprocess in this suite.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of running the CLI under a timeout.
pub struct RunResult {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// True when the child had to be killed because it exceeded the timeout.
    pub timed_out: bool,
    pub elapsed: Duration,
}

/// A scratch environment for one test: an isolated HOME/JCODE_HOME so tests
/// never touch the real state, and so they can run in parallel.
pub struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    pub fn new() -> Self {
        Self { dir: tempfile::tempdir().expect("create sandbox") }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    pub fn jcode_home(&self) -> PathBuf {
        self.dir.path().join("jcode-home")
    }

    /// Write `content` to a file inside the sandbox, creating parents.
    pub fn write_file(&self, rel: &str, content: &[u8]) -> PathBuf {
        let p = self.dir.path().join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(&p, content).expect("write sandbox file");
        p
    }

    /// Build a `Command` for the real CLI binary with a hermetic environment.
    pub fn command(&self, args: &[&str]) -> Command {
        std::fs::create_dir_all(self.home()).ok();
        std::fs::create_dir_all(self.jcode_home()).ok();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_jcode"));
        cmd.current_dir(self.dir.path())
            .env_clear()
            .env("HOME", self.home())
            .env("JCODE_HOME", self.jcode_home())
            .env("JCODE_NO_TELEMETRY", "1")
            .env("NO_COLOR", "1")
            // Keep the terminal deterministic; some paths sniff TERM.
            .env("TERM", "dumb")
            .args(args);
        cmd
    }

    /// Run the CLI, capturing output, killing it if it exceeds `timeout`.
    pub fn run(&self, args: &[&str]) -> RunResult {
        self.run_with_timeout(args, DEFAULT_TIMEOUT)
    }

    pub fn run_with_timeout(&self, args: &[&str], timeout: Duration) -> RunResult {
        let mut cmd = self.command(args);
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        run_command_with_timeout(cmd, timeout)
    }
}

/// Spawn `cmd` and enforce `timeout`.
///
/// Output is drained on dedicated threads so a child that fills a pipe buffer
/// while we are still waiting cannot deadlock the harness itself.
pub fn run_command_with_timeout(mut cmd: Command, timeout: Duration) -> RunResult {
    let start = Instant::now();
    let mut child: Child = cmd.spawn().expect("spawn child process");

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    fn drain<R: Read + Send + 'static>(r: Option<R>) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        if let Some(mut r) = r {
            thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 64 * 1024];
                loop {
                    match r.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            // Cap retained output so a runaway child cannot OOM the test.
                            if buf.len() < 8 * 1024 * 1024 {
                                buf.extend_from_slice(&chunk[..n]);
                            }
                        }
                        Err(_) => break,
                    }
                }
                let _ = tx.send(buf);
            });
        } else {
            let _ = tx.send(Vec::new());
        }
        rx
    }

    let out_rx = drain(stdout);
    let err_rx = drain(stderr);

    let mut timed_out = false;
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(st) => break Some(st.code()),
            None => {
                if start.elapsed() >= timeout {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
    };

    let collect = |rx: mpsc::Receiver<Vec<u8>>| -> String {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(_) => String::new(),
        }
    };

    RunResult { status, stdout: collect(out_rx), stderr: collect(err_rx), timed_out, elapsed: start.elapsed() }
}

/// Count the open file descriptors of the current process.
#[cfg(target_os = "linux")]
pub fn open_fd_count() -> Option<usize> {
    std::fs::read_dir("/proc/self/fd").ok().map(|d| d.count())
}

#[cfg(not(target_os = "linux"))]
pub fn open_fd_count() -> Option<usize> {
    None
}

/// Write a file of `len` bytes without holding it all in memory.
pub fn write_sized_file(path: &Path, byte: u8, len: u64) {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut f = std::fs::File::create(path).expect("create sized file");
    let chunk = vec![byte; 1024 * 1024];
    let mut written = 0u64;
    while written < len {
        let n = std::cmp::min(chunk.len() as u64, len - written) as usize;
        f.write_all(&chunk[..n]).expect("write chunk");
        written += n as u64;
    }
    f.flush().ok();
}

/// Run a closure on a thread and fail if it does not finish in `timeout`.
/// Used to assert that a library-level operation cannot hang.
pub fn with_timeout<T, F>(timeout: Duration, what: &str, f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(timeout) {
        Ok(v) => v,
        Err(_) => panic!("`{what}` did not finish within {timeout:?}"),
    }
}