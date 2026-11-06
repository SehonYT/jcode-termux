# Mobile / Android adversarial test suite

jcode runs on Android phones with ~1.4-3.8 GB RAM, where the failure modes are
different from a desktop: the process is **SIGKILLed** by the low-memory killer
instead of swapping, terminals can be killed mid-write, and subprocesses hang
for reasons that do not occur on a normal Linux box.

This suite exists to make those failures **reproducible and loud** rather than
occasional field reports.

## Running it

```bash
scripts/test-mobile.sh              # full suite
scripts/test-mobile.sh --quick      # skip heavy tests
scripts/test-mobile.sh --soak       # include long-running soak tests
scripts/test-mobile.sh --parallel   # high test-thread count (race hunting)
scripts/test-mobile.sh --serial     # single-threaded (ordering bugs)
```

Individual suites:

```bash
cargo test --test mobile_resource_exhaustion
cargo test --test mobile_hang_and_injection
cargo test --test mobile_concurrency
cargo test --test mobile_thirtytwo_bit
```

## Design rules

1. **The suite can never hang.** Every subprocess wait goes through
   `support::run_command_with_timeout`, which kills the child on expiry. The
   runner script adds a second, outer `timeout` per suite. This matters because
   a hung child is one of the things under test.
2. **Tests exercise the real binary**, via `env!("CARGO_BIN_EXE_jcode")`, not a
   library shortcut. Only the shipped artifact's behaviour counts.
3. **Hermetic.** Every test gets an isolated `HOME`/`JCODE_HOME` in a temp dir,
   with `env_clear()`, `JCODE_NO_TELEMETRY=1` and `TERM=dumb`, so tests are
   parallel-safe and never touch developer state.
4. **A failing test that exposes a real bug is a success.** Never weaken a test
   to make it pass; report the bug instead.

## What each suite covers

### `mobile_resource_exhaustion` - OOM, the #1 mobile crash

| Test | Proves |
|---|---|
| `large_file_read_does_not_oom` | A 64 MiB file present in the workspace does not get the process OOM-killed (an OOM kill surfaces as *no exit status*). |
| `enormous_single_line_does_not_oom_or_hang` | A 32 MiB line with no newline - the classic log-tailer/TUI killer - is survivable. |
| `deeply_nested_json_does_not_stack_overflow` | 200k-deep JSON does not blow the parser stack (a stack overflow is SIGSEGV). |
| `fifo_where_file_expected_does_not_hang` | A named pipe where a regular file is expected cannot block forever. |
| `symlink_loop_does_not_hang` | Mutually-recursive symlinks terminate. |
| `file_growing_during_read_terminates` | Reading a file that grows does not loop forever. |
| `malformed_inputs_do_not_panic` | Empty, truncated, NUL-filled, BOM-prefixed and invalid-UTF-8 inputs do not crash. |
| `repeated_invocations_do_not_exhaust_fds` | 12 sequential runs do not leak file descriptors (Android has a lower fd limit). |

### `mobile_hang_and_injection` - hangs and terminal escape injection

| Test | Proves |
|---|---|
| `hung_helper_process_does_not_hang_the_cli` | A helper on `PATH` that never exits **and ignores SIGTERM** cannot wedge the CLI. |
| `stderr_flood_does_not_deadlock` | A child writing ~400 KB to stderr while stdout stays open does not cause the classic pipe-buffer deadlock. |
| `osc52_injection_is_neutralised` | **Security.** A file containing an OSC 52 payload must not be able to set the user's clipboard. |
| `control_sequence_injection_is_neutralised` | Window-title/cursor/terminal-reset sequences do not pass through raw. |
| `path_traversal_paths_are_handled` | Traversal variants do not crash or hang. |
| `wrong_file_type_does_not_break` | Directory-where-file-expected and vice versa are handled. |
| `read_only_workspace_is_handled` | A read-only cwd produces a clean error, not a panic. |
| `oversized_argv_is_handled` | A 100k-character argument is survivable. |

The OSC 52 test deserves emphasis: this port runs on Termux where the clipboard
is written **via OSC 52**, so a tool output containing that sequence is a
clipboard-injection vector. Raw tool output must never reach the terminal
un-neutralised.

### `mobile_concurrency` - races and recovery

| Test | Proves |
|---|---|
| `concurrent_invocations_sharing_home_are_safe` | 8 concurrent runs sharing one `HOME` do not corrupt state or deadlock on a lock. |
| `rapid_start_stop_cycles_are_stable` | 20 start/stop cycles are stable. |
| `stale_lock_file_does_not_wedge` | A lock file left behind by a killed process does not wedge the next run. |
| `abrupt_kill_then_restart_recovers` | SIGKILL during startup (what Android does) does not poison subsequent runs. |

### `mobile_thirtytwo_bit` - armv7 truncation

| Test | Proves |
|---|---|
| `sparse_file_over_4gib_does_not_truncate` | A sparse file just over 4 GiB does not wrap `u64 -> usize` (32-bit only). |
| `boundary_values_do_not_wrap` | Values at 2^31, 2^32 and i64::MAX boundaries do not wrap or flip sign. |
| `pointer_width_assumption_is_recorded` | Records the pointer width so the run is self-describing. |

On armv7 `usize` is 32 bits, so any `u64 -> usize` conversion silently
truncates above 4 GiB - and a >4 GiB file is trivially reachable on a phone with
a large SD card. These are realistic bugs, not theoretical ones.

## Adding a test

Use the `support` module - do not hand-roll process handling:

```rust
mod support;
use support::Sandbox;

#[test]
fn my_case() {
    let sb = Sandbox::new();
    sb.write_file("input.json", b"...");
    let res = sb.run_with_timeout(&["--version"], std::time::Duration::from_secs(20));
    assert!(!res.timed_out, "hung!");
    assert!(res.status.is_some(), "crashed: {}", res.stderr);
}
```

`status.is_some()` is the key assertion for crash detection: a process killed by
a signal (OOM, SIGSEGV) has **no exit code**, so `None` means it died.