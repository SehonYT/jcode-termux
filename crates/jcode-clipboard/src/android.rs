//! The Android backend.
//!
//! Android has no X11, Wayland, Win32 or NSPasteboard, and arboard does not
//! even declare a backend for `target_os = "android"`. The mechanisms that
//! actually exist inside a terminal app on a phone are:
//!
//! 1. **Termux:API** - the `termux-clipboard-get` / `termux-clipboard-set`
//!    binaries, installed by the Termux:API addon app plus `pkg install
//!    termux-api`. These talk to Android's real `ClipboardManager` through a
//!    bound service, so they interoperate with every other app on the device.
//!    They are the only mechanism that can *read* the clipboard.
//! 2. **OSC 52** - a terminal escape sequence, correct for a phone whose
//!    terminal implements it, and the only thing available when the addon is
//!    not installed. Write-only.
//!
//! Deliberately **not** used: `/system/bin/service call clipboard ...`. It is
//! SELinux-blocked for ordinary apps (it would have to run as shell/root), and
//! when blocked it hangs on the binder transaction instead of failing, which
//! is the worst possible behavior for a UI-thread copy.
//!
//! Every subprocess here goes through [crate::runner] and therefore has a hard
//! timeout; these binaries block indefinitely while Android shows a
//! "Termux:API wants permission" dialog.

use std::sync::Mutex;
use std::time::Duration;

use crate::error::{ClipboardError, Result};
use crate::runner::{CommandRunner, CommandSpec, RunOutput, StdioChannel};
use crate::{osc52, ClipboardConfig};

/// Program that writes the Android clipboard from Termux.
pub const SET_PROGRAM: &str = "termux-clipboard-set";
/// Program that reads the Android clipboard from Termux.
pub const GET_PROGRAM: &str = "termux-clipboard-get";

/// Programs that can read an X11/Wayland clipboard.
///
/// Only reachable on an *odd* Android install: a chroot/proot with a display,
/// or a Termux X11 session. `has_program` short-circuits the common case, so
/// this costs nothing on a stock phone, and it is the one place where the
/// otherwise-dead `wl-paste`/`xclip` selectors can still pay off.
const GET_FALLBACKS: &[(&str, &[&str])] = &[
    ("wl-paste", &["--no-newline"]),
    ("xclip", &["-selection", "clipboard", "-o"]),
    ("xsel", &["--clipboard", "--output"]),
];

/// How long the one-time availability probe is allowed to take.
///
/// Shorter than the general timeout on purpose: the probe must never be on the
/// critical path of the *actual* copy, which repeats the same spawn and would
/// then pay the timeout twice in the worst case.
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// Cached answer to "is a Termux:API clipboard helper installed?".
///
/// `None` = not looked up yet, `Some(true/false)` = cached. The mutex is only
/// ever held for a load/store, never across a spawn, so it cannot serialize
/// clipboard operations or deadlock with a runner that calls back in.
static TERMUX_API: Mutex<Option<bool>> = Mutex::new(None);

/// Whether the last attempted mechanism failed and OSC 52 took over.
///
/// `jcode-tui` uses this to say "Termux:API is installed but failed; OSC 52
/// used" instead of an unconditional "Copied", which would have hidden a
/// real setup problem (most often a revoked Termux:API permission).
static OSC52_REQUESTED_AFTER_FAILURE: Mutex<bool> = Mutex::new(false);

/// The last failure seen by [set_text], for diagnostics.
static LAST_FAILURE: Mutex<Option<String>> = Mutex::new(None);

/// Reset the cached availability answer.
///
/// Needed when the user installs the Termux:API addon while jcode is running:
/// without this, the process would keep answering "not available" forever.
pub fn reset_probe_cache() {
    if let Ok(mut slot) = TERMUX_API.lock() {
        *slot = None;
    }
}

