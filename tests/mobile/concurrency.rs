//! Category D: concurrency and race conditions.
//!
//! These are run under both low and high parallelism by scripts/test-mobile.sh.
//! Races here matter more on mobile: 8 cores with far less memory than a
//! desktop means contention windows are wider relative to available headroom.

mod support;

use std::sync::Arc;
use std::thread;
use std::time::Duration;
use support::Sandbox;

/// Many concurrent CLI invocations sharing one HOME must not corrupt state or
/// deadlock on a shared lock file.
#[test]
fn concurrent_invocations_sharing_home_are_safe() {
    let sb = Arc::new(Sandbox::new());
    let mut handles = Vec::new();
    for i in 0..8 {
        let sb = Arc::clone(&sb);
        handles.push(thread::spawn(move || {
            let res = sb.run_with_timeout(&["--version"], Duration::from_secs(30));
            assert!(res.status.is_some(), "concurrent run {i} died: {}", res.stderr);
            assert!(!res.timed_out, "concurrent run {i} hung");
        }));
    }
    for h in handles {
        h.join().expect("concurrent invocation thread panicked");
    }
}

/// Starting many short-lived processes in a tight loop stresses the scheduler
/// and any lock acquisition on the session store.
#[test]
fn rapid_start_stop_cycles_are_stable() {
    let sb = Sandbox::new();
    for i in 0..20 {
        let res = sb.run_with_timeout(&["--version"], Duration::from_secs(20));
        assert!(res.status.is_some(), "cycle {i} died: {}", res.stderr);
        assert!(!res.timed_out, "cycle {i} hung");
    }
}

/// A stale lock file left behind by a killed process must not wedge the CLI
/// forever. This is a realistic mobile scenario: Android kills apps at will.
#[test]
fn stale_lock_file_does_not_wedge() {
    let sb = Sandbox::new();
    let jh = sb.jcode_home();
    std::fs::create_dir_all(&jh).unwrap();
    // Plausible lock file names, left stale.
    for name in ["lock", "server.lock", ".lock", "sessions/.lock"] {
        let p = jh.join(name);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&p, b"");
    }

    let res = sb.run_with_timeout(&["--version"], Duration::from_secs(25));
    assert!(!res.timed_out, "CLI wedged on a stale lock file");
    assert!(res.status.is_some(), "CLI crashed on a stale lock file");
}

/// SIGKILL during startup (what Android does under memory pressure) must not
/// leave behind state that breaks the next run.
#[cfg(unix)]
#[test]
fn abrupt_kill_then_restart_recovers() {
    let sb = Sandbox::new();
    // Do not wait for completion: start and immediately kill.
    if let Ok(mut child) = sb.command(&["--version"]).spawn() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let res = sb.run_with_timeout(&["--version"], Duration::from_secs(25));
    assert!(!res.timed_out, "CLI hung after an abrupt kill");
    assert!(res.status.is_some(), "CLI crashed after an abrupt kill: {}", res.stderr);
}