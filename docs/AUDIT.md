# Jcode 32-bit ARM Android (Termux / armv7) Compatibility Audit

**Target:** `armv7-linux-androideabi` (or `thumbv7neon-linux-androideabi`), bionic libc, Termux prefix `/data/data/com.termux/files/usr`, 32-bit only (abilist=`armeabi-v7a,armeabi`, abilist64 empty).
**Source:** `repo/` @ master `c22c4aa`, 89 workspace crates, 1002 lockfile packages.
**Auditor:** teammate `auditor` (shared task `task-2`).
**Method:** manifest + lockfile reverse-dependency tracing, in-tree source grep, and extraction of the *actual crate tarballs* from static.crates.io to verify platform support rather than relying on reputation.

---

## 0. Executive summary

The single most important structural fact: **the heavy stacks are correctly optional**, but **three C-toolchain dependencies are NOT optional and are hard-wired into the always-compiled core**, so `--no-default-features` alone will **not** produce a building binary.

| Category | Verdict |
|---|---|
| aws-lc-rs / aws-lc-sys (32-bit ARM Android) | **BLOCKER** — fixed by lead in task-3 (aws_lc_rs → ring) |
| **rquickjs-sys / QuickJS** | **BLOCKER, unfixed** — no android bindings exist upstream; hard dep of app-core |
| **syntect `regex-onig` / onig_sys** | **BLOCKER, unfixed** — C Oniguruma; pure-Rust alternative exists |
| **`.cargo/config.toml` rustc-wrapper** | **BLOCKER for CI** — nightly-only `-Zthreads` + `RUSTC_BOOTSTRAP=1` in a stable cross build |
| pdf / bedrock defaults | **BLOCKER** — drop via `--no-default-features` (clean, no source changes) |
| libsqlite3-sys (bundled), zstd-sys | **MAJOR** — C, but armv7-clean; just needs correct NDK `CC`/`AR` env |
| `proctitle` | **NOT an issue** — see §3.5; pure Rust, already no-op on Android |
| jemalloc | **Safe** — not in any default feature set |
| 64-bit assumptions (`target_pointer_width`) | **Safe** — zero occurrences in src/ and crates/ |

---

## 1. Issue table

Severity key: **BLOCKER** = build or runtime cannot succeed without this change. **MAJOR** = will likely fail or misbehave in CI without a fix/flag. **MINOR** = cosmetic, latent, or test-only.

