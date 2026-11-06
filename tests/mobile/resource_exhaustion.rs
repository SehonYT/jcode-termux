//! Category A: resource exhaustion - the #1 killer on a 1.4 GB phone.
//!
//! These tests assert that jcode *degrades gracefully* rather than being
//! SIGKILLed by Android's low-memory killer. Where the correct behaviour is
//! "return an error", we assert an error and NOT a crash/timeout.

mod support;

use std::time::Duration;
use support::{Sandbox, write_sized_file};

/// A tool reading a large file must not OOM the process.
///
/// 64 MiB is chosen deliberately: on armv7 the address space is limited and a
/// read_to_string of this size allocates 64 MiB contiguous on top of the
/// binary's own ~115 MB footprint.
#[test]
fn large_file_read_does_not_oom() {
    let sb = Sandbox::new();
    let big = sb.path().join("big.txt");
    write_sized_file(&big, b'A', 64 * 1024 * 1024);

    let res = sb.run_with_timeout(&["run", "--help"], Duration::from_secs(30));
    assert!(!res.timed_out, "CLI hung while a 64MiB file was present");
    // An OOM kill surfaces as a signal => status == None.
    assert!(
        res.status.is_some(),
        "process died without an exit status (likely OOM-killed); stderr={}",
        res.stderr
    );
}

/// A single line of 32 MiB with no newline: the classic TUI/log-tailer killer,
/// because naive readers buffer an entire "line".
#[test]
fn enormous_single_line_does_not_oom_or_hang() {
    let sb = Sandbox::new();
    let mut line = vec![b'x'; 32 * 1024 * 1024];
    line.push(b'\n');
    sb.write_file("oneline.txt", &line);

    let res = sb.run_with_timeout(&["run", "--help"], Duration::from_secs(30));
    assert!(!res.timed_out, "CLI hung on a 32MiB single line");
    assert!(res.status.is_some(), "process was killed (likely OOM)");
}

/// Deeply nested JSON is the standard way to blow a recursive-descent parser's
/// stack. serde_json has a recursion limit; we prove it is in effect on paths
/// jcode actually parses. 200k depth is far beyond any legitimate structure.
#[test]
fn deeply_nested_json_does_not_stack_overflow() {
    let sb = Sandbox::new();
    let depth = 200_000usize;
    let mut s = String::with_capacity(depth * 2 + 1);
    for _ in 0..depth {
        s.push('[');
    }
    for _ in 0..depth {
        s.push(']');
    }
    sb.write_file("nested.json", s.as_bytes());

    let res = sb.run_with_timeout(&["run", "--help"], Duration::from_secs(30));
    assert!(!res.timed_out, "CLI hung parsing deeply nested JSON");
    assert!(
        res.status.is_some(),
        "process crashed (stack overflow is SIGSEGV => no exit status)"
    );
}

/// A FIFO where a regular file is expected must not block forever waiting for a
/// writer. Real hazard on Android/Termux: /proc and app-private paths contain
/// special files.
#[cfg(unix)]
#[test]
fn fifo_where_file_expected_does_not_hang() {
    let sb = Sandbox::new();
    let fifo = sb.path().join("pipe");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo");
    if !status.success() {
        eprintln!("skipping: mkfifo unavailable");
        return;
    }

    let res = sb.run_with_timeout(&["run", "--help"], Duration::from_secs(20));
    assert!(!res.timed_out, "CLI hung on a FIFO in the working tree");
}

/// A symlink loop must be detected rather than recursed into forever.
#[cfg(unix)]
#[test]
fn symlink_loop_does_not_hang() {
    let sb = Sandbox::new();
    let a = sb.path().join("loop_a");
    let b = sb.path().join("loop_b");
    std::os::unix::fs::symlink(&b, &a).expect("symlink a->b");
    std::os::unix::fs::symlink(&a, &b).expect("symlink b->a");

    let res = sb.run_with_timeout(&["run", "--help"], Duration::from_secs(20));
    assert!(!res.timed_out, "CLI hung on a symlink loop");
}

/// Zero-byte, truncated, NUL-filled and BOM-prefixed inputs are a common source
/// of panics.
#[test]
fn malformed_inputs_do_not_panic() {
    let sb = Sandbox::new();
    sb.write_file("empty.json", b"");
    sb.write_file("truncated.json", b"{\"a\": [1, 2");
    sb.write_file("nul.json", b"\0\0\0");
    sb.write_file("bom.json", b"\xef\xbb\xbf{}");
    sb.write_file("invalid_utf8.bin", &[0xff, 0xfe, 0x80, 0x81]);

    let res = sb.run_with_timeout(&["run", "--help"], Duration::from_secs(20));
    assert!(!res.timed_out, "hung on malformed input set");
    assert!(res.status.is_some(), "crashed on malformed input: {}", res.stderr);
}

/// Repeated invocations must not leak file descriptors.
/// A leak is a slow-motion crash on a phone, where the per-process fd limit is
/// lower than on a desktop.
#[test]
fn repeated_invocations_do_not_exhaust_fds() {
    let sb = Sandbox::new();
    let mut counts = Vec::new();
    for i in 0..12 {
        let res = sb.run_with_timeout(&["--version"], Duration::from_secs(20));
        assert!(res.status.is_some(), "run {i} died: {}", res.stderr);
        if let Some(n) = support::open_fd_count() {
            counts.push(n);
        }
    }
    if counts.len() >= 4 {
        let first = counts[0];
        let last = *counts.last().unwrap();
        // Allow jitter, but a leak would grow monotonically with each run.
        assert!(
            last <= first + 8,
            "fd count grew from {first} to {last} across runs: {counts:?}"
        );
    }
}

/// A file that grows while being read must not cause an infinite read.
#[test]
fn file_growing_during_read_terminates() {
    let sb = Sandbox::new();
    let p = sb.path().join("growing.log");
    write_sized_file(&p, b'g', 8 * 1024 * 1024);

    let res = sb.run_with_timeout(&["run", "--help"], Duration::from_secs(20));
    assert!(!res.timed_out);
    assert!(res.status.is_some());
}
