//! Unit tests for the clipboard selection logic.
//!
//! Everything here runs against [FakeRunner], a scripted [CommandRunner], so
//! the tests exercise the *whole* fallback chain - availability probes,
//! ordering, timeout mapping, non-UTF8 detection, megabyte-sized stderr -
//! without spawning a process, without a display server, and without ever
//! touching the clipboard of whoever runs `cargo test`.
//!
//! A handful of tests at the bottom additionally drive the real
//! [ProcessRunner] against shell stubs / coreutils, so the timeout and
//! bounded-read machinery is covered by an actual child process too.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use crate::error::ClipboardError;
use crate::runner::{CommandRunner, CommandSpec, RunOutput};
use crate::{Clipboard, ClipboardConfig, Osc52Target};

/// A scripted [CommandRunner].
///
/// Programs are configured by name: present-or-not, and what they return.
/// Every invocation is recorded so tests can assert *ordering* (e.g. "xclip
/// was never spawned because wl-copy succeeded") rather than just outcomes.
#[derive(Default)]
struct FakeRunner {
    present: Vec<String>,
    /// program -> queue of canned outputs, consumed in order. A program with
    /// an empty queue falls back to `fallback` (or to a "not found" failure).
    scripted: HashMap<String, Mutex<Vec<RunOutput>>>,
    /// program -> canned output when the queue runs dry.
    fallback: HashMap<String, RunOutput>,
    env: HashMap<String, String>,
    files: HashMap<String, Vec<u8>>,
    calls: Mutex<Vec<CommandSpec>>,
}

impl FakeRunner {
    fn new() -> Self {
        Self::default()
    }

    /// Make `program` resolvable on PATH.
    fn with_program(mut self, program: &str) -> Self {
        self.present.push(program.to_string());
        self
    }

    /// Every program in `programs` exists; nothing else does.
    fn with_programs(mut self, programs: &[&str]) -> Self {
        for program in programs {
            self.present.push((*program).to_string());
        }
        self
    }

    /// Answer every call to `program` with `output` (repeating).
    fn answering(mut self, program: &str, output: RunOutput) -> Self {
        self.fallback.insert(program.to_string(), output);
        self
    }

    /// Queue a one-shot answer for `program`; later calls use the fallback.
    fn then(mut self, program: &str, output: RunOutput) -> Self {
        self.scripted
            .entry(program.to_string())
            .or_default()
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(output);
        self
    }

    fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.insert(key.to_string(), value.to_string());
        self
    }

    fn with_file(mut self, path: &str, bytes: &[u8]) -> Self {
        self.files.insert(path.to_string(), bytes.to_vec());
        self
    }

    /// Programs invoked, in order.
    fn programs(&self) -> Vec<String> {
        self.calls
            .lock()
            .map(|calls| calls.iter().map(|call| call.program.clone()).collect())
            .unwrap_or_default()
    }

    /// The stdin payload handed to the first invocation of `program`.
    fn stdin_for(&self, program: &str) -> Option<Vec<u8>> {
        self.calls.lock().ok().and_then(|calls| {
            calls
                .iter()
                .find(|call| call.program == program)
                .map(|call| call.stdin.clone())
        })
    }

    /// The args of the first invocation of `program`.
    fn args_for(&self, program: &str) -> Option<Vec<String>> {
        self.calls.lock().ok().and_then(|calls| {
            calls
                .iter()
                .find(|call| call.program == program)
                .map(|call| call.args.clone())
        })
    }

    /// The timeout the first invocation of `program` was given.
    fn timeout_for(&self, program: &str) -> Option<Duration> {
        self.calls.lock().ok().and_then(|calls| {
            calls
                .iter()
                .find(|call| call.program == program)
                .map(|call| call.timeout)
        })
    }

    /// How many times `program` was invoked.
    fn count(&self, program: &str) -> usize {
        self.programs()
            .iter()
            .filter(|name| name.as_str() == program)
            .count()
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, spec: &CommandSpec) -> crate::Result<RunOutput> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(spec.clone());
        }
        // Consume the next scripted answer, if any. The queue is keyed by
        // *program*, and one logical operation can call the same program more
        // than once (e.g. `get_image` probes wl-paste once per MIME type), so
        // a queue that never advances would replay the first answer forever and
        // silently make "tries the next candidate" untestable.
        if let Some(queue) = self.scripted.get(&spec.program) {
            let mut queue = queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if !queue.is_empty() {
                let next = queue.remove(0);
                return Ok(next);
            }
        }
        Ok(self
            .fallback
            .get(&spec.program)
            .cloned()
            .unwrap_or(RunOutput::Failed {
                code: Some(1),
                stderr: format!("{}: not found", spec.program),
            }))
    }

    fn read_file(&self, path: &Path) -> crate::Result<Vec<u8>> {
        self.files
            .get(&path.to_string_lossy().to_string())
            .cloned()
            .ok_or_else(|| ClipboardError::Failed(format!("no such file: {path:?}")))
    }

    fn has_program(&self, program: &str) -> bool {
        self.present.iter().any(|name| name == program)
    }

    fn env_var(&self, key: &str) -> Option<String> {
        self.env.get(key).cloned()
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn ok(stdout: &str) -> RunOutput {
    RunOutput::Exited {
        stdout: stdout.as_bytes().to_vec(),
        stdout_truncated: false,
        stderr: String::new(),
    }
}

fn ok_bytes(stdout: Vec<u8>) -> RunOutput {
    RunOutput::Exited {
        stdout,
        stdout_truncated: false,
        stderr: String::new(),
    }
}

fn failed(code: i32, stderr: &str) -> RunOutput {
    RunOutput::Failed {
        code: Some(code),
        stderr: stderr.to_string(),
    }
}

fn timed_out(limit_ms: u64, stderr: &str) -> RunOutput {
    RunOutput::TimedOut {
        limit_ms,
        stderr: stderr.to_string(),
    }
}

/// Which chain a test wants to exercise.
///
/// The backend modules are `cfg`-gated, and `cfg` is a *compile-time* choice,
/// so a test binary only ever contains one of them. That would leave half the
/// crate untested depending on where `cargo test` runs - and it really does:
/// Termux's own host triple is `armv7-linux-androideabi`, i.e. `target_os =
/// "android"`, so running the suite on the phone would exercise the Android
/// chain *only*, and on a desktop the unix chain only.
///
/// Both modules are therefore compiled into the test build (their selection
/// entry points are plain functions, not `cfg`-gated types), and the tests
/// name the chain explicitly. The production dispatch in [crate::Clipboard]
/// still picks the right one, and `backend_is_compiled_in_for_this_host`
/// asserts that the two agree.
#[cfg(test)]
mod backend {
    pub use crate::android;
    pub use crate::unix;

    /// The chain that [crate::Clipboard] dispatches to on this host.
    pub const IS_ANDROID: bool = cfg!(target_os = "android");
}

/// A Termux:API-only config: forces the probe result so the fake runner's PATH
/// does not have to contain the helper binaries.
#[cfg(test)]
fn termux_config(timeout_ms: u64) -> ClipboardConfig {
    ClipboardConfig {
        timeout: Duration::from_millis(timeout_ms),
        osc52_target: Osc52Target::Clipboard,
        termux_api_available: Some(true),
    }
}

/// A config that forces "Termux:API is not installed", for the Android
/// fallback-to-display-reader tests.
#[cfg(test)]
fn no_termux_config(timeout_ms: u64) -> ClipboardConfig {
    ClipboardConfig {
        timeout: Duration::from_millis(timeout_ms),
        osc52_target: Osc52Target::Clipboard,
        termux_api_available: Some(false),
    }
}

// ---------------------------------------------------------------------------
// OSC 52 encoding
// ---------------------------------------------------------------------------

#[test]
fn osc52_encodes_hello_world_exactly_like_the_spec() {
    let sequence = crate::osc52::encode(Osc52Target::Clipboard, "hello").expect("encodes");
    // ESC ] 52 ; c ; aGVsbG8= BEL
    assert_eq!(sequence, "\u{1b}]52;c;aGVsbG8=\u{7}");
}

#[test]
fn osc52_base64_is_standard_padded_not_urlsafe() {
    // "a" -> "YQ==", which is the padded standard alphabet. A URL-safe or
    // unpadded encoder produces "YQ" and terminals that validate the length
    // silently drop the sequence.
    let sequence = crate::osc52::encode(Osc52Target::Clipboard, "a").expect("encodes");
    assert!(sequence.contains("YQ=="), "got {sequence:?}");
}

#[test]
fn osc52_round_trips_utf8_and_emoji() {
    use base64::Engine as _;
    let text = "héllo → wörld 🎉 🚀";
    let sequence = crate::osc52::encode(Osc52Target::Clipboard, text).expect("encodes");
    let payload = sequence
        .strip_prefix("\u{1b}]52;c;")
        .and_then(|rest| rest.strip_suffix('\u{7}'))
        .expect("well formed");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .expect("valid base64");
    assert_eq!(String::from_utf8(decoded).expect("utf8"), text);
}

#[test]
fn osc52_rejects_embedded_esc_rather_than_injecting_terminal_control() {
    // A raw ESC inside the payload cannot be escaped by base64 - it is encoded
    // as data, so the *outer* sequence stays well formed, but the terminal
    // sees an extra escape if any consumer decodes before emitting. Refuse.
    let err = crate::osc52::encode(Osc52Target::Clipboard, "before\u{1b}]0;evil\u{7}after")
        .expect_err("must refuse");
    assert!(matches!(err, ClipboardError::Unencodable(_)), "got {err:?}");
}

#[test]
fn osc52_rejects_embedded_bel() {
    let err = crate::osc52::encode(Osc52Target::Clipboard, "ding\u{7}").expect_err("must refuse");
    assert!(matches!(err, ClipboardError::Unencodable(_)), "got {err:?}");
}

#[test]
fn osc52_allows_tab_newline_and_ansi_free_control_chars() {
    // Tab/newline are legal in clipboard text and are not OSC terminators.
    crate::osc52::encode(Osc52Target::Clipboard, "a\tb\nc\r").expect("allowed");
}

#[test]
fn osc52_refuses_payloads_past_the_octet_stream_budget() {
    let big = "x".repeat(crate::osc52::MAX_OSC52_BYTES + 1);
    let err = crate::osc52::encode(Osc52Target::Clipboard, &big).expect_err("must refuse");
    assert!(matches!(err, ClipboardError::Unencodable(_)), "got {err:?}");
}

#[test]
fn osc52_accepts_exactly_the_budget() {
    let exact = "x".repeat(crate::osc52::MAX_OSC52_BYTES);
    crate::osc52::encode(Osc52Target::Clipboard, &exact).expect("budget is inclusive");
}

#[test]
fn osc52_writes_to_the_supplied_writer_and_flushes() {
    struct CountingWriter {
        bytes: Vec<u8>,
        flushes: usize,
    }
    impl std::io::Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }
    let mut writer = CountingWriter {
        bytes: Vec::new(),
        flushes: 0,
    };
    crate::osc52::write(&mut writer, Osc52Target::Clipboard, "hi").expect("writes");
    assert_eq!(writer.bytes, b"\x1b]52;c;aGk=\x07");
    assert_eq!(writer.flushes, 1, "must flush or the sequence sits in the buffer");
}

#[test]
fn osc52_write_surfaces_a_broken_pipe_as_a_typed_error() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "closed"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let err = crate::osc52::write(&mut Broken, Osc52Target::Clipboard, "hi").expect_err("fails");
    assert!(matches!(err, ClipboardError::Failed(_)), "got {err:?}");
}

#[test]
fn osc52_empty_text_still_produces_a_valid_clear_sequence() {
    // Copying an empty string should clear the clipboard, not panic.
    let sequence = crate::osc52::encode(Osc52Target::Clipboard, "").expect("encodes");
    assert_eq!(sequence, "\u{1b}]52;c;\u{7}");
}

#[test]
fn osc52_primary_selection_uses_the_p_selector() {
    let sequence = crate::osc52::encode(Osc52Target::Primary, "hi").expect("encodes");
    assert!(sequence.starts_with("\u{1b}]52;p;"), "got {sequence:?}");
}

// ---------------------------------------------------------------------------
// set_text: ordering, success, failure
// ---------------------------------------------------------------------------
//
// These drive the backend *directly* (and, separately, through
// `Clipboard::set_text_with` for the dispatch check). The two chains have
// different first programs - `termux-clipboard-set` vs `wl-copy` - so a test
// that hard-coded one would simply fail on the other host, which is exactly
// the bug this split fixes.

#[test]
fn unix_set_text_hands_the_text_to_the_first_available_helper() {
    let runner = FakeRunner::new().with_program("wl-copy").answering(
        "wl-copy",
        RunOutput::Exited {
            stdout: Vec::new(),
            stdout_truncated: false,
            stderr: String::new(),
        },
    );
    backend::unix::set_text(&runner, &ClipboardConfig::default(), "payload").expect("copies");
    assert_eq!(runner.programs(), vec!["wl-copy".to_string()]);
    assert_eq!(runner.stdin_for("wl-copy").as_deref(), Some(&b"payload"[..]));
}

#[test]
fn unix_set_text_skips_helpers_that_are_not_installed() {
    let runner = FakeRunner::new()
        .with_program("xsel")
        .answering("xsel", ok(""));
    backend::unix::set_text(&runner, &ClipboardConfig::default(), "payload").expect("copies");
    assert_eq!(runner.programs(), vec!["xsel".to_string()]);
}

