# Mobile hardening: crash, hang and lag

jcode runs on Android phones. That changes the failure modes in three ways that
matter more than anything else:

1. **Crash** - there is no swap. When memory runs out the process is **SIGKILLed**
   by the low-memory killer, instantly and without a core dump. A desktop would
   simply page.
2. **Hang** - there is no controlling terminal and no `/proc` guarantees. Child
   processes hang for reasons that do not occur on a normal Linux box, and a
   hung `git` or `curl` takes the agent down with it.
3. **Lag** - a phone has 8 cores but a fraction of a desktop's per-core budget.
   Work that is invisible on a laptop is visible as input lag here.

This document records every hardening fix, the evidence that motivated it, and how
to verify it.

## 1. Unbounded reads -> OOM (the #1 mobile crash)

### 1.1 Background task output files

`crates/jcode-base/src/background.rs`

**Failure mode.** A background task's output file is written by a shell with no
size limit. `yes`, a runaway build log, or a stuck network client can produce
gigabytes in minutes. Every reader in this file used `fs::read_to_string`, so
merely *inspecting* such a task - the ordinary `bg action=output`, `bg action=wait`,
and completion-notification paths - allocated the **entire file**. On Android that
is an immediate SIGKILL.

**Fix.** Added `read_file_tail_capped()` / `read_file_tail_capped_with_limit()`
(cap: `MAX_BACKGROUND_OUTPUT_READ_BYTES` = 4 MiB) and routed the output-reading
paths through it. The cap reads the **tail**, not the head, deliberately:

- the renderers want a bounded tail (`output_preview`, 50 KB / N lines), and
- the exit-code marker the wrapper appends lives at the **end** of the file, so a
  head-truncation would break task completion detection.

A leading partial line is trimmed so a truncated tail never surfaces half a line.
The read is bounded with `.take()` rather than trusting the earlier `metadata()`
length, because a live background task keeps appending while we read.

**Verified.** `cargo check --target armv7-linux-androideabi -p jcode-base`.
Behavioural coverage: `tests/mobile/resource_exhaustion.rs`.

### 1.2 Child-process stdout (credential helper)

`crates/jcode-base/src/auth/claude.rs`

**Failure mode.** After a helper process exits, its stdout was drained with an
uncapped `read_to_string`. The child is not ours; a misbehaving helper can emit
unbounded output and OOM the parent.

**Fix.** Bound the read with `.take(1 MiB)`. A credential blob is a few hundred
bytes, so 1 MiB is far above any legitimate response while bounding the worst case.

## 2. Hangs

### 2.1 Credential helper timeout (already present, now bounded on both axes)

The macOS Keychain helper at `claude.rs` already polled with a 5 s timeout and
killed the child on expiry. With the read now capped, both the *time* and the
*memory* of that call are bounded - previously a helper that exited quickly after
emitting gigabytes would still have OOM-killed us.

### 2.2 Subprocesses without timeouts

**Hazard.** Android has no controlling terminal. A child that reads stdin, or that
blocks on network/DNS, will wait forever. Any `Command::output()` / `.wait()` /
`.status()` without a watchdog is a hang.

**Status.** The call sites audited in the auth and background paths either use a
poll-with-timeout loop (see 2.1) or are short-lived and bounded. This is not an
exhaustive proof: a full sweep of every `Command` in a 90-crate workspace needs
the process-level tests in `tests/mobile/hang_and_injection.rs` to act as the
regression net.

### 2.3 Pipe-buffer deadlock

**Hazard.** If a parent reads one stream to completion before draining the other,
a child that fills the undrained pipe's 64 KB buffer blocks forever - and so does
the parent. Classically triggered by a command that logs heavily to stderr while
stdout is quiet.

**Coverage.** `stderr_flood_does_not_deadlock` reproduces exactly this shape against
the real binary.

## 3. Lag

### 3.1 Idle CPU burn

jcode ships an idle animation with configurable FPS (`JCODE_IDLE_ANIMATION`,
`JCODE_ANIMATION_FPS`). On a phone, a continuously animating TUI is a battery drain
and a source of input latency. Operators on mobile should disable it:

```bash
export JCODE_IDLE_ANIMATION=0
```

This is a configuration answer rather than a code change, because the animation is
a deliberate desktop feature and disabling it by default for Android would be a
behaviour regression on other platforms.

### 3.2 Startup cost

The armv7 binary is ~113 MB unstripped. Startup is dominated by dynamic linking and
relocation of a large PIE on a slower CPU. `jcode --version` is the cheapest probe;
measure it before and after any startup-affecting change.

## 4. What is NOT hardened (honest limits)

- **No global memory ceiling.** There is no allocator-level cap that degrades
  gracefully; the caps above are point fixes at the highest-risk readers. A general
  solution would need a custom global allocator that returns errors under pressure,
  which is a much larger change and would affect every platform.
- **No systematic `Command` timeout wrapper.** Audit above is partial.
- **Tokio worker count** is not yet tuned for phone core/memory ratios.
- **OOM behaviour of the TUI transcript buffer** under multi-megabyte sessions was
  not measured on-device.

These are the next candidates. Each would benefit from a test in
`tests/mobile/` so it cannot regress.