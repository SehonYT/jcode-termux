# Android / Termux documentation

This directory documents the Android port of jcode, maintained in
**[SehonYT/jcode-termux](https://github.com/SehonYT/jcode-termux)** — a fork of
[1jehuang/jcode](https://github.com/1jehuang/jcode).

Everything here is specific to the Android fork. For the agent itself, upstream
documentation applies.

## Start here

| If you want to... | Read |
|---|---|
| Install jcode on a phone | [INSTALL.md](INSTALL.md) |
| Know which ABI your device needs | [INSTALL.md](INSTALL.md#which-build-do-i-need) |
| Fix a problem | [INSTALL.md](INSTALL.md#troubleshooting) |
| Understand the build/CI | [BUILD.md](BUILD.md) |
| Run the robustness tests | [TESTING.md](TESTING.md) |
| Know what was changed and why | [PORT-AUDIT.md](PORT-AUDIT.md) |

## Contents

| Document | Contents |
|---|---|
| [INSTALL.md](INSTALL.md) | User-facing install guide, known limitations, troubleshooting |
| [BUILD.md](BUILD.md) | The four-ABI CI matrix, per-ABI linker and bindgen configuration |
| [HARDENING.md](HARDENING.md) | Crash/hang/lag fixes for memory-constrained phones |
| [TESTING.md](TESTING.md) | The adversarial mobile test suite |
| [CODEMODE.md](CODEMODE.md) | Enabling the QuickJS sandbox on Android |
| [PORT-AUDIT.md](PORT-AUDIT.md) | Every blocker found during the port, with file:line evidence |
| [TOOLCHAIN.md](TOOLCHAIN.md) | Target triple and toolchain investigation |

## Summary of the port

**The problem.** Upstream builds glibc binaries for desktop Linux and macOS.
Android uses bionic and has no `/lib64/ld-linux-*`, so upstream artifacts cannot
run. The installer's Termux path only patches the ELF interpreter for Termux
glibc, and only on aarch64/x86_64 — there is no 32-bit ARM path at all.

A second trap: `uname -m` on a 32-bit-only phone reports `armv8l`, which *looks*
64-bit. It is not. Such devices have an empty `ro.product.cpu.abilist64` and no
`/system/bin/linker64`, so an aarch64 binary cannot execute at all.

**The fix.** Build for Android targets (`*-linux-android*`) instead of glibc, and
remove the dependencies that have no Android support:

| Blocker | Why it breaks | Resolution |
|---|---|---|
| `aws-lc-sys` | No armv7-Android support; CMake/C/asm build | Use `ring` as the rustls provider |
| `openssl-sys` | `imap`'s default feature pulls `native-tls`; no 32-bit Android OpenSSL | `imap` uses its `rustls-tls` feature |
| `onig_sys` | C Oniguruma, no armv7-Android bindings | syntect uses the pure-Rust `regex-fancy` backend |
| `rquickjs-sys` | No pregenerated Android bindings | Enable its `bindgen` feature to generate them |
| `arboard` | Excludes Android in its own source | New pure-Rust `jcode-clipboard` crate |
| `azure_core` | Forces `reqwest/rustls` = `aws-lc-rs`; features are additive so it cannot be switched off | Compiled out on armv7, kept on aarch64 |

## How the fork is kept mergeable

Android changes are confined to:

- `cfg(target_os = "android")` target gates in existing crates,
- the new `crates/jcode-clipboard/`,
- `docs/android/` and `tests/mobile/`,
- `.github/workflows/termux-armv7.yml` (CI only).

So an `upstream/master` merge stays small. See the "Staying in sync" section of
the [README](../../README.md).