/// Whether a Termux:API clipboard helper exists, probing once per process.
fn termux_api_available<R: CommandRunner>(runner: &R, config: &ClipboardConfig) -> bool {
    // Test/config override wins: it must not be overwritten by a probe, so the
    // cache is not consulted at all in that case.
    if let Some(forced) = config.termux_api_available {
        return forced;
    }
    // `has_program` is a pure PATH lookup (no spawn), so this is cheap; the
    // mutex is still dropped before probing so two racing threads do not
    // serialize behind each other.
    if let Ok(slot) = TERMUX_API.lock() {
        if let Some(cached) = *slot {
            return cached;
        }
    }
    let available = runner.has_program(SET_PROGRAM) || runner.has_program(GET_PROGRAM);
    if let Ok(mut slot) = TERMUX_API.lock() {
        *slot = Some(available);
    }
    available
}

fn note_failure(detail: String) {
    if let Ok(mut slot) = LAST_FAILURE.lock() {
        *slot = Some(detail);
    }
}

/// Why the last Termux:API attempt failed, if it did.
pub fn last_failure() -> Option<String> {
    LAST_FAILURE.lock().ok().and_then(|slot| slot.clone())
}

/// Record that an OSC 52 write was reached after a failed Termux:API call.
///
/// Called by [crate::Clipboard::write_osc52] - the OSC 52 write itself needs a
/// caller-supplied writer, so this crate cannot observe it from inside
/// [set_text].
pub fn note_osc52_after_failed_termux_api() {
    if last_failure().is_some() {
        if let Ok(mut slot) = OSC52_REQUESTED_AFTER_FAILURE.lock() {
            *slot = true;
        }
    }
}

/// Whether OSC 52 was used as a fallback after Termux:API failed.
pub fn osc52_fallback_used() -> bool {
    OSC52_REQUESTED_AFTER_FAILURE
        .lock()
        .map(|slot| *slot)
        .unwrap_or(false)
}

/// Forget the recorded failure / fallback state (call at the start of a copy).
fn clear_attempt_state() {
    if let Ok(mut slot) = LAST_FAILURE.lock() {
        *slot = None;
    }
    if let Ok(mut slot) = OSC52_REQUESTED_AFTER_FAILURE.lock() {
        *slot = false;
    }
}

/// Copy `text` to the Android clipboard.
///
/// Order: Termux:API (authoritative, writes the *system* clipboard) then OSC 52
/// (needs the terminal to cooperate). Reached from
/// [crate::Clipboard::set_text]; the OSC 52 half is left to the caller because
/// only the caller owns stdout, so a successful return with a recorded
/// [last_failure] means "Termux:API failed, please write the OSC 52 sequence".
pub fn set_text<R: CommandRunner>(
    runner: &R,
    config: &ClipboardConfig,
    text: &str,
) -> Result<()> {
    clear_attempt_state();
    if termux_api_available(runner, config) {
        // `CommandSpec::new` defaults to 2s; the config must win, otherwise a
        // caller that raised or lowered the budget (a test, or a slow phone)
        // silently gets the default and the timeout is not actually configurable.
        let spec = CommandSpec::new(SET_PROGRAM, &[])
            .with_stdin(text.as_bytes())
            .with_timeout(config.timeout);
        return match runner.run(&spec)? {
            RunOutput::Exited { .. } => Ok(()),
            other => {
                // The helper exists but did not take the text (permission
                // dialog dismissed, `am broadcast` unresolved, ...). Report it
                // as a real failure instead of pretending OSC 52 is a
                // substitute: the caller decides whether to fall back, and the
                // recorded reason ends up in `jcode doctor` output.
                let detail = other.stderr_text();
                note_failure(format!("{SET_PROGRAM}: {detail}"));
                Err(clipboard_error_for(SET_PROGRAM, &other, config.timeout))
            }
        };
    }
    // No Termux:API: the only remaining movable part is the terminal itself.
    Err(ClipboardError::NoMechanism)
}

