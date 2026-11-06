# BUILD.md — Android / Termux build matrix

jcode cross-compiles for **all four Android ABIs** from a single CI workflow,
[`.github/workflows/termux-armv7.yml`](../.github/workflows/termux-armv7.yml). One matrix entry per
ABI carries every per-target difference (NDK clang wrapper name, cargo env-var
suffixes, expected ELF identity), so adding an ABI is one entry and no new step.

For the device-side story (why the official installer fails on a 32-bit-only
phone, how to install from a release) see [TERMUX.md](TERMUX.md). For the
original toolchain investigation see [TOOLCHAIN.md](TOOLCHAIN.md).

## The matrix

| Target triple | ABI | Devices | NDK clang wrapper | ELF |
|---|---|---|---|---|
| `armv7-linux-androideabi` | 32-bit ARM | `armv8l` / `armeabi-v7a` phones | `armv7a-linux-androideabi24-clang` | ELF32, ARM |
| `aarch64-linux-android` | 64-bit ARM | arm64 phones | `aarch64-linux-android24-clang` | ELF64, AArch64 |
| `i686-linux-android` | 32-bit x86 | x86 emulators, old x86 tablets | `i686-linux-android24-clang` | ELF32, Intel 80386 |
| `x86_64-linux-android` | 64-bit x86 | x86_64 emulators | `x86_64-linux-android24-clang` | ELF64, Advanced Micro Devices X86-64 |

API level **24** for every ABI (the Termux baseline).

> **`armv8l` is 32-bit ARM.** It is the 32-bit personality of an ARMv8 core, i.e.
> it is armv7. The armv7 lane therefore covers every `armv8l`/`armeabi-v7a` phone.
> A device that reports `abilist64` empty and has no `/system/bin/linker64`
> **cannot** execute an aarch64 binary — use the armv7 artifact there.

## Per-ABI naming traps

Three names differ per target and are easy to get wrong. They are all data
fields in the matrix, and the workflow re-derives and asserts them.

1. **The clang wrapper is named after the triple, not the ABI alias.**
   aarch64 → `aarch64-linux-android24-clang` (**not** `aarch64a-...`).
   armv7 is the odd one out: the Rust triple is `armv7-linux-androideabi` but the
   NDK/clang target string is `armv7a-linux-androideabi` (extra `a`).

2. **Cargo env-var suffixes use underscores of the triple**: `armv7_linux_androideabi`,
   `aarch64_linux_android`, `i686_linux_android`, `x86_64_linux_android`.
   cc-rs reads `CC_`/`CXX_`/`AR_`/`RANLIB_`/`CFLAGS_` with that suffix; the linker
   uses the uppercase form `CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER`.

3. **`BINDGEN_EXTRA_CLANG_ARGS_<suffix>` is mandatory, not optional.**
   codemode/QuickJS is enabled on Android through `rquickjs-sys`'s `bindgen`
   feature, because there are no pregenerated `src/bindings/<triple>.rs` for
   these ABIs. `rquickjs-sys`'s build script never passes `--target` itself; it
   relies entirely on this environment variable, which `bindgen` reads via its
   target-dependent lookup. Without it, libclang parses the **host** headers and
   emits bindings with the host's `size_t` width and struct layout — which usually
   still **compiles** and is then subtly wrong at runtime. The workflow hard-fails
   the lane if the variable is unset or does not carry `--target=<this ABI>`.

## What the workflow does

Three jobs:

1. **`graph-audit`** (matrix over all four ABIs). Runs `cargo tree` per target and
   fails fast when a crate known to be unbuildable for Android is reachable —
   before spending 20+ minutes compiling. Hard gates:
   - forbidden: `aws-lc-sys`, `aws-lc-rs`, `openssl-sys`, `native-tls`, `onig_sys`
   - required: `ring` (the rustls provider this port switched to)
   - required: `rquickjs-sys` (codemode is enabled; a silent drop is a regression)
   - required: `proctitle` (the process-title path calls it unconditionally)

   Every query passes `--target` **and** `-p jcode` **and** `--no-default-features`.
   Without all three, cargo resolves target-conditional dependencies for the
   host and reports false positives.