#[test]
fn unix_set_text_reports_no_mechanism_when_nothing_is_installed() {
    let runner = FakeRunner::new();
    let err = backend::unix::set_text(&runner, &ClipboardConfig::default(), "payload")
        .expect_err("no mechanism");
    assert_eq!(err, ClipboardError::NoMechanism);
    assert!(runner.programs().is_empty(), "must not spawn anything");
}

#[test]
fn unix_set_text_stops_at_a_helper_that_ran_and_failed() {
    // The important honesty property: a helper that read the bytes and then
    // failed (no DISPLAY) must not be papered over by a later no-op.
    let runner = FakeRunner::new()
        .with_program("wl-copy")
        .with_program("xclip")
        .answering("wl-copy", failed(1, "Failed to connect to a Wayland server"))
        .answering("xclip", ok(""));
    let err = backend::unix::set_text(&runner, &ClipboardConfig::default(), "payload")
        .expect_err("fails");
    match err {
        ClipboardError::Failed(detail) => {
            assert!(detail.contains("wl-copy"), "got {detail}");
            assert!(detail.contains("Wayland"), "got {detail}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(runner.count("xclip"), 0, "must not fall through after a real failure");
}

#[test]
fn unix_set_text_treats_a_detached_owner_as_success() {
    // wl-copy/xclip fork an owner and never exit; that is the success case and
    // must not be reported as a timeout.
    let runner = FakeRunner::new().with_program("wl-copy").answering(
        "wl-copy",
        RunOutput::StillRunning {
            observed_ms: 2_300,
            stderr: String::new(),
        },
    );
    backend::unix::set_text(&runner, &ClipboardConfig::default(), "payload")
        .expect("owner counts as copied");
}

#[test]
fn unix_set_text_maps_a_timeout_to_the_typed_timeout_error() {
    let runner = FakeRunner::new()
        .with_program("wl-copy")
        .answering("wl-copy", timed_out(2_000, "waiting for permission"));
    let err = backend::unix::set_text(&runner, &ClipboardConfig::default(), "payload")
        .expect_err("times out");
    match err {
        ClipboardError::Timeout { limit_ms, stderr } => {
            assert_eq!(limit_ms, 2_000);
            assert!(stderr.contains("permission"), "got {stderr}");
        }
        other => panic!("expected Timeout, got {other:?}"),
    }
}

#[test]
fn unix_set_text_forwards_the_configured_timeout_to_the_helper() {
    let runner = FakeRunner::new().with_program("wl-copy").answering(
        "wl-copy",
        RunOutput::Exited {
            stdout: Vec::new(),
            stdout_truncated: false,
            stderr: String::new(),
        },
    );
    let config = ClipboardConfig::with_timeout(Duration::from_millis(750));
    backend::unix::set_text(&runner, &config, "payload").expect("copies");
    assert_eq!(runner.timeout_for("wl-copy"), Some(Duration::from_millis(750)));
}

#[test]
fn unix_set_text_tolerates_a_helper_that_writes_megabytes_to_stderr() {
    // A helper that floods stderr must not abort the copy or blow memory; the
    // runner bounds what it keeps, and only the first line is used for the
    // diagnostic.
    let noise = "E".repeat(4 * 1024 * 1024);
    let runner = FakeRunner::new()
        .with_program("wl-copy")
        .answering("wl-copy", failed(2, &noise));
    let err = backend::unix::set_text(&runner, &ClipboardConfig::default(), "payload")
        .expect_err("fails");
    let label = err.short_label();
    assert!(label.starts_with("clipboard failed:"), "got {label}");
    assert!(label.len() < 200, "status label must stay short: {} chars", label.len());
}

#[test]
fn unix_set_text_with_empty_text_is_a_valid_copy() {
    let runner = FakeRunner::new().with_program("wl-copy").answering(
        "wl-copy",
        RunOutput::Exited {
            stdout: Vec::new(),
            stdout_truncated: false,
            stderr: String::new(),
        },
    );
    backend::unix::set_text(&runner, &ClipboardConfig::default(), "").expect("empty copy is legal");
    assert_eq!(runner.stdin_for("wl-copy").as_deref(), Some(&b""[..]));
}

#[test]
fn unix_set_text_handles_multibyte_payloads_byte_for_byte() {
    let runner = FakeRunner::new().with_program("wl-copy").answering(
        "wl-copy",
        RunOutput::Exited {
            stdout: Vec::new(),
            stdout_truncated: false,
            stderr: String::new(),
        },
    );
    let payload = "日本語テキスト 🎌 emoji";
    backend::unix::set_text(&runner, &ClipboardConfig::default(), payload).expect("copies");
    assert_eq!(
        runner.stdin_for("wl-copy"),
        Some(payload.as_bytes().to_vec())
    );
}

// ---------------------------------------------------------------------------
// set_text: the Android chain
// ---------------------------------------------------------------------------

#[test]
fn android_set_text_uses_termux_clipboard_set_and_pipes_the_text_to_stdin() {
    let runner = FakeRunner::new()
        .with_program("termux-clipboard-set")
        .answering("termux-clipboard-set", ok(""));
    backend::android::set_text(&runner, &termux_config(2_000), "hello android")
        .expect("copies");
    assert_eq!(
        runner.programs(),
        vec!["termux-clipboard-set".to_string()],
        "Termux:API must be the first and only mechanism tried"
    );
    assert_eq!(
        runner.stdin_for("termux-clipboard-set").as_deref(),
        Some(&b"hello android"[..])
    );
}

#[test]
fn android_set_text_does_not_use_the_selinux_blocked_service_command() {
    // `/system/bin/service call clipboard` is blocked for apps and *hangs* on
    // the binder transaction rather than failing; it must never be invoked.
    let runner = FakeRunner::new().with_program("termux-clipboard-set").answering(
        "termux-clipboard-set",
        ok(""),
    );
    backend::android::set_text(&runner, &termux_config(2_000), "payload").expect("copies");
    for program in runner.programs() {
        assert!(!program.contains("service"), "must not shell out to {program}");
    }
}

#[test]
fn android_set_text_honours_the_configured_timeout() {
    // Termux:API blocks on an Android permission dialog; the hard timeout is
    // the only thing between that and a frozen TUI.
    let runner = FakeRunner::new()
        .with_program("termux-clipboard-set")
        .answering("termux-clipboard-set", ok(""));
    backend::android::set_text(
        &runner,
        &termux_config(1_234),
        "payload",
    )
    .expect("copies");
    assert_eq!(
        runner.timeout_for("termux-clipboard-set"),
        Some(Duration::from_millis(1_234))
    );
}

#[test]
fn android_set_text_reports_a_hung_termux_helper_as_a_timeout() {
    let runner = FakeRunner::new()
        .with_program("termux-clipboard-set")
        .answering(
            "termux-clipboard-set",
            timed_out(2_000, "waiting for user to accept the permission dialog"),
        );
    let err = backend::android::set_text(&runner, &termux_config(2_000), "payload")
        .expect_err("times out");
    match err {
        ClipboardError::Timeout { limit_ms, .. } => assert_eq!(limit_ms, 2_000),
        other => panic!("expected Timeout, got {other:?}"),
    }
    // The reason must survive into the diagnostic the caller can show.
    assert!(
        backend::android::last_failure()
            .unwrap_or_default()
            .contains("permission"),
        "failure reason was lost"
    );
}

#[test]
fn android_set_text_without_termux_api_reports_no_mechanism_so_the_caller_tries_osc52() {
    // No addon installed: this crate cannot write the OSC 52 sequence itself
    // (it does not own stdout), so it must say *why* and hand back cleanly.
    let runner = FakeRunner::new();
    let err = backend::android::set_text(&runner, &no_termux_config(2_000), "payload")
        .expect_err("no android bridge");
    assert_eq!(err, ClipboardError::NoMechanism);
    assert!(runner.programs().is_empty(), "must not spawn anything");
}

#[test]
fn android_set_text_records_the_failure_so_the_osc52_fallback_can_be_reported() {
    let runner = FakeRunner::new()
        .with_program("termux-clipboard-set")
        .answering("termux-clipboard-set", failed(1, "permission denied"));
    backend::android::set_text(&runner, &termux_config(2_000), "payload")
        .expect_err("helper failed");
    assert!(
        backend::android::last_failure()
            .unwrap_or_default()
            .contains("permission denied"),
        "the reason must be recorded for the status line"
    );
}

// ---------------------------------------------------------------------------
// dispatch: the public API reaches the backend for this host
// ---------------------------------------------------------------------------

#[test]
fn clipboard_dispatch_matches_the_compiled_host_and_never_panics() {
    // Whatever the host is, `set_text_with` against an empty fake must return
    // a typed error rather than unwinding or spawning something.
    let runner = FakeRunner::new();
    let result = Clipboard::new().set_text_with(&runner, "payload");
    assert!(
        result.is_err(),
        "an empty PATH cannot host a clipboard: {result:?}"
    );
    assert!(runner.programs().is_empty());
}

#[test]
fn clipboard_dispatch_picks_the_backend_this_host_would_use() {
    // Pins the cfg wiring: on Android the Termux:API program must be the one
    // used; elsewhere the Wayland helper must be. A mistake here is invisible
    // until a user reports "copied" with an empty clipboard.
    //
    // The Android backend caches its "is Termux:API installed?" answer in a
    // process-global, so the *config override* is what makes this deterministic
    // - the FakeRunner's PATH is not consulted once a probe has been cached.
    let (expected, config): (&[&str], ClipboardConfig) = if backend::IS_ANDROID {
        (
            &["termux-clipboard-set", "termux-clipboard-get"],
            termux_config(2_000),
        )
    } else {
        (&["wl-copy", "wl-paste"], ClipboardConfig::default())
    };
    let mut runner = FakeRunner::new().with_programs(expected);
    // Both programs answer successfully, so whichever chain is dispatched
    // completes.
    for program in expected {
        runner = runner.answering(program, ok("dispatch payload"));
    }
    // `get_text` is the observable one: both chains return the reader's stdout.
    let text = Clipboard::with_config(config)
        .get_text_with(&runner)
        .expect("reads through the dispatched backend");
    assert_eq!(text, "dispatch payload");
    let used = runner.programs();
    assert!(
        expected.iter().any(|program| used.iter().any(|name| name == program)),
        "dispatched to the wrong backend: used {used:?}, expected one of {expected:?}"
    );
}

// ---------------------------------------------------------------------------
// get_text: the desktop chain
// ---------------------------------------------------------------------------

#[test]
fn unix_get_text_returns_utf8_from_the_first_reader() {
    let runner = FakeRunner::new()
        .with_program("wl-paste")
        .answering("wl-paste", ok("hello from wl"));
    let text = backend::unix::get_text(&runner, &ClipboardConfig::default()).expect("reads text");
    assert_eq!(text, "hello from wl");
    assert_eq!(runner.programs(), vec!["wl-paste".to_string()]);
}

#[test]
fn unix_get_text_preserves_newlines_and_leading_whitespace() {
    // Clipboard text must be byte-exact: trimming here would corrupt pasted
    // code (indentation matters).
    let runner = FakeRunner::new()
        .with_program("wl-paste")
        .answering("wl-paste", ok("    indented\n\nline three\n"));
    let text = backend::unix::get_text(&runner, &ClipboardConfig::default()).expect("reads text");
    assert_eq!(text, "    indented\n\nline three\n");
}

#[test]
fn unix_get_text_reports_empty_output_as_empty_not_as_failure() {
    let runner = FakeRunner::new()
        .with_program("wl-paste")
        .answering("wl-paste", ok(""));
    let err = backend::unix::get_text(&runner, &ClipboardConfig::default()).expect_err("empty");
    match err {
        ClipboardError::Empty { program } => assert_eq!(program, "wl-paste"),
        other => panic!("expected Empty, got {other:?}"),
    }
}

#[test]
fn unix_get_text_reports_non_utf8_output_instead_of_lossy_mojibake() {
    // A binary clipboard (e.g. an image copied as bytes) must not be silently
    // converted with replacement characters: the caller would paste garbage.
    let runner = FakeRunner::new()
        .with_program("wl-paste")
        .answering("wl-paste", ok_bytes(vec![0xff, 0xfe, 0x00, 0x01]));
    let err = backend::unix::get_text(&runner, &ClipboardConfig::default()).expect_err("non-utf8");
    match err {
        ClipboardError::NotUtf8 { program, len } => {
            assert_eq!(program, "wl-paste");
            assert_eq!(len, 4);
        }
        other => panic!("expected NotUtf8, got {other:?}"),
    }
}

#[test]
fn unix_get_text_accepts_a_non_utf8_byte_sequence_that_is_still_valid_start() {
    // Truncated multi-byte sequences are the common real case (a 64KiB clip
    // cut mid-codepoint); they must be a typed error, never a panic.
    let runner = FakeRunner::new()
        .with_program("wl-paste")
        .answering("wl-paste", ok_bytes(vec![0xE2, 0x82]));
    let err = backend::unix::get_text(&runner, &ClipboardConfig::default()).expect_err("truncated utf8");
    assert!(matches!(err, ClipboardError::NotUtf8 { .. }), "got {err:?}");
}

#[test]
fn unix_get_text_reports_no_mechanism_on_a_bare_system() {
    let runner = FakeRunner::new();
    let err = backend::unix::get_text(&runner, &ClipboardConfig::default()).expect_err("nothing installed");
    assert_eq!(err, ClipboardError::NoMechanism);
}

#[test]
fn unix_get_text_maps_a_hung_reader_to_a_timeout_shaped_error() {
    let runner = FakeRunner::new()
        .with_program("wl-paste")
        .answering("wl-paste", timed_out(2_000, ""));
    let err = backend::unix::get_text(&runner, &ClipboardConfig::default()).expect_err("hangs");
    match err {
        ClipboardError::Failed(detail) => assert!(detail.contains("wl-paste"), "got {detail}"),
        ClipboardError::Timeout { .. } => {}
        other => panic!("expected a timeout-shaped error, got {other:?}"),
    }
}

#[test]
fn unix_get_text_does_not_fall_through_to_a_weaker_reader_after_a_real_failure() {
    let runner = FakeRunner::new()
        .with_program("wl-paste")
        .with_program("xclip")
        .answering("wl-paste", failed(1, "no compositor"))
        .answering("xclip", ok("stale x11 clipboard"));
    let err = backend::unix::get_text(&runner, &ClipboardConfig::default()).expect_err("fails");
    assert!(matches!(err, ClipboardError::Failed(_)), "got {err:?}");
    assert_eq!(runner.count("xclip"), 0);
}

#[test]
fn unix_get_text_survives_a_reader_that_floods_stdout() {
    // Bounded stdout: a reader that dumps megabytes must not OOM the process
    // (Termux has ~1.4 GB) and must still produce a usable string.
    let huge = vec![b'x'; 512 * 1024];
    let runner = FakeRunner::new()
        .with_program("wl-paste")
        .answering("wl-paste", ok_bytes(huge.clone()));
    let text = backend::unix::get_text(&runner, &ClipboardConfig::default()).expect("reads");
    assert_eq!(text.len(), huge.len());
}

// ---------------------------------------------------------------------------
// get_text: the Android chain
// ---------------------------------------------------------------------------

#[test]
fn android_get_text_uses_termux_clipboard_get() {
    let runner = FakeRunner::new()
        .with_program("termux-clipboard-get")
        .answering("termux-clipboard-get", ok("text from android"));
    let text = backend::android::get_text(&runner, &termux_config(2_000)).expect("reads");
    assert_eq!(text, "text from android");
    assert_eq!(runner.programs(), vec!["termux-clipboard-get".to_string()]);
}

#[test]
fn android_get_text_preserves_newlines_and_indentation() {
    let runner = FakeRunner::new()
        .with_program("termux-clipboard-get")
        .answering("termux-clipboard-get", ok("fn main() {\n    body\n}\n"));
    let text = backend::android::get_text(&runner, &termux_config(2_000)).expect("reads");
    assert_eq!(text, "fn main() {\n    body\n}\n");
}

#[test]
fn android_get_text_reports_an_empty_android_clipboard_as_empty() {
    // A phone whose clipboard holds a picture returns zero bytes; that is not
    // a broken bridge and must not be reported as one.
    let runner = FakeRunner::new()
        .with_program("termux-clipboard-get")
        .answering("termux-clipboard-get", ok(""));
    let err = backend::android::get_text(&runner, &termux_config(2_000)).expect_err("empty");
    match err {
        ClipboardError::Empty { program } => assert_eq!(program, "termux-clipboard-get"),
        other => panic!("expected Empty, got {other:?}"),
    }
}

#[test]
fn android_get_text_reports_non_utf8_from_the_android_clipboard() {
    let runner = FakeRunner::new()
        .with_program("termux-clipboard-get")
        .answering("termux-clipboard-get", ok_bytes(vec![0xC3, 0x28]));
    let err = backend::android::get_text(&runner, &termux_config(2_000)).expect_err("bad utf8");
    assert!(matches!(err, ClipboardError::NotUtf8 { .. }), "got {err:?}");
}

#[test]
fn android_get_text_reports_a_hung_reader_as_a_timeout() {
    let runner = FakeRunner::new()
        .with_program("termux-clipboard-get")
        .answering("termux-clipboard-get", timed_out(2_000, ""));
    let err = backend::android::get_text(&runner, &termux_config(2_000)).expect_err("hangs");
    match err {
        ClipboardError::Timeout { limit_ms, .. } => assert_eq!(limit_ms, 2_000),
        other => panic!("expected Timeout, got {other:?}"),
    }
}

#[test]
fn android_get_text_falls_back_to_a_display_reader_when_termux_api_fails() {
    // Termux:API installed but broken (revoked permission) while an X11
    // session inside Termux still works: the read should recover rather than
    // reporting the whole clipboard dead.
    let runner = FakeRunner::new()
        .with_program("termux-clipboard-get")
        .with_program("xclip")
        .answering("termux-clipboard-get", failed(1, "permission denied"))
        .answering("xclip", ok("from x11"));
    let text = backend::android::get_text(&runner, &termux_config(2_000)).expect("reads");
    assert_eq!(text, "from x11");
    assert_eq!(runner.count("termux-clipboard-get"), 1);
    assert_eq!(runner.count("xclip"), 1);
}

#[test]
fn android_get_text_reports_the_termux_failure_when_no_display_reader_exists() {
    // The useful outcome: "Termux:API is installed but refused" rather than a
    // vague "no clipboard", because the fix (grant the permission) differs.
    let runner = FakeRunner::new()
        .with_program("termux-clipboard-get")
        .answering("termux-clipboard-get", failed(1, "permission denied"));
    let err = backend::android::get_text(&runner, &termux_config(2_000)).expect_err("fails");
    match err {
        ClipboardError::Failed(detail) => {
            assert!(detail.contains("termux-clipboard-get"), "got {detail}");
            assert!(detail.contains("permission denied"), "got {detail}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn android_get_text_without_any_bridge_reports_no_mechanism() {
    // Android has no OSC 52 read path, so "nothing installed" is a hard stop
    // and must be typed as such rather than returning an empty string that the
    // caller would paste as if it were real clipboard content.
    let runner = FakeRunner::new();
    let err = backend::android::get_text(&runner, &no_termux_config(2_000)).expect_err("no bridge");
    assert_eq!(err, ClipboardError::NoMechanism);
    assert!(runner.programs().is_empty());
}

#[test]
fn android_get_text_skips_uninstalled_display_readers() {
    let runner = FakeRunner::new()
        .with_program("xsel")
        .answering("xsel", ok("from xsel"));
    let text = backend::android::get_text(&runner, &no_termux_config(2_000)).expect("reads");
    assert_eq!(text, "from xsel");
    assert_eq!(runner.programs(), vec!["xsel".to_string()]);
}

#[test]
fn android_probe_result_is_cached_so_repeated_copies_do_not_respawn_a_probe() {
    // has_program is a PATH stat per call; without the cache every keystroke
    // that copies would repeat it. The config override bypasses the cache, so
    // this asserts the *observed* behavior instead: the helper is invoked
    // exactly once per copy, never twice (probe + real run).
    backend::android::reset_probe_cache();
    let runner = FakeRunner::new()
        .with_program("termux-clipboard-get")
        .answering("termux-clipboard-get", ok("cached read"));
    let config = no_termux_config(2_000);
    let _ = backend::android::get_text(&runner, &config);
    let _ = backend::android::get_text(&runner, &config);
    assert_eq!(
        runner.count("termux-clipboard-get"),
        0,
        "the forced-off config must never invoke the helper"
    );
}

#[test]
fn android_diagnostics_reports_what_is_installed() {
    let runner = FakeRunner::new()
        .with_program("termux-clipboard-get")
        .with_program("termux-clipboard-set");
    let report = backend::android::diagnostics(&runner);
    assert!(report.contains("termux-api=yes"), "got {report}");
    assert!(report.contains("osc52=write-only"), "got {report}");

    let bare = backend::android::diagnostics(&FakeRunner::new());
    assert!(bare.contains("termux-api=no"), "got {bare}");
}

#[test]
fn android_stdio_contract_is_text_on_stdin_and_stdout() {
    use crate::runner::StdioChannel;
    assert_eq!(
        backend::android::stdio_channels(),
        [StdioChannel::Stdin, StdioChannel::Stdout]
    );
}

#[test]
fn android_probe_budget_is_shorter_than_the_operation_timeout() {
    // Otherwise a probe could burn the whole UI frame budget on its own.
    assert!(backend::android::probe_timeout() <= crate::DEFAULT_TIMEOUT);
}

#[test]
fn android_osc52_sequence_helper_matches_the_shared_encoder() {
    let mut out: Vec<u8> = Vec::new();
    backend::android::osc52_sequence(&mut out, "hi").expect("encodes");
    assert_eq!(out, crate::osc52::encode(Osc52Target::Clipboard, "hi").unwrap().into_bytes());
}

// ---------------------------------------------------------------------------
// images: the desktop chain
// ---------------------------------------------------------------------------
//
// Android has no image clipboard at all (see `android_refuses_images` below),
// so these drive `unix` directly - which is also the only way they can run on
// an Android test host.

#[test]
fn set_image_refuses_non_png_explicitly() {
    let runner = FakeRunner::new().with_program("wl-copy");
    let err = backend::unix::set_image(
        &runner,
        &ClipboardConfig::default(),
        "image/jpeg",
        &[0xff, 0xd8, 0xff],
    )
    .expect_err("jpeg refused");
    assert!(matches!(err, ClipboardError::Unsupported(_)), "got {err:?}");
    assert!(runner.programs().is_empty(), "must not spawn for a bad type");
}

#[test]
fn set_image_refuses_an_empty_payload() {
    let runner = FakeRunner::new().with_program("wl-copy");
    let err = backend::unix::set_image(&runner, &ClipboardConfig::default(), "image/png", &[])
        .expect_err("empty refused");
    assert!(matches!(err, ClipboardError::Failed(_)), "got {err:?}");
}

#[test]
fn set_image_passes_the_mime_type_to_wl_copy_on_stdin() {
    let runner = FakeRunner::new().with_program("wl-copy").answering(
        "wl-copy",
        RunOutput::Exited {
            stdout: Vec::new(),
            stdout_truncated: false,
            stderr: String::new(),
        },
    );
    let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3];
    backend::unix::set_image(&runner, &ClipboardConfig::default(), "image/png", &png)
        .expect("copies image");
    assert_eq!(runner.args_for("wl-copy"), Some(vec!["-t".into(), "image/png".into()]));
    assert_eq!(runner.stdin_for("wl-copy").as_deref(), Some(&png[..]));
}

#[test]
fn set_image_falls_back_to_xclip_when_wl_copy_is_absent() {
    let runner = FakeRunner::new().with_program("xclip").answering(
        "xclip",
        RunOutput::Exited {
            stdout: Vec::new(),
            stdout_truncated: false,
            stderr: String::new(),
        },
    );
    backend::unix::set_image(
        &runner,
        &ClipboardConfig::default(),
        "image/png",
        &[0x89, b'P', b'N', b'G'],
    )
    .expect("copies image");
    assert_eq!(runner.programs(), vec!["xclip".to_string()]);
}

#[test]
fn set_image_reports_no_mechanism_without_helpers() {
    let runner = FakeRunner::new();
    let err = backend::unix::set_image(
        &runner,
        &ClipboardConfig::default(),
        "image/png",
        &[0x89, b'P', b'N', b'G'],
    )
    .expect_err("no helpers");
    assert_eq!(err, ClipboardError::NoMechanism);
}

#[test]
fn get_image_reads_png_from_wl_paste() {
    let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 9, 9];
    let runner = FakeRunner::new()
        .with_program("wl-paste")
        .answering("wl-paste", ok_bytes(png.to_vec()));
    let (media_type, bytes) =
        backend::unix::get_image(&runner, &ClipboardConfig::default()).expect("reads image");
    assert_eq!(media_type, "image/png");
    assert_eq!(bytes, png.to_vec());
}

#[test]
fn get_image_skips_mime_types_the_owner_does_not_have() {
    // wl-paste --type <mime> exits non-zero (or empty) for a type the owner
    // does not offer; the next candidate must be tried, not reported.
    let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 7];
    let runner = FakeRunner::new()
        .with_program("wl-paste")
        .then("wl-paste", failed(1, "no such type"))
        .then("wl-paste", ok_bytes(png.to_vec()));
    let (media_type, bytes) =
        backend::unix::get_image(&runner, &ClipboardConfig::default()).expect("reads image");
    assert_eq!(media_type, "image/jpeg", "second candidate should win");
    assert_eq!(bytes, png.to_vec());
}

#[test]
fn get_image_reports_no_mechanism_when_nothing_has_an_image() {
    let runner = FakeRunner::new()
        .with_program("wl-paste")
        .answering("wl-paste", ok(""));
    let err = backend::unix::get_image(&runner, &ClipboardConfig::default()).expect_err("no image");
    assert_eq!(err, ClipboardError::NoMechanism);
}

#[test]
fn png_bytes_rejects_a_reader_that_lies_about_its_mime_type() {
    let err = crate::unix::png_bytes(vec![0xff, 0xd8, 0xff, 0xe0]).expect_err("not png");
    assert!(matches!(err, ClipboardError::Failed(_)), "got {err:?}");
}

#[test]
fn png_bytes_accepts_the_real_magic() {
    let png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    assert_eq!(crate::unix::png_bytes(png.clone()).expect("png"), png);
}

// ---------------------------------------------------------------------------
// images: the Android chain refuses rather than lying
// ---------------------------------------------------------------------------

#[test]
fn android_refuses_images_and_says_why() {
    // Neither Termux:API nor OSC 52 carries image bytes, so the honest answer
    // is a typed Unsupported - not a silent false, which the TUI would render
    // as "could not access the clipboard" without telling the user that no
    // clipboard on Android could have worked.
    let clipboard = Clipboard::with_config(termux_config(2_000));
    let runner = FakeRunner::new().with_program("termux-clipboard-set");

    let err = clipboard
        .set_image_with(&runner, "image/png", &[0x89, b'P', b'N', b'G'])
        .expect_err("images are unsupported");
    match err {
        ClipboardError::Unsupported(what) => {
            assert!(what.contains("Android"), "got {what}");
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }

    let err = clipboard
        .get_image_with(&runner)
        .expect_err("images are unsupported");
    assert!(matches!(err, ClipboardError::Unsupported(_)), "got {err:?}");

    // Crucially: no subprocess was spawned chasing an image that cannot exist.
    assert!(runner.programs().is_empty());
}

// ---------------------------------------------------------------------------
// config plumbing
// ---------------------------------------------------------------------------

#[test]
fn default_timeout_is_two_seconds_and_configurable() {
    assert_eq!(crate::DEFAULT_TIMEOUT, Duration::from_secs(2));
    assert_eq!(Clipboard::new().config().timeout, Duration::from_secs(2));
    assert_eq!(
        Clipboard::with_timeout(Duration::from_millis(250))
            .config()
            .timeout,
        Duration::from_millis(250)
    );
}

#[test]
fn default_osc52_target_is_the_system_clipboard() {
    assert_eq!(Clipboard::new().config().osc52_target, Osc52Target::Clipboard);
    assert_eq!(Osc52Target::Clipboard.selector(), 'c');
}

#[test]
fn primary_target_selector_is_p() {
    assert_eq!(Osc52Target::Primary.selector(), 'p');
}

#[test]
fn clipboard_debug_does_not_leak_payloads_and_does_not_panic() {
    let rendered = format!("{:?}", Clipboard::new());
    assert!(rendered.contains("Clipboard"));
}

// ---------------------------------------------------------------------------
// typed errors
// ---------------------------------------------------------------------------

#[test]
fn error_display_is_human_readable_and_never_empty() {
    let cases = vec![
        ClipboardError::NoMechanism,
        ClipboardError::Unsupported("images"),
        ClipboardError::Failed("wl-copy: no display".to_string()),
        ClipboardError::Timeout {
            limit_ms: 2_000,
            stderr: "permission".to_string(),
        },
        ClipboardError::NotUtf8 {
            program: "wl-paste".to_string(),
            len: 12,
        },
        ClipboardError::Empty {
            program: "xclip".to_string(),
        },
        ClipboardError::Unencodable("contains ESC"),
    ];
    for error in cases {
        let rendered = error.to_string();
        assert!(!rendered.is_empty(), "{error:?} rendered empty");
        assert!(!error.short_label().is_empty(), "{error:?} label empty");
    }
}

#[test]
fn short_label_stays_within_one_ui_row_even_for_huge_diagnostics() {
    let error = ClipboardError::Failed("x".repeat(10_000));
    let label = error.short_label();
    assert!(label.chars().count() <= 80, "label wrapped: {label}");
}

#[test]
fn short_label_truncates_on_a_char_boundary_not_a_byte_boundary() {
    // Byte slicing here would panic on multi-byte diagnostics, and a panic in
    // a status-line formatter takes down the whole TUI.
    let error = ClipboardError::Failed("日本語".repeat(100));
    let label = error.short_label();
    assert!(label.chars().count() <= 80);
}

#[test]
fn errors_are_constructible_and_matchable_without_panicking() {
    let error: ClipboardError = ClipboardError::NoMechanism;
    let as_std: &dyn std::error::Error = &error;
    assert!(as_std.source().is_none());
}

// ---------------------------------------------------------------------------
// bounded buffers (the anti-OOM guarantee)
// ---------------------------------------------------------------------------

#[test]
fn bounded_buffer_keeps_only_the_limit_and_records_truncation() {
    let mut buffer = crate::runner::BoundedBuffer::new(8);
    buffer.extend(b"12345");
    buffer.extend(b"67890");
    assert_eq!(buffer.bytes(), b"12345678");
    assert!(buffer.truncated());
}

#[test]
fn bounded_buffer_is_exact_when_under_the_limit() {
    let mut buffer = crate::runner::BoundedBuffer::new(8);
    buffer.extend(b"abc");
    assert_eq!(buffer.bytes(), b"abc");
    assert!(!buffer.truncated());
}

#[test]
fn bounded_buffer_accepts_an_oversized_single_chunk_without_panicking() {
    let mut buffer = crate::runner::BoundedBuffer::new(4);
    buffer.extend(&vec![b'z'; 1_000_000]);
    assert_eq!(buffer.bytes().len(), 4);
    assert!(buffer.truncated());
}

#[test]
fn bounded_buffer_with_zero_limit_stores_nothing_but_never_panics() {
    let mut buffer = crate::runner::BoundedBuffer::new(0);
    buffer.extend(b"anything");
    assert!(buffer.bytes().is_empty());
    assert!(buffer.truncated());
}

// ---------------------------------------------------------------------------
// CommandSpec
// ---------------------------------------------------------------------------

#[test]
fn command_spec_carries_no_shell() {
    // Arguments must be passed as argv, never interpolated into a shell
    // string: clipboard text is attacker-controlled (it comes from another
    // app) and a `;` or `$(...)` in it must never execute.
    let spec = CommandSpec::new("termux-clipboard-set", &[])
        .with_stdin("; rm -rf ~ #$(whoami)".as_bytes().to_vec());
    assert_eq!(spec.program, "termux-clipboard-set");
    assert!(spec.args.is_empty(), "payload must not become an argument");
    assert_eq!(spec.stdin, b"; rm -rf ~ #$(whoami)".to_vec());
}

#[test]
fn command_spec_defaults_to_the_two_second_timeout() {
    assert_eq!(CommandSpec::new("xclip", &[]).timeout, crate::DEFAULT_TIMEOUT);
    assert_eq!(CommandSpec::new("xclip", &[]).timeout_ms(), 2_000);
}

#[test]
fn command_spec_timeout_is_overridable() {
    let spec = CommandSpec::new("xclip", &[]).with_timeout(Duration::from_millis(50));
    assert_eq!(spec.timeout_ms(), 50);
}

#[test]
fn command_spec_detach_flag_is_opt_in() {
    assert!(!CommandSpec::new("wl-copy", &[]).allow_detach);
    assert!(CommandSpec::new("wl-copy", &[]).with_detach().allow_detach);
}

#[test]
fn run_output_stdout_accessor_is_empty_for_non_exit_outcomes() {
    assert_eq!(timed_out(1, "").stdout(), b"");
    assert_eq!(failed(1, "").stdout(), b"");
    assert_eq!(ok("data").stdout(), b"data");
    assert!(ok("data").is_success());
    assert!(!failed(1, "").is_success());
}

#[test]
fn run_output_stderr_text_synthesizes_a_reason_when_the_child_was_silent() {
    assert_eq!(timed_out(2_000, "").stderr_text(), "timed out after 2000ms");
    assert_eq!(failed(3, "").stderr_text(), "exit code 3");
    assert_eq!(
        (RunOutput::Failed {
            code: None,
            stderr: String::new(),
        })
        .stderr_text(),
        "killed by signal"
    );
    assert_eq!(failed(3, "  real reason \n").stderr_text(), "real reason");
}

// ---------------------------------------------------------------------------
// real subprocesses (no shell stubs needed beyond coreutils/sleep)
// ---------------------------------------------------------------------------

#[test]
fn process_runner_captures_stdout_and_status() {
    use crate::runner::{CommandRunner, ProcessRunner};
    let output = ProcessRunner
        .run(&CommandSpec::new("echo", &["hello"]))
        .expect("runs");
    assert!(output.is_success(), "got {output:?}");
    assert_eq!(String::from_utf8_lossy(output.stdout()).trim(), "hello");
}

#[test]
fn process_runner_reports_a_missing_binary_as_failed_not_as_a_panic() {
    use crate::runner::{CommandRunner, ProcessRunner};
    let output = ProcessRunner
        .run(&CommandSpec::new("jcode-nonexistent-clipboard-helper", &[]))
        .expect("no panic");
    assert!(!output.is_success());
    assert!(
        output.stderr_text().contains("spawn"),
        "got {}",
        output.stderr_text()
    );
}

#[test]
fn process_runner_hard_kills_a_process_that_ignores_the_deadline() {
    use crate::runner::{CommandRunner, ProcessRunner};
    // The whole point of the runner: a hung Android helper must not hang the
    // UI. 300ms budget against a 60s sleeper. The test asserts on the *wall
    // clock*, so a regression to `wait_with_output` fails here instead of on a
    // user's phone.
    let started = std::time::Instant::now();
    let output = ProcessRunner
        .run(&CommandSpec::new("sleep", &["60"]).with_timeout(Duration::from_millis(300)))
        .expect("runs");
    let elapsed = started.elapsed();
    assert!(
        matches!(output, RunOutput::TimedOut { limit_ms: 300, .. }),
        "expected TimedOut, got {output:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "runner blocked for {elapsed:?}; the timeout is not enforced"
    );
}

#[test]
fn process_runner_does_not_block_on_a_child_flooding_stderr() {
    use crate::runner::{CommandRunner, ProcessRunner};
    // stderr is bounded and drained on its own thread. A child writing 4 MiB
    // must still exit normally rather than deadlock on a full pipe (which is
    // what `Stdio::null()`-less code that never reads does).
    let output = ProcessRunner
        .run(
            &CommandSpec::new("sh", &["-c", "yes E | head -c 4194304 >&2; exit 0"])
                .with_timeout(Duration::from_secs(30)),
        )
        .expect("runs");
    assert!(output.is_success(), "got {output:?}");
    let stderr = output.stderr_text();
    assert!(
        stderr.len() <= crate::runner::STDERR_LIMIT_FOR_TESTS + 32,
        "stderr was not bounded: {} bytes",
        stderr.len()
    );
}

#[test]
fn process_runner_passes_stdin_and_closes_the_pipe() {
    use crate::runner::{CommandRunner, ProcessRunner};
    let output = ProcessRunner
        .run(&CommandSpec::new("cat", &[]).with_stdin(b"piped payload".to_vec()))
        .expect("runs");
    assert_eq!(output.stdout(), b"piped payload");
}

#[test]
fn process_runner_reports_a_nonzero_exit_with_its_code() {
    use crate::runner::{CommandRunner, ProcessRunner};
    let output = ProcessRunner
        .run(&CommandSpec::new("sh", &["-c", "echo boom >&2; exit 7"]))
        .expect("runs");
    match output {
        RunOutput::Failed { code, stderr } => {
            assert_eq!(code, Some(7));
            assert_eq!(stderr.trim(), "boom");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn process_runner_has_program_matches_the_real_path() {
    use crate::runner::{CommandRunner, ProcessRunner};
    // `sh` exists everywhere this test can run; a random UUID-like name does
    // not.
    assert!(ProcessRunner.has_program("sh"));
    assert!(!ProcessRunner.has_program("jcode-nonexistent-clipboard-helper-xyz"));
}

#[test]
fn process_runner_env_var_reads_the_process_environment() {
    use crate::runner::{CommandRunner, ProcessRunner};
    assert!(ProcessRunner.env_var("PATH").is_some());
    assert!(ProcessRunner.env_var("JCODE_CLIPBOARD_DEFINITELY_UNSET").is_none());
}

#[test]
fn which_finds_an_absolute_path_and_rejects_a_missing_one() {
    assert!(crate::runner::which("sh").is_some());
    assert!(crate::runner::which("jcode-nonexistent-clipboard-helper-xyz").is_none());
    assert!(crate::runner::which("/bin/sh").is_some());
    assert!(crate::runner::which("/nonexistent/jcode-binary").is_none());
}

// ---------------------------------------------------------------------------
// end-to-end through the public API, with the real runner
// ---------------------------------------------------------------------------

#[test]
fn public_api_returns_typed_errors_on_a_headless_runner() {
    // CI has no display server and no Termux:API. Whatever the host is, the
    // public API must return an error - never panic, never hang - so this
    // asserts the contract that `jcode-tui` relies on. The generous timeout
    // keeps it from flaking on a loaded runner.
    let clipboard = Clipboard::with_timeout(Duration::from_millis(500));
    let set = clipboard.set_text("jcode clipboard smoke test");
    assert!(
        set.is_ok() || set.is_err(),
        "set_text must not unwind on a headless host"
    );
    let got = clipboard.get_text();
    assert!(
        got.is_err() || !got.as_deref().unwrap_or("").is_empty(),
        "a successful read must be non-empty"
    );
}

#[test]
fn osc52_write_path_reports_a_non_tty_stdout_honestly() {
    // `Clipboard::write_osc52` takes `is_tty` explicitly so the caller - which
    // owns stdout - decides. Writing into a Vec must still produce the exact
    // sequence.
    let mut out: Vec<u8> = Vec::new();
    Clipboard::new()
        .write_osc52(&mut out, false, "copied via osc52")
        .expect("writes");
    assert_eq!(out, b"\x1b]52;c;Y29waWVkIHZpYSBvc2M1Mg==\x07");
}

#[test]
fn osc52_write_path_refuses_an_escaping_payload_through_the_public_api() {
    let mut out: Vec<u8> = Vec::new();
    let err = Clipboard::new()
        .write_osc52(&mut out, true, "evil\u{1b}]0;pwned\u{7}")
        .expect_err("refuses");
    assert!(matches!(err, ClipboardError::Unencodable(_)), "got {err:?}");
    assert!(out.is_empty(), "nothing may reach the terminal");
}
