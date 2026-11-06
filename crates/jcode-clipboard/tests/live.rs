//! Live integration checks against the REAL clipboard mechanisms of whatever
//! host runs them.
//!
//! These are the only tests that execute an actual helper binary, so they are
//! the ones that can catch a mistake the fake runner cannot: a wrong flag, a
//! stdin/stdout contract mismatch, or - most importantly on Android - a helper
//! that *hangs* instead of failing.
//!
//! They are written to be honest on any host: a machine with no clipboard
//! bridge at all must return a typed error, not pass by accident, and must
//! return it within the configured budget. Nothing here asserts that a real
//! clipboard is *populated*, because CI and a headless Termux session have
//! none; what is asserted is the contract the TUI depends on - never panic,
//! never exceed the timeout, always a typed Result.

use std::time::{Duration, Instant};

use jcode_clipboard::{Clipboard, ClipboardError, Osc52Target};

/// A budget short enough to keep the suite fast, long enough for a healthy
/// local IPC round trip.
const BUDGET: Duration = Duration::from_millis(1_500);

#[test]
fn set_text_never_exceeds_its_timeout_and_never_panics() {
    // The load-bearing guarantee on Android: `termux-clipboard-set` is a
    // shell script that blocks forever when the Termux:API *addon app* is not
    // installed, even though the script itself exists and looks available.
    // That is not hypothetical - it is the state of a stock `pkg install
    // termux-api` without the companion APK. Without the hard timeout this
    // call would hang the TUI thread indefinitely.
    let clipboard = Clipboard::with_timeout(BUDGET);
    let started = Instant::now();
    let result = clipboard.set_text("jcode-clipboard live integration probe");
    let elapsed = started.elapsed();

    // Allow the kill/reap overhead on top of the budget, but nothing like a
    // hang: an unbounded `wait()` would sit here for the helper's lifetime.
    assert!(
        elapsed < BUDGET + Duration::from_secs(5),
        "set_text blocked for {elapsed:?}, budget was {BUDGET:?}: {result:?}"
    );
    match result {
        // A working bridge (Termux:API present, or wl-copy/xclip on a desktop).
        Ok(()) => {}
        // No bridge installed: the honest, typed outcome.
        Err(ClipboardError::NoMechanism) => {}
        // A bridge exists but did not answer in time - the case this crate
        // exists to survive.
        Err(ClipboardError::Timeout { limit_ms, .. }) => {
            assert_eq!(limit_ms, BUDGET.as_millis() as u64);
        }
        Err(ClipboardError::Failed(detail)) => {
            // Any other failure is acceptable as long as it is typed and names
            // the program; a bare panic or a hang is not.
            assert!(detail.contains(':'), "unhelpful failure detail: {detail}");
        }
        Err(other) => panic!("unexpected error from set_text: {other:?}"),
    }
}

#[test]
fn get_text_returns_a_typed_result_within_the_budget() {
    let clipboard = Clipboard::with_timeout(BUDGET);
    let started = Instant::now();
    let result = clipboard.get_text();
    let elapsed = started.elapsed();

    assert!(
        elapsed < BUDGET + Duration::from_secs(5),
        "get_text blocked for {elapsed:?}: {result:?}"
    );
    match result {
        Ok(text) => assert!(!text.is_empty(), "a successful read must carry text"),
        Err(
            ClipboardError::NoMechanism
            | ClipboardError::Empty { .. }
            | ClipboardError::NotUtf8 { .. }
            | ClipboardError::Timeout { .. }
            | ClipboardError::Failed(_)
            | ClipboardError::Unsupported(_),
        ) => {}
        Err(ClipboardError::Unencodable(reason)) => {
            panic!("unencodable is a write-only error, not a read one: {reason}")
        }
    }
}

#[test]
fn an_absurdly_short_timeout_still_returns_instead_of_hanging() {
    // Guards the deadline arithmetic: a 1ms budget must trip the timeout path
    // (or succeed instantly), not sit in the poll loop. This is the test that
    // would catch a regression to `wait_with_output`, which has no deadline.
    let clipboard = Clipboard::with_timeout(Duration::from_millis(1));
    let started = Instant::now();
    let _ = clipboard.set_text("tiny budget");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "a 1ms budget took {elapsed:?}; the deadline is not enforced"
    );
}

#[test]
fn osc52_write_reaches_a_buffer_with_the_exact_sequence() {
    // The one path that is pure this crate: no helper, no display, no
    // environment dependence. `stdout` is a Vec so the test cannot disturb
    // the terminal it runs in.
    let mut out: Vec<u8> = Vec::new();
    Clipboard::new()
        .write_osc52(&mut out, false, "live osc52")
        .expect("writes");
    let expected = jcode_clipboard::osc52::encode(Osc52Target::Clipboard, "live osc52")
        .expect("encodes");
    assert_eq!(out, expected.into_bytes());
    assert!(out.starts_with(b"\x1b]52;c;"), "wrong selector: {out:?}");
    assert!(out.ends_with(b"\x07"), "missing BEL terminator: {out:?}");
}

#[test]
fn the_clipboard_type_is_send_and_sync_so_it_can_be_shared() {
    // jcode-tui reads the clipboard from a background paste thread; a
    // !Send handle would not compile there.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Clipboard>();
    assert_send_sync::<ClipboardError>();
}

#[test]
fn repeated_failed_operations_do_not_leak_processes_or_stall() {
    // A buggy caller (or a copy keybinding held down) must not accumulate
    // zombie helpers. Ten calls under a 250ms budget must all return promptly;
    // if a killed child were not reaped, this would slow down or hang.
    let clipboard = Clipboard::with_timeout(Duration::from_millis(250));
    let started = Instant::now();
    for _ in 0..10 {
        let _ = clipboard.set_text("repeat");
        let _ = clipboard.get_text();
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(30),
        "20 clipboard calls took {elapsed:?}; a child is probably not being reaped"
    );
}