2. **`build`** (matrix over all four ABIs, `fail-fast: false`). Discovers the NDK,
   asserts the per-ABI clang wrapper exists, exports the cross environment,
   builds with `--release --locked --no-default-features`, then **verifies the ELF**
   and packages a `.tar.gz` per ABI.

3. **`release`** — only when `release_tag` is set and **all four** lanes passed, so
   a release never gets a partial ABI set. Downloads every artifact and attaches
   it with `--clobber` (idempotent reruns).

### ELF verification (hard-fail)

The class and machine are compared **exactly** against the matrix's expected
values, and `file(1)` is matched with a class-pinned regex:

```text
armv7  -> ELF32 / Machine ARM                        file: "ELF 32-bit LSB.*ARM"
aarch64-> ELF64 / Machine AArch64                    file: "ELF 64-bit LSB.*aarch64"
i686   -> ELF32 / Machine Intel 80386                file: "ELF 32-bit LSB.*Intel i386"
x86_64 -> ELF64 / Machine Advanced Micro Devices X86-64  file: "ELF 64-bit LSB.*x86-64"
```

Two subtleties, both verified against synthesized ELF headers:

- `file(1)` reports EM_386 as **Intel i386**, not "Intel 80386" (which is
  `readelf`'s spelling). Asking `file` for "80386" always fails.
- A bare `ARM` substring is **not** a valid check for the armv7 lane: `file(1)`
  describes an aarch64 binary as "ARM aarch64", so an unpinned `grep -i ARM`
  would happily pass a totally wrong artifact. The class word is pinned to stop
  exactly that.

The armv7 lane additionally rejects a hard-float binary
(`Tag_ABI_VFP_args: VFP registers`). Android armv7 is **softfp**: a hard-float
binary links fine and then SIGILLs on device. The workflow never passes
`-mfloat-abi`; the NDK clang wrapper already selects the correct ABI.

### Caching

`Swatinem/rust-cache@v2` keyed `android-<target>`, one cache per ABI. Sharing one
would thrash, since rlibs are per-target, and one poisoned entry would affect
every lane. Caching matters more now that QuickJS is compiled from vendored C on
every Android ABI (roughly +3-4 min per lane).

## Running it

Manual dispatch (`workflow_dispatch`) takes `features`, `no_default_features` and
`release_tag`. It also runs on pushes to `termux-armv7`/`termux` and on PRs that
touch the workflow or the manifests.

To attach every ABI to a release, dispatch with `release_tag` set:

```bash
gh workflow run termux-armv7.yml -f release_tag=v0.99.1-termux
```

Artifacts are named `jcode-<target>` and `jcode-<target>.tar.gz`, matching the
release asset names in [TERMUX.md](TERMUX.md).

## Building locally (on-device, one ABI)

The full workspace needs a lot of RAM and time. `CARGO_BUILD_RUSTC_WRAPPER=''` is
required: `.cargo/config.toml` pins a nightly-only rustc-wrapper (`-Zthreads` +
`RUSTC_BOOTSTRAP`) that breaks stable builds.

```bash
pkg install rust rust-std-armv7-linux-androideabi clang binutils git cmake
export CARGO_BUILD_RUSTC_WRAPPER=''
CARGO_BUILD_JOBS=2 cargo build --release --locked \
  --target armv7-linux-androideabi -p jcode --no-default-features
```

**Verify a graph claim** with all three selectors — anything less gives false
positives:

```bash
cargo tree -e normal --target armv7-linux-androideabi -p jcode --no-default-features -i <crate>
```

## Platform limitations (all Android ABIs)

- **Azure Entra ID auth** — `azure_core` hardcodes `reqwest/rustls`, which is
  `__rustls-aws-lc-rs`; cargo features are additive so it cannot be switched off.
  `jcode-azure-auth` is compiled out on Android.
- **Embeddings / local inference** — off under `--no-default-features`.
- **Global push-to-talk listener** — Linux-desktop only (evdev); not applicable.
- Cloud model providers, the TUI, sessions, codemode and the swarm all work.
