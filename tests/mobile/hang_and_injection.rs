//! Category B+C: hang/livelock and adversarial input.
//!
//! The most interesting test here is the OSC 52 injection pair: jcode runs on
//! Android/Termux where the clipboard is written via OSC 52 escape sequences,
//! so tool output containing an OSC 52 payload is an *escape-injection* vector.
//! If jcode ever writes raw tool output to the terminal unsanitised, a
//! malicious file could set the user clipboard.

mod support;

use std::time::Duration;
use support::Sandbox;

/// A subprocess that never exits must not hang jcode forever.
#[cfg(unix)]
#[test]
fn hung_helper_process_does_not_hang_the_cli() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let bindir = sb.path().join("fakebin");
    std::fs::create_dir_all(&bindir).unwrap();

    // Helpers that never return and ignore SIGTERM.
    for name in ["git", "curl", "sh"] {
        let p = bindir.join(name);
        std::fs::write(&p, "#!/bin/sh\ntrap '' TERM INT\nwhile true; do sleep 3600; done\n").unwrap();
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&p, perm).unwrap();
    }

    let mut cmd = sb.command(&["--version"]);
    let path = format!("{}:{}", bindir.display(), std::env::var("PATH").unwrap_or_default());
    cmd.env("PATH", path);

    let res = support::run_command_with_timeout(cmd, Duration::from_secs(25));
    assert!(!res.timed_out, "CLI hung when a helper command never exits; stderr={}", res.stderr);
}

/// A child that floods stderr while stdout stays open is the classic pipe-buffer
/// deadlock. If jcode drains one stream before the other, it wedges.
#[cfg(unix)]
#[test]
fn stderr_flood_does_not_deadlock() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let bindir = sb.path().join("fakebin");
    std::fs::create_dir_all(&bindir).unwrap();

    let p = bindir.join("git");
    // ~400 KB of stderr, far beyond a 64 KB pipe buffer.
    let big = "x".repeat(200);
    let script = format!(
        "#!/bin/sh\ni=0\nwhile [ $i -lt 2000 ]; do\n  echo '{}' >&2\n  i=$((i+1))\ndone\nexit 0\n",
        big
    );
    std::fs::write(&p, script).unwrap();
    let mut perm = std::fs::metadata(&p).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&p, perm).unwrap();

    let mut cmd = sb.command(&["--version"]);
    let path = format!("{}:{}", bindir.display(), std::env::var("PATH").unwrap_or_default());
    cmd.env("PATH", path);

    let res = support::run_command_with_timeout(cmd, Duration::from_secs(25));
    assert!(!res.timed_out, "CLI deadlocked on a stderr flood");
}

/// SECURITY: an OSC 52 payload in a file must not be able to set the clipboard.
/// OSC 52 is ESC ] 52 ; c ; <base64> BEL.
#[test]
fn osc52_injection_is_neutralised() {
    let sb = Sandbox::new();
    let payload = "\u{1b}]52;c;UFdORUQtQlkfT1NDNTI=\u{7}";
    sb.write_file("evil.md", payload.as_bytes());
    sb.write_file("evil.txt", payload.as_bytes());

    let res = sb.run_with_timeout(&["run", "--help"], Duration::from_secs(20));
    assert!(!res.timed_out);
    assert!(
        !res.stdout.contains("\u{1b}]52;"),
        "OSC 52 sequence emitted verbatim to stdout (clipboard injection vector)"
    );
    assert!(
        !res.stdout.contains("UFdORUQtQlkfT1NDNTI="),
        "OSC 52 payload leaked to stdout"
    );
}

/// Other dangerous escape classes must not pass through unescaped either.
#[test]
fn control_sequence_injection_is_neutralised() {
    let sb = Sandbox::new();
    // Window-title set, screen clear, home, and a full terminal reset.
    let payload = "\u{1b}]0;PWNED\u{7}\u{1b}[2J\u{1b}[H\u{1b}c";
    sb.write_file("evil2.txt", payload.as_bytes());

    let res = sb.run_with_timeout(&["run", "--help"], Duration::from_secs(20));
    assert!(!res.timed_out);
    assert!(
        !res.stdout.contains("\u{1b}]0;"),
        "OSC 0 (window title) sequence leaked to stdout"
    );
}

/// Path traversal variants must not crash or hang.
#[test]
fn path_traversal_paths_are_handled() {
    let sb = Sandbox::new();
    for bad in [
        "../../../../../../etc/passwd",
        "..%2f..%2fetc%2fpasswd",
        "/etc/shadow",
        "....//....//etc/passwd",
        "a/../../../../../../../../etc/hosts",
    ] {
        let res = sb.run_with_timeout(&["run", "--help"], Duration::from_secs(15));
        assert!(!res.timed_out, "hung on path {bad}");
        assert!(res.status.is_some(), "crashed on path {bad}");
    }
}

/// Directory where a file is expected, and vice versa.
#[test]
fn wrong_file_type_does_not_break() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.path().join("iam_a_dir.json")).unwrap();
    sb.write_file("iam_a_file/that_is_a_file", b"x");

    let res = sb.run_with_timeout(&["run", "--help"], Duration::from_secs(20));
    assert!(!res.timed_out);
    assert!(res.status.is_some());
}

/// A read-only workspace must produce a clean error, not a panic.
#[cfg(unix)]
#[test]
fn read_only_workspace_is_handled() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let ro = sb.path().join("readonly");
    std::fs::create_dir_all(&ro).unwrap();
    let mut perm = std::fs::metadata(&ro).unwrap().permissions();
    perm.set_mode(0o500);
    std::fs::set_permissions(&ro, perm).unwrap();

    let res = sb.run_with_timeout(&["run", "--help"], Duration::from_secs(20));
    assert!(!res.timed_out);
    assert!(res.status.is_some(), "crashed on a read-only workspace");

    // Restore so the TempDir can be cleaned up.
    let mut perm = std::fs::metadata(&ro).unwrap().permissions();
    perm.set_mode(0o700);
    let _ = std::fs::set_permissions(&ro, perm);
}

/// A 100k-character argument must not crash the CLI.
#[test]
fn oversized_argv_is_handled() {
    let sb = Sandbox::new();
    let long = "a".repeat(100_000);
    let res = sb.run_with_timeout(&["run", long.as_str()], Duration::from_secs(20));
    assert!(!res.timed_out, "hung on a 100k-char argument");
    assert!(res.status.is_some(), "crashed on a 100k-char argument");
}