/// Read text from the Android clipboard.
///
/// Order: Termux:API (the real `ClipboardManager`), then `wl-paste`/`xclip`/
/// `xsel` for the exotic X11-in-Termux setups. There is no OSC 52 read path,
/// so when neither exists the answer is a typed [ClipboardError::NoMechanism]
/// rather than an empty string - "nothing is installed" and "the clipboard is
/// empty" lead to different advice.
pub fn get_text<R: CommandRunner>(runner: &R, config: &ClipboardConfig) -> Result<String> {
    clear_attempt_state();
    let mut termux_failure: Option<ClipboardError> = None;
    if termux_api_available(runner, config) {
        let spec = CommandSpec::new(GET_PROGRAM, &[]).with_timeout(config.timeout);
        match runner.run(&spec)? {
            RunOutput::Exited { stdout, .. } => return decode_text(GET_PROGRAM, stdout),
            other => {
                // A failed *read* is not fatal while a display helper might
                // exist: Termux:API can be installed-but-broken (revoked
                // permission) while an X11 clipboard still works.
                let detail = other.stderr_text();
                note_failure(format!("{GET_PROGRAM}: {detail}"));
                termux_failure = Some(clipboard_error_for(GET_PROGRAM, &other, config.timeout));
            }
        }
    }
    for (program, args) in GET_FALLBACKS {
        if !runner.has_program(program) {
            continue;
        }
        let spec = CommandSpec::new(*program, args).with_timeout(config.timeout);
        match runner.run(&spec)? {
            RunOutput::Exited { stdout, .. } => return decode_text(program, stdout),
            // Same rule as the desktop backend: a helper that ran and failed
            // is a real failure, not a reason to try a weaker one and report a
            // false success.
            other => return Err(clipboard_error_for(program, &other, config.timeout)),
        }
    }
    if let Some(error) = termux_failure {
        return Err(error);
    }
    Err(ClipboardError::NoMechanism)
}

/// Interpret a finished process's stdout as UTF-8 text.
fn decode_text(program: &str, stdout: Vec<u8>) -> Result<String> {
    if stdout.is_empty() {
        return Err(ClipboardError::Empty {
            program: program.to_string(),
        });
    }
    String::from_utf8(stdout).map_err(|err| ClipboardError::NotUtf8 {
        program: program.to_string(),
        len: err.as_bytes().len(),
    })
}

/// Map a non-successful [RunOutput] onto the typed error the caller reports.
fn clipboard_error_for(program: &str, output: &RunOutput, timeout: Duration) -> ClipboardError {
    match output {
        RunOutput::TimedOut { stderr, .. } => ClipboardError::Timeout {
            limit_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            stderr: stderr.clone(),
        },
        RunOutput::StillRunning { observed_ms, .. } => ClipboardError::Failed(format!(
            "{program}: still running after {observed_ms}ms (killed; it never returned)"
        )),
        other => ClipboardError::Failed(format!("{program}: {}", other.stderr_text())),
    }
}

/// Human-readable availability summary, for `jcode doctor`.
pub fn diagnostics<R: CommandRunner>(runner: &R) -> String {
    let termux = if runner.has_program(SET_PROGRAM) || runner.has_program(GET_PROGRAM) {
        "yes"
    } else {
        "no"
    };
    let mut display: Vec<&str> = Vec::new();
    for (program, _) in GET_FALLBACKS {
        if runner.has_program(program) {
            display.push(program);
        }
    }
    let display = if display.is_empty() {
        "none".to_string()
    } else {
        display.join(", ")
    };
    let last = last_failure().unwrap_or_else(|| "-".to_string());
    format!(
        "android clipboard: termux-api={termux} ({SET_PROGRAM}/{GET_PROGRAM}), display-readers={display}, osc52=write-only, last-failure={last}"
    )
}

/// The stdio channels the Android helpers use. Kept so the contract (text in on
/// stdin, text out on stdout) is documented in code and covered by a test.
pub fn stdio_channels() -> [StdioChannel; 2] {
    [StdioChannel::Stdin, StdioChannel::Stdout]
}

/// OSC 52 helper for callers that want the Android backend's encoding rules.
pub fn osc52_sequence<W: std::io::Write>(out: &mut W, text: &str) -> Result<()> {
    osc52::write(out, crate::Osc52Target::Clipboard, text)
}

/// The probe budget, exposed so tests can assert it stays under the real
/// timeout rather than being silently raised.
pub const fn probe_timeout() -> Duration {
    PROBE_TIMEOUT
}
