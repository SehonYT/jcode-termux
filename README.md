<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/jcode-mark-inverse.svg">
  <img src="assets/brand/jcode-mark.svg" alt="jcode halftone donut logo" width="160">
</picture>

# jcode for Android / Termux

<p>
  <strong>The full jcode coding agent, running natively on Android.</strong><br>
  Every Android ABI: 32-bit ARM, 64-bit ARM, and x86 emulators.
</p>

[![Build](https://github.com/SehonYT/jcode-termux/actions/workflows/termux-armv7.yml/badge.svg)](https://github.com/SehonYT/jcode-termux/actions/workflows/termux-armv7.yml)
[![Release](https://img.shields.io/github/v/release/SehonYT/jcode-termux?style=flat-square&label=release)](https://github.com/SehonYT/jcode-termux/releases)
[![Fork of](https://img.shields.io/badge/fork%20of-1jehuang%2Fjcode-blue?style=flat-square)](https://github.com/1jehuang/jcode)
[![License](https://img.shields.io/badge/license-MIT-green?style=flat-square)](LICENSE)

</div>

---

> ### This is a fork
>
> **Maintained by [@SehonYT](https://github.com/SehonYT)** (KR父TAJ).
>
> Upstream jcode is **[1jehuang/jcode](https://github.com/1jehuang/jcode)** by Jeremy Huang, and all
> credit for the agent itself belongs there. This fork exists for one reason: **upstream cannot run
> on Android.** It ships x86_64 and aarch64 glibc binaries only, so the official installer fails on
> every phone. This fork adds full Android support and maintains the Android-specific code, build
> CI, documentation and releases.
>
> For anything not Android-specific, use upstream.

## Install

On your phone, in Termux:

```bash
curl -fsSL https://raw.githubusercontent.com/SehonYT/jcode-termux/master/scripts/install-jcode-android.sh | bash
```

Or install a specific ABI manually from the
[latest release](https://github.com/SehonYT/jcode-termux/releases):

| Your device | File |
|---|---|
| 32-bit ARM (`armv8l`, `armeabi-v7a`) - most phones | `jcode-armv7-linux-androideabi.tar.gz` |
| 64-bit ARM (modern phones) | `jcode-aarch64-linux-android.tar.gz` |
| x86 emulator (32-bit) | `jcode-i686-linux-android.tar.gz` |
| x86 emulator (64-bit) | `jcode-x86_64-linux-android.tar.gz` |

Not sure which you need?

```bash
getprop ro.product.cpu.abilist
# armeabi-v7a,armeabi          -> armv7
# arm64-v8a                    -> aarch64
```

Then run `jcode login` and start it with `jcode`.

## Why upstream does not work on Android

Upstream releases are built against **glibc** for desktop Linux and macOS. Android uses **bionic**
and has no `/lib64/ld-linux-*`. The official installer’s Termux handling only patches the ELF
interpreter for Termux glibc, and only on aarch64/x86_64 - there is no 32-bit ARM path at all.

Worse, `uname -m` on a 32-bit-only phone reports `armv8l`, which *looks* 64-bit. It is not: these
devices have an empty `ro.product.cpu.abilist64` and no `/system/bin/linker64`, so an aarch64
binary **cannot execute at all**. The correct target is `armv7-linux-androideabi`.

## What this fork adds

### All Android ABIs
Four targets build in CI on every push, each asserting its own ELF class and machine:

| ABI | ELF |
|---|---|
| `armv7-linux-androideabi` | ELF32, Machine ARM |
| `aarch64-linux-android` | ELF64, Machine AArch64 |
| `i686-linux-android` | ELF32, Machine Intel 80386 |
| `x86_64-linux-android` | ELF64, Machine x86-64 |

### Features restored on Android
- **Codemode / QuickJS** works on Android. `rquickjs-sys` ships no Android bindings, but it
  vendors the QuickJS C source and has a `bindgen` feature that generates them at build time.
  Enabling that restores the sandbox instead of gating it out.
- **A working clipboard.** `arboard` has no Android backend at all, so this fork ships
  [`jcode-clipboard`](crates/jcode-clipboard), a pure-Rust (`#![forbid(unsafe_code)]`)
  implementation that drives Termux:API and falls back to OSC 52. Every subprocess has a hard
  timeout, because `termux-clipboard-set` will hang forever if the Termux:API APK is missing.

### Mobile hardening
Phones have no swap: running out of memory means an instant SIGKILL, not paging. This fork caps the
reads that could previously allocate unbounded memory - most importantly background task output,
which is written by a shell with no size limit and was read with `read_to_string` on every
inspection. See [`docs/android/HARDENING.md`](docs/android/HARDENING.md).

### An adversarial test suite
[`tests/mobile/`](tests/mobile) targets the failure modes that actually kill an agent on a phone:
memory exhaustion, hangs, terminal escape injection, races, and 32-bit truncation.
Run it with `scripts/test-mobile.sh`. Every test has a hard timeout so the suite cannot hang.

## Android documentation

| Document | Contents |
|---|---|
| [INSTALL.md](docs/android/INSTALL.md) | User guide, troubleshooting, known limitations |
| [BUILD.md](docs/android/BUILD.md) | The four-ABI build matrix and CI |
| [HARDENING.md](docs/android/HARDENING.md) | Mobile crash/hang/lag fixes |
| [TESTING.md](docs/android/TESTING.md) | The adversarial test suite |
| [CODEMODE.md](docs/android/CODEMODE.md) | How QuickJS is enabled on Android |
| [PORT-AUDIT.md](docs/android/PORT-AUDIT.md) | Every blocker found, with evidence |
| [TOOLCHAIN.md](docs/android/TOOLCHAIN.md) | Target/triple investigation |

## Known limitations on Android

- **Azure Entra ID auth** is unavailable on 32-bit ARM (`aws-lc-sys` has no armv7-Android support).
  It works on aarch64.
- **Image to clipboard** is unavailable; text works.
- **Process titles** are not set, so `ps` shows a generic name.
- **Local embeddings** are off (`--no-default-features`).

## Building

See [docs/android/BUILD.md](docs/android/BUILD.md). In short, CI cross-compiles with the Android
NDK; local builds need `CARGO_BUILD_RUSTC_WRAPPER=''` and are memory-hungry.

## Staying in sync with upstream

```bash
git remote add upstream https://github.com/1jehuang/jcode.git
git fetch upstream && git merge upstream/master
```

The Android changes are confined to target gates (`cfg(target_os = "android")`), the
`jcode-clipboard` crate, `docs/android/`, `tests/mobile/` and the CI workflow, so upstream merges
stay small.

---

## Upstream jcode

This fork tracks [1jehuang/jcode](https://github.com/1jehuang/jcode). Everything below is upstream’s
own description and remains accurate for the agent itself.

<details>
<summary><strong>About jcode (upstream)</strong></summary>

### Benches

#### RAM per additional session (headless)

| Tool | First session | Each additional session | Comparison (additional) |
|---|---|---|---|
| **jcode** | **71.3 MB** | **~3.3 MB** | baseline |
| **OpenCode** | 637.1 MB | ~5.9 MB | **1.8x more RAM** |
| **pi** | 84.0 MB | ~79.6 MB | **24.1x more RAM** |
| **Claude Code** | 272.9 MB | ~169.3 MB | **51.3x more RAM** |
| **Codex CLI** | 398.6 MB | ~194.1 MB | **58.8x more RAM** |

#### Time to first input

| Tool | Time to first input | Comparison |
|---|---|---|
| **jcode** | **19.1 ms** | baseline |
| **pi** | 596.4 ms | **31.2x slower** |
| **Codex CLI** | 905.8 ms | **47.4x slower** |
| **OpenCode** | 1047.9 ms | **54.9x slower** |
| **Claude Code** | 3512.8 ms | **183.9x slower** |

### Swarm

Spawn two or more agents in the same repo and the server manages collaboration between them: when
agent A edits a file agent B has read, B is notified so it can check the diff for conflicts. Agents
can message one agent, broadcast to all agents in a repo, or spawn their own sub-swarms.

### OAuth and providers

Supports a wide range of providers including Claude OAuth and OpenAI Codex OAuth. See the
[upstream docs](https://jcode.sh/docs#providers).

### Self-dev

jcode can modify its own source, rebuild, reload its binary and continue work across sessions.

Links: [Token optimization](docs/HARNESS.md#token-optimization) ·
[KV cache](docs/HARNESS.md#kv-cache) ·
[Upstream repository](https://github.com/1jehuang/jcode)

</details>

## License

MIT, inherited from [1jehuang/jcode](https://github.com/1jehuang/jcode).
Original copyright (c) 2025 Jeremy Huang. Android port additions (c) 2026 KR父TAJ (@SehonYT).
