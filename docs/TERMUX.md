# jcode on Termux / 32-bit ARM Android (armv8l)

This fork adds support for running **jcode** on Termux on **32-bit-only ARM Android
devices** — the `armv8l` devices that report an `armeabi-v7a` ABI.

Upstream `1jehuang/jcode` ships prebuilt binaries only for:
`linux-x86_64`, `linux-aarch64`, `macos-aarch64`, `macos-x86_64`,
`windows-x86_64`, `windows-aarch64`.

**None of these can run on a 32-bit-only ARM device.** See
[Why the official installer fails](#why-the-official-installer-fails) below.

## Is my device affected?

```bash
getprop ro.product.cpu.abilist     # if this has no 64-bit entry, you need this fork
getprop ro.product.cpu.abilist64   # if this is EMPTY, you are 32-bit only
```

If `abilist64` is empty, or `/system/bin/linker64` does not exist, an aarch64 binary
**cannot** execute on your device — even if `uname -m` says `aarch64` or `armv8l`.

## Why the official installer fails

`curl -fsSL https://jcode.sh/install | bash` detects your platform as one of the six
supported targets above. On a 32-bit ARM Android device:

1. `uname -m` reports `armv8l`, which the installer does not recognize as any supported
   architecture, so it errors with an unsupported-architecture message, **or**
2. it resolves to `linux-aarch64` and downloads a 64-bit binary that the kernel cannot
   execute (`Exec format error`).

The installer's Termux handling only patches the ELF interpreter for **Termux glibc**,
and only for `aarch64`/`x86_64`. There is no 32-bit ARM path, and glibc is usually not
installed.

## The correct target

`armv7-linux-androideabi` — **Android/bionic**, not a glibc triple.

| Triple | Verdict |
|---|---|
| `armv7-linux-androideabi` | **Correct.** Rust std is prebuilt upstream. |
| `arm-linux-androideabi` | Legacy ARMv5 baseline — avoid. |
| `thumbv7neon-linux-androideabi` | Valid for the CPU, but Termux does not package std. |
| `aarch64-linux-android` | **Cannot run** on a 32-bit-only device. |

The resulting binary is:

```
ELF 32-bit LSB pie executable, ARM, EABI5, interpreter /system/bin/linker
```

## Installing

### From a CI-built release (recommended)

```bash
pkg install curl tar
curl -fsSL -o jcode.tar.gz \
  https://github.com/SehonYT/jcode-termux/releases/latest/download/jcode-armv7-linux-androideabi.tar.gz
tar xzf jcode.tar.gz
mkdir -p ~/.local/bin
mv jcode-armv7-linux-androideabi ~/.local/bin/jcode
chmod +x ~/.local/bin/jcode
jcode --version
```

Make sure `~/.local/bin` is on your `PATH`:

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
```

### From source (on-device)

Building the full 90-crate workspace on-device needs a lot of RAM and time. If you want
to try:

```bash
pkg install rust rust-std-armv7-linux-androideabi clang binutils git cmake
git clone https://github.com/SehonYT/jcode-termux
cd jcode-termux
CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_JOBS=1 \
  cargo build --release --target armv7-linux-androideabi -p jcode --no-default-features
```

`CARGO_BUILD_JOBS=1` matters: `-Zthreads` is not available on stable, and parallel rustc
jobs will exhaust RAM on a phone.

## Building in CI

The cross-compile lives in
[`.github/workflows/termux-armv7.yml`](.github/workflows/termux-armv7.yml) and now builds
**all four Android ABIs** — see [BUILD.md](BUILD.md) for the full matrix. It:

1. **Gate 1 — dependency graph audit.** Fails fast if any crate known to be unbuildable
   for Android is reachable, before spending 20+ minutes compiling.
2. **Gate 2 — cross build** with the Android NDK, `--no-default-features`, then asserts the
   artifact really is the requested `class`/`machine` (and explicitly fails on a mismatch).

## What was changed for this port

Five blockers, all verified with
`cargo tree -e normal --target armv7-linux-androideabi -p jcode --no-default-features -i <crate>`:

| Blocker | Why it breaks | Fix |
|---|---|---|
| `aws-lc-rs` as the rustls provider | `aws-lc-sys` has no 32-bit Android ARM bindings and is a CMake/C build | Use `ring` (already in the lock and used elsewhere in the workspace) |
| `azure_core` | Forces `reqwest/rustls`, which is `__rustls-aws-lc-rs`; cargo features are additive so it cannot be switched off | `jcode-azure-auth` is compiled out on Android |
| `imap` (default `native-tls`) | → `openssl-sys`, which needs a system OpenSSL that does not exist for this target | `imap` uses its `rustls-tls` feature instead |
| `syntect` `regex-onig` | Links C Oniguruma (`onig_sys`) | Switched to the pure-Rust `regex-fancy` backend |
| `rquickjs-sys` (QuickJS) | No pregenerated bindings for this ABI | Extended the crate's existing FreeBSD cfg gate to Android — codemode reports itself unavailable |

Build with `--no-default-features`, which cleanly drops the `pdf` and `bedrock` (AWS SDK)
feature chains.

## Known limitations on this platform

- **Codemode** is unavailable (QuickJS is not compiled).
- **Azure Entra ID auth** is unavailable.
- **Process titles** are not set, so `ps` shows a generic name (cosmetic).
- **Embeddings / local inference** are off (`--no-default-features`).
- Cloud model providers, the TUI, sessions, and the swarm all work as normal.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `Exec format error` | You are running a 64-bit (aarch64) binary. Check with `readelf -h jcode \| grep Class` — it must say `ELF32`. |
| `No such file or directory` when the file clearly exists | Wrong ELF interpreter. The 32-bit Android loader is `/system/bin/linker`; verify with `readelf -l jcode \| grep interpreter`. |
| Link errors mentioning `-lm`/`-ldl` | You built for a glibc triple. Use `armv7-linux-androideabi`. |
| Build dies in `aws-lc-sys` or `openssl-sys` | `--no-default-features` was not passed, or the source patches are missing. |
| Build dies in `rquickjs-sys` / `onig_sys` | Same as above — these must be gated out for Android. |
| Runtime crash with hard-float builds | Never pass `-mfloat-abi=hard`; Android armv7 is **softfp**. |

## Notes for maintainers

See [`docs/AUDIT.md`](docs/AUDIT.md) for the full blocker analysis with file:line references,
and [`docs/TOOLCHAIN.md`](docs/TOOLCHAIN.md) for the toolchain investigation.

One trap worth repeating: `.cargo/config.toml` sets `rustc-wrapper` to
`scripts/rustc-parallel-frontend`, which injects `RUSTC_BOOTSTRAP=1` and the nightly-only
`-Zthreads`. Any stable cross build must set `CARGO_BUILD_RUSTC_WRAPPER=''`.