| # | Severity | Issue | file:line | Concrete fix |
|---|---|---|---|---|
| 1 | **BLOCKER** | `rustls` pinned to `aws_lc_rs`; aws-lc-sys 0.45.0 has **no** armv7-android entry in its pregenerated-bindings allowlist (it lists `aarch64-linux-android` only; its only 32-bit ARM entry is `armv7-unknown-linux-ohos`) and is a CMake+C build. | `repo/Cargo.toml:149`; `crates/jcode-provider-openai-runtime/Cargo.toml:33,35`; `crates/jcode-voice/Cargo.toml:29` | **DONE (lead, task-3):** provider switched to `ring`. ring 0.17.14 is already in the lock and supports armv7-android. |
| 2 | **BLOCKER** | Runtime provider install calls the aws-lc-rs constructor by name (compile error if the feature is dropped). | `src/main.rs:107`; `crates/jcode-voice/src/nari.rs:337`; 6 test files under `crates/jcode-provider-openai-runtime/src/` | **DONE (lead, task-3):** `rustls::crypto::ring::default_provider()`. |
| 3 | **BLOCKER** | `quinn-proto 0.11.18` depends on **both** `aws-lc-rs` **and** `ring` unconditionally, and is pulled by `reqwest 0.12.28`. Feature unification can therefore keep aws-lc-sys in the graph even after all jcode manifests are switched to ring. | `Cargo.lock` (reqwest → quinn → quinn-proto → aws-lc-rs) | **Verify first, then decide:** run `cargo tree -e features -i aws-lc-sys --target armv7-linux-androideabi` as the **first CI gate**. reqwest does not enable HTTP/3 by default, so quinn is probably a lock-only entry and never compiled — but this must be proven, not assumed. If it *is* built, disable reqwest's http3 path or vendor a `[patch]` for quinn-proto. |
| 4 | **BLOCKER** | `default = ["pdf", "bedrock"]` pulls the AWS SDK (~38 aws-*/aws-smithy-* crates) and PDF extraction. | `repo/Cargo.toml:224`; `crates/jcode-tui/Cargo.toml:95`; `crates/jcode-app-core/Cargo.toml:110`; `crates/jcode-base/Cargo.toml:128` | Build with `--no-default-features`. The forwarding chain is clean and `jcode-base` already declares `jcode-provider-bedrock` with `default-features = false` (`crates/jcode-base/Cargo.toml:74`), so **no source patching is required**. |
| 5 | **BLOCKER** | **`rquickjs-sys` (QuickJS C engine).** Verified from the tarball: it ships exactly **14** bundled binding files and **none** match android — no `armv7-linux-androideabi.rs`, no `aarch64-linux-android.rs`. Its `build.rs:269-280` detects the mismatch and only emits a *warning*, then proceeds to compile QuickJS anyway via `cc::Build` (`build.rs:157`). It is reached unconditionally: `jcode-app-core` → `jcode-codemode` is a **non-optional** dependency. | dep: `crates/jcode-app-core/Cargo.toml:100`; use: `crates/jcode-app-core/src/tool/codemode.rs:14,76,234`; manifest: `crates/jcode-codemode/Cargo.toml:25` | Mirror the **existing FreeBSD pattern** at `crates/jcode-codemode/Cargo.toml:22-25`: add `android` to that `cfg(not(target_os = "freebsd"))` exclusion so rquickjs is not compiled, and make `jcode_codemode::AVAILABLE` false on Android (`crates/jcode-app-core/src/tool/codemode.rs:76` already gates on it). This is exactly the escape hatch the crate author already designed for unsupported platforms. |
| 6 | **BLOCKER** | **`syntect` with `regex-onig`** compiles vendored C Oniguruma (`onig_sys`). Reached unconditionally via `jcode-tui-markdown`, which is a non-optional dep of `jcode-tui`. | `crates/jcode-tui-markdown/Cargo.toml:18` | syntect 5.3.0 exposes a **pure-Rust** alternative, `regex-fancy` → `fancy-regex` (verified in the extracted tarball's `[features]`: `regex-fancy = ["fancy-regex"]`). Change `regex-onig` → `regex-fancy`. The workspace is *already* prepared for this: `crates/jcode-tui-markdown/tests/highlight_backend_probe.rs:1` is an existing probe comparing the two regex backends. |
| 7 | **BLOCKER** | `.cargo/config.toml` sets `rustc-wrapper = "scripts/rustc-parallel-frontend"`, a POSIX sh wrapper that injects `RUSTC_BOOTSTRAP=1` and the **nightly-only** `-Zthreads` flag. It will break a stable cross-compile. | `repo/.cargo/config.toml:20`; wrapper at `scripts/rustc-parallel-frontend` | Set `CARGO_BUILD_RUSTC_WRAPPER=""` in the workflow env (the repo already does exactly this for native Windows CI — see the note at `.cargo/config.toml:15-19`), or `JCODE_PARALLEL_FRONTEND=0`. |
| 8 | **MAJOR** | `rusqlite` with `features = ["bundled"]` compiles the SQLite C amalgamation. | `crates/jcode-base/Cargo.toml:45`; also `crates/jcode-harness-api-server/Cargo.toml:16` | **Keep bundled** (see §3.1). It is plain C89 and armv7-clean. The real risk is toolchain discovery, not the C itself: export `CC_armv7_linux_androideabi`, `AR_armv7_linux_androideabi`, `CFLAGS_armv7_linux_androideabi`, and `CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER` pointing at the NDK clang wrappers. |
| 9 | **MAJOR** | `zstd` (C) default-features off, but `zstd-sys` still builds the C library. On the **default** graph only — reached via `jcode-provider-core`, which is non-optional in the core. | `crates/jcode-provider-core/Cargo.toml:17` | armv7-clean; same NDK `CC`/`AR` env as #8. If it still fights the toolchain, gate it behind a feature and fall back to identity request encoding. |
| 10 | **MAJOR** | `tokenizers` with `features = ["onig"]` pulls Oniguruma too. | `crates/jcode-embedding/Cargo.toml:13` | Already **unreachable** with `--no-default-features` (`embeddings` feature is off by default and `jcode-embedding` is optional at `crates/jcode-base/Cargo.toml:53`). Belt-and-braces: switch to the `fancy` regex backend if embeddings are ever enabled. |
| 11 | **MAJOR** | `/proc/version` is read unconditionally and is **unavailable/unreadable** under Android app sandboxing. | `crates/jcode-base/src/auth/commands.rs:59`; `crates/jcode-app-core/src/perf.rs:440` | Both already treat a read failure as a fallback path (`if let Ok(v)` / `let Ok(...)`), so this degrades rather than panics. Confirm the fallback yields a sane value; consider gating on `cfg(not(target_os = "android"))`. |
| 12 | **MAJOR** | `/proc/<pid>/syscall` and `/proc/<pid>/stat` reads are used for process/child detection; Android SELinux commonly denies reading another process's `/proc` entries. | `crates/jcode-core/src/stdin_detect.rs:32`; `crates/jcode-base/src/claude_live.rs:154`; `crates/jcode-base/src/browser.rs:1154,1206`; `crates/jcode-app-core/src/tool/bridge_reload.rs:151-152` | All are already `Result`-guarded. Verify the degraded path is functional on-device; do not treat as a build blocker. |
| 13 | **MINOR** | Stale `[profile.*.package]` pins for crates that are **not in the lockfile at all**: `cosmic-text`, `swash`, `yazi`. Harmless (cargo ignores unknown package profiles) but misleading. `fontdb` and `rustybuzz` *are* in the lock, pulled by `mermaid-rs-renderer`/`usvg` via `jcode-tui-mermaid`. | `repo/Cargo.toml:459-499` (cosmic-text/swash/yazi); pins at :487-499 | No build action. `fontdb` may probe fontconfig — if `jcode-tui-mermaid` fails, build it with `--no-default-features` to disable `mermaid-renderer` (`crates/jcode-tui-markdown/Cargo.toml:24`). |
| 14 | **MINOR** | Hardcoded `/usr/bin/...`, `/bin/sh`, `/usr/local/bin` paths — all invalid on Termux (which has no `/usr`). | macOS-only: `crates/jcode-base/src/auth/claude.rs:765,836`; `crates/jcode-app-core/src/notifications.rs:506,605,620,625,643`; `crates/jcode-app-core/src/tool/computer/*` | All are inside `cfg(target_os = "macos")` blocks and are compiled out on Android. **No action required** — verified, not merely assumed. |
| 15 | **MINOR** | `/tmp` literals appear across the tree. | e.g. `src/bin/session_memory_bench.rs:213`, plus many `*_tests.rs` | Production paths correctly use `std::env::temp_dir()` / `$TMPDIR` (`crates/jcode-storage/src/lib.rs:119`), which resolves to the Termux app temp dir rather than `/tmp`. The literal hits are almost entirely test fixtures and the `dev-bins` benchmarks. No action. |
| 16 | **MINOR** | `bash` is invoked by name for the shell tool. | `crates/jcode-app-core/src/tool/bash.rs:678,708` | Termux ships bash; `PATH` includes `$PREFIX/bin`. Fine. |
| 17 | **MINOR** | `agentgrep` git dependency (`tag = v0.2.0`, rev `a2c52bbf`) adds a network dependency; `cargo tree --offline` fails on it. | `crates/jcode-app-core/Cargo.toml:107` | **Verified clean for 32-bit:** its only native-relevant dep is `libc`; no `cc`, `bindgen`, or `cmake`. It is a public repo so CI can fetch it. Optionally vendor it to make CI hermetic. |

---

## 2. Confirmed-safe (do not spend effort here)

- **No 64-bit pointer-width assumptions.** `grep -rn 'target_pointer_width' src crates` → **0 hits**. `AtomicU64` is used in ~40 production files but exclusively as monotonic counters/epochs/IDs (`crates/jcode-app-core/src/server.rs:589`, `crates/jcode-agent-runtime/src/lib.rs:41`, `crates/jcode-logging/src/watchdog.rs`, …). On `armv7-linux-androideabi` `target_has_atomic = "64"` holds (LDREXD/STREXD), and even where it did not, `AtomicU64` degrades to a lock-based fallback rather than failing to build. **Not a risk.**
- **No `unsafe impl Send`/`unsafe impl Sync` anywhere** in `src/` or `crates/` → 0 hits. No hand-rolled concurrency assumptions to re-verify.
- **glibc-specific code is correctly gated.** `mallopt` usage in `src/main.rs:30-78` and `crates/jcode-base/src/process_memory.rs:289+` is behind `target_env = "gnu"`, which is **false** for `target_env = "android"`. These correctly compile to no-ops. `crates/jcode-harness-api-server/src/allocator.rs:16` is likewise `gnu`-gated.
- **`cfg(target_os = "linux")` does not silently capture Android.** 272 occurrences, but the ones that matter pair it with `target_env = "gnu"` or are pure-procfs/optional paths. Notably `crates/jcode-base/src/process_title.rs:109` (`cfg(target_os = "linux")` → `prctl(PR_SET_NAME)`) is **skipped** on Android, which is correct and harmless.
- **No `cfg(target_arch)` branching that excludes arm.** The only two uses are `crates/jcode-base/src/auth/antigravity.rs:71` and `crates/jcode-provider-antigravity/src/lib.rs:210`, both `cfg!(target_arch = "aarch64")` used as an *optional fast path* with an `else` — armv7 takes the fallback branch correctly.
- **jemalloc is off.** `tikv-jemallocator`/`-ctl`/`-sys` are all `optional = true` and appear in **no** default feature set (`repo/Cargo.toml:226-235`). Never enable `--features jemalloc` on 32-bit Android.
- **No openssl-sys on the Android path.** `native-tls`/`openssl` are only reachable through the opt-in `linux-compat-vendored-openssl` feature (`crates/jcode-notify-email/Cargo.toml:19,27`), which is off by default.

---

## 3. Targeted confirmations requested by the lead

### 3.1 `libsqlite3-sys` via `rusqlite features=["bundled"]` — **CONFIRMED present; MAJOR, keep bundled**
`crates/jcode-base/Cargo.toml:45` and `crates/jcode-harness-api-server/Cargo.toml:16` both declare `features = ["bundled"]`. Because both crates are in the root binary's graph, feature unification makes `bundled` **sticky** — un-bundling would require editing two manifests *and* making Termux's `libsqlite3.so` a runtime prerequisite. The amalgamation is plain C89 and builds fine under NDK clang for armv7. **Verdict: keep bundled; fix the toolchain env, not the manifest.**

### 3.2 `zstd-sys` via `zstd` — **CONFIRMED reachable, MAJOR**
`crates/jcode-provider-core/Cargo.toml:17` (`default-features = false`, which only disables the *bindings* feature, not the C build). `jcode-provider-core` is a non-optional dependency of the core, so this is on the default path. armv7-clean C; needs the same NDK `CC`/`AR` env.

### 3.3 `onig_sys` via syntect / tokenizers — **CONFIRMED; BLOCKER via syntect, unreachable via tokenizers**
`crates/jcode-tui-markdown/Cargo.toml:18` uses `regex-onig` and is compiled unconditionally → **BLOCKER** (#6). `crates/jcode-embedding/Cargo.toml:13` uses `onig` but sits behind the off-by-default `embeddings` feature → not on the minimal path.

### 3.4 `rquickjs-sys` via `jcode-codemode` — **CONFIRMED BLOCKER; the worst item**
Verified directly from the crate tarball, not from documentation: `rquickjs-sys 0.11.0` ships 14 bundled binding files (`aarch64-apple-darwin`, `aarch64-pc-windows-msvc`, `aarch64-unknown-linux-{gnu,musl}`, `i686-unknown-linux-gnu`, `loongarch64-*`, `wasm32-wasip{1,2}`, `x86_64-*`) — **no android target of any word size**. Its `build.rs` checks for `src/bindings/<target>.rs`, emits only a *warning* on absence, and compiles QuickJS via `cc::Build` regardless — so this fails at C-compile/link time rather than with a clear message. Reached unconditionally through `jcode-app-core` → `jcode-codemode`. **This is the item most likely to sink the first CI run after the TLS fix.**

### 3.5 `proctitle 0.1.1` — **NOT an issue; builder's flag is INCORRECT**
I extracted the crate tarball and read it. `proctitle 0.1.1` has **no `build.rs`, no `cc` dependency, and no C source**:
- Linux impl (`src/lib.rs:31-43`) is pure Rust calling `libc::prctl(PR_SET_NAME, ...)`.
- Android matches **neither** `target_os = "linux"` **nor** the BSD list, so it selects the no-op stub at `src/lib.rs:130-144`, whose body is literally `pub fn set_title<T: AsRef<OsStr>>(_title: T) {}`.
- Its only target-specific dependency is `libc` under `cfg(target_os = "linux")`.

**Verdict: compiles and runs on armv7-android with zero changes.** The *functional* consequence is that `crates/jcode-base/src/process_title.rs:104` becomes a no-op, so process titles are not set on Termux — a cosmetic `ps` regression, not a build blocker. No patch needed.

### 3.6 `cfg(target_os = "linux")` excluding or wrongly-including Android — **NO ACTION NEEDED**
See §2. The important gnu-gated sites are correct; the `target_os = "linux"`-only site (`process_title.rs:109`) is harmlessly skipped.

### 3.7 `agentgrep` git dependency — **CONFIRMED benign**
Locked at `git+https://github.com/1jehuang/agentgrep.git?tag=v0.2.0#a2c52bbf`. Its dependency list is `clap, globset, ignore, libc, regex, serde, serde_json, windows-sys` — **no `cc`, `bindgen`, or `cmake`** anywhere. Public repo, so CI can fetch it; consider vendoring for hermetic builds.

---

## 4. Recommended minimal feature set for a Termux/CI build

```bash
export CARGO_BUILD_RUSTC_WRAPPER=""        # neutralise the nightly -Zthreads wrapper (#7)
cargo build --release \
  --target armv7-linux-androideabi \
  --no-default-features                    # drops pdf + bedrock (#4)
```

Do **not** add any feature. Explicitly **never** enable `jemalloc`, `embeddings`, `pdf`, `bedrock`, or `linux-compat-vendored-openssl`.

**Required NDK environment** (the fix for MAJOR items #8/#9):
```bash
export CC_armv7_linux_androideabi=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/armv7a-linux-androideabi21-clang
export AR_armv7_linux_androideabi=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar
export CFLAGS_armv7_linux_androideabi="--sysroot=$NDK/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER=$CC_armv7_linux_androideabi
```

**Prerequisite source changes before CI can succeed at all** (items #5 and #6 — neither is removable by a feature flag):
1. `crates/jcode-codemode/Cargo.toml:24` — extend to `cfg(not(any(target_os = "freebsd", target_os = "android")))`.
2. `crates/jcode-tui-markdown/Cargo.toml:18` — `regex-onig` → `regex-fancy`.

**First CI gate, before any compile** — this decides whether item #3 is real:
```bash
cargo tree -e features -i aws-lc-sys --target armv7-linux-androideabi
```
If this prints a non-empty tree, aws-lc-sys is still being built and the quinn/reqwest path must be broken before anything else will work.

### Crate classification for the minimal CLI

| Needed always | Safe to leave off |
|---|---|
| `jcode-base`, `jcode-app-core`, `jcode-tui`, `jcode-core`, `jcode-storage`, `jcode-provider-core`, the provider `*-runtime` crates, `jcode-transport`, `jcode-logging` | `jcode-embedding` (tract-onnx + tokenizers; `embeddings`) |
| `rustls` + `ring` (after fix) | `jcode-provider-bedrock` + all `aws-*` (`bedrock`) |
| `rusqlite`/`libsqlite3-sys` bundled (session storage) | `jcode-pdf` / `pdf-extract` / `lopdf` (`pdf`) |
| | `jcode-voice` capture (target-gated; `cpal`/`oboe` only via `voice-capture`) |
| | `tikv-jemalloc*` (`jemalloc`) |
| | `native-tls`/`openssl-sys` (`linux-compat-vendored-openssl`) |
| | `rquickjs` (must be excluded — #5) |

---

## 5. Residual uncertainty (stated honestly)

- **Item #3 (quinn/aws-lc-sys):** I traced this statically through `Cargo.lock`; I could not run `cargo tree` (no crate cache on this device and `cargo tree --offline` fails on the `agentgrep` git dep). **This must be verified in CI as the first step.** It is the one claim here I cannot close out from the workspace alone.
- **Items #8/#9 (bundled C):** verified as armv7-clean *C*; not compiled end-to-end under NDK in this audit.
- The audit was performed without network access to the crates.io *index* for dependency resolution; crate *tarballs* were fetched directly from `static.crates.io` where verification mattered (proctitle, rquickjs-sys, syntect), so those three findings rest on primary sources.
