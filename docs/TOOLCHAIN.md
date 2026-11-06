# TOOLCHAIN.md — Rust for 32-bit ARM Android / Termux

Verified on this device (2026-11-06) by teammate `toolchain-scout` for shared **task-1**.

## TL;DR

| Question | Answer |
|---|---|
| **Target triple** | `armv7-linux-androideabi` |
| **std prebuilt?** | **YES** — no `-Zbuild-std` needed |
| **Local toolchain** | `rustc 1.99.1` / `cargo 1.99.1` (Termux `rust` package) |
| **CI toolchain** | stable rustup + `rustup target add armv7-linux-androideabi` |
| **Hello-world runs on device?** | **YES** — verified below |
| **Submodules / LFS?** | **No** — neither is used by this repo |

---

## 1. Device constraints (re-verified)

```
uname -m                        = armv8l        (64-bit CAPABLE kernel)
getprop ro.product.cpu.abilist  = armeabi-v7a,armeabi
getprop ...abilist64            = (EMPTY)
dpkg --print-architecture       = arm
```

**This is a 32-bit-only userspace.** There is no 64-bit linker and no 64-bit
`app_process`; aarch64 binaries **cannot** execute here. Any binary shipped to
this device must be **32-bit ARM (armv7)**, not arm64.

---

## 2. Target triple decision

Verified with `rustc --print target-list` (armv7 is present) and against the
official `channel-rust-stable.toml` manifest:

| Triple | std `available` upstream | Termux `rust-std-*` package | Verdict |
|---|---|---|---|
| `armv7-linux-androideabi` | ✅ `true` | ✅ `rust-std-armv7-linux-androideabi` | ✅ **USE THIS** |
| `thumbv7neon-linux-androideabi` | ✅ `true` | ❌ not packaged | works in CI only |
| `arm-linux-androideabi` | ✅ `true` (std only, legacy ARMv5) | ❌ | ❌ avoid — wrong baseline |
| `aarch64-linux-android` | ✅ `true` | ✅ packaged | ❌ **cannot run on this device** |

- `arm-linux-androideabi` is the **armv5** baseline — it would run, but with no
  ARMv7 instructions. Do not use it for a modern device.
- `thumbv7neon-linux-androideabi` is **Thumb2 + NEON**; also valid for this
  hardware, but it is not packaged by Termux, so a *local* build fails with
  `can't find crate for std` (reproduced below). In **CI it works fine** since
  rustup has it.
- Official tier classification (doc.rust-lang.org platform-support): both
  `armv7-linux-androideabi` ("Armv7-A Android") and
  `thumbv7neon-linux-androideabi` ("Thumb2-mode Armv7-A Android with NEON")
  are listed with a ✅, i.e. **Tier 2 with prebuilt std**.

### Proof that std is prebuilt (no `-Zbuild-std`)

```
[pkg.rust-std.target.armv7-linux-androideabi]
available = true
url = "https://static.rust-lang.org/dist/2026-10-15/rust-std-1.99.1-armv7-linux-androideabi.tar.xz"
```

Since `available = true`, rustup ships a **precompiled std**. **`-Zbuild-std` is
NOT required** and must not be used (it needs nightly + `rust-src`).

---

## 3. Local install (Termux) — DONE and verified

```bash
apt-get update
apt-get install -y rust rust-std-armv7-linux-androideabi clang binutils
```

Result:

```
rustc 1.99.1 (58cf8f9c3 2026-10-13) (built from a source tarball)
cargo 1.99.1 (5f94df478 2026-08-27) (built from a source tarball)
host: armv7-linux-androideabi      <-- rustc's own host IS the target
LLVM version: 21.1.8
```

> **Note on mirrors.** The default `grimler.se` mirror measured only
> **2–9 KB/s** here. `https://mirror.mwt.me/termux/main` measured **~217 KB/s
> (1390 KB/s on the real fetch)** — ~23× faster. Switch with:
> `echo "deb https://mirror.mwt.me/termux/main stable main" > $PREFIX/etc/apt/sources.list`

The `rust` package already **Depends: rust-std-armv7-linux-androideabi**, so std
for the target arrives automatically.

### Verified smoke test (actually ran on this device)

```bash
rustc --target armv7-linux-androideabi hello.rs -o hello_armv7
./hello_armv7
```

Output:
```
hello from rust on 32-bit android!
ptr_width=32 arch=arm os=android
```

`file` on the produced binary:
```
ELF 32-bit LSB pie executable, ARM, EABI5 version 1 (SYSV),
dynamically linked, interpreter /system/bin/linker, for Android 24,
built by NDK r30, not stripped
```

Interpreter is **`/system/bin/linker`** (the 32-bit Android linker) and it
executes. This is the definitive proof the triple is correct.

---

## 4. CI configuration (GitHub Actions)

### 4.1 Toolchain

```yaml
- uses: dtolnay/rust-toolchain@stable
  with:
    targets: armv7-linux-androideabi
```

or equivalently `rustup target add armv7-linux-androideabi`.
**No `-Zbuild-std`, no nightly, no `rust-src`.**

### 4.2 Android NDK linker + env vars

```yaml
env:
  NDK: ${{ env.ANDROID_NDK_LATEST_HOME }}   # set on GitHub runners
  TARGET: armv7-linux-androideabi
  API: "24"
  NDK_BIN: ${{ env.ANDROID_NDK_LATEST_HOME }}/toolchains/llvm/prebuilt/linux-x86_64/bin

  CC_armv7_linux_androideabi:   ${{ env.NDK_BIN }}/armv7a-linux-androideabi24-clang
  CXX_armv7_linux_androideabi:  ${{ env.NDK_BIN }}/armv7a-linux-androideabi24-clang++
  AR_armv7_linux_androideabi:   ${{ env.NDK_BIN }}/llvm-ar
  RANLIB_armv7_linux_androideabi: ${{ env.NDK_BIN }}/llvm-ranlib

  CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER: ${{ env.NDK_BIN }}/armv7a-linux-androideabi24-clang
  CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_AR:     ${{ env.NDK_BIN }}/llvm-ar
```

```bash
cargo build --release --target armv7-linux-androideabi
```

Notes:
- The NDK wrapper name is `armv7a-linux-androideabi<API>-clang` — note **armv7a**
  in the file name vs **armv7** in the Rust triple. This mismatch is a classic
  misconfiguration; the env-var *key* uses the Rust triple with `-` → `_`.
- **`--sysroot` is NOT needed manually** — the `armv7a-...-clang` wrapper already
  injects the correct sysroot and API level.
- **`RANLIB` should be set** for crates that build C code (`cc` crate honours it);
  otherwise `ar`/ranlib mismatch can produce `.a` archives that fail to link.
- **`AR` must be `llvm-ar`** (NDK), not the host `ar`.

---

## 5. Gotchas specific to 32-bit ARM

1. **`-Zbuild-std` is a trap here.** People reach for it because they think std
   is missing. It is **not** missing — `available = true`. Using it forces
   nightly and hugely increases CI time for no benefit.
2. **thumb vs arm mode.** `armv7-linux-androideabi` emits **ARM mode**;
   `thumbv7neon-linux-androideabi` emits **Thumb2 + NEON**. Both run on this CPU.
   ARM mode generally has fewer edge-case codegen bugs; prefer
   `armv7-linux-androideabi` unless you specifically want NEON.
3. **Do not add `-march`/`-mfpu` by hand.** rustc's target spec already targets
   ARMv7-A with the correct float ABI. Adding `-mfloat-abi=hard` breaks the
   Android ABI (Android armv7 is **softfp**). This is the single most common way
   to produce a binary that links but crashes at runtime.
4. **`target_pointer_width = 32`.** Verified via `rustc --print cfg`. Any
   dependency assuming 64-bit `usize` (large mmap offsets, `u64`→`usize` casts)
   can overflow. Watch for `as usize` truncation.
5. **No glibc on this device.** The upstream installer
   (`scripts/install.sh`, `crates/jcode-app-core/src/update.rs`) only knows how
   to re-point the ELF interpreter at Termux's **glibc** loader and only for
   `aarch64`/`x86_64` — there is **no 32-bit ARM path** and `glibc` is not
   installed here. A plain Android-target binary links against
   `/system/bin/linker` (bionic) instead, which is why the smoke test above
   works with no interpreter patching. **CI should therefore build for the
   Android target, not for the Termux/glibc target.**
6. **aarch64 artifacts are useless here.** If CI produces `jcode-linux-aarch64`,
   it will not run on this device (empty `abilist64`).

---

## 6. rustup on Termux — investigated, **not usable as a host**

This is worth recording because it is widely misunderstood:

- rustup **does** publish 32-bit Android binaries:
  `https://static.rust-lang.org/rustup/dist/arm-linux-androideabi/rustup-init`
  returns HTTP 200 (11.4 MB). Inspected the magic bytes — it is a **genuine
  32-bit ARM ELF** (`7f 45 4c 46 01 01`, `e_machine = 0x28` EM_ARM).
- **However, Rust ships no compiler for any Android host.** In
  `channel-rust-stable.toml` there are **34 host platforms** with a full
  `pkg.rust.target.*` (compiler + std), and:

  ```
  [pkg.rustc.target.*android]  ->  NONE
  [pkg.cargo.target.*android]  ->  NONE
  [pkg.rust.target.*android]   ->  NONE
  ```

  Android appears **only** under `pkg.rust-std.target.*` (i.e. as a *target*
  stdlib, never as a *host* toolchain).

**Conclusion:** even though the `rustup-init` binary runs, `rustup install`
would fail — there is no `rustc`/`cargo` build to download for an Android host.
`rustup` is also **not packaged in the Termux repos** (`apt-cache policy rustup`
is empty). **Use `apt-get install rust` locally, and rustup only inside CI
(Linux x86_64 host).**

---

## 7. Git submodules / LFS

- **Submodules: not needed.** `git show HEAD:.gitmodules` → *does not exist*.
- **LFS: not needed.** `.gitattributes` contains only `* text=auto` — no
  `filter=lfs` entries, and `git lfs` is not even installed.
- The clone at `repo/` is a partial clone (`blob:none` filter). In CI use a
  normal `actions/checkout`, or `git clone --filter=blob:none` locally followed
  by `git sparse-checkout`/`git fetch` as needed. No
  `submodules: recursive` and no `lfs: true` flags are required.

---

## 8. Project-specific notes

- `Cargo.toml` declares **`edition = "2024"`**, which requires **rustc ≥ 1.85**.
  Both the local 1.99.1 and current CI stable satisfy this.
- `.cargo/config.toml` sets `rustc-wrapper = "scripts/rustc-parallel-frontend"`
  and `jobs = 4`. In CI that wrapper is a POSIX shell script and will run; if it
  causes trouble cross-compiling, disable it with
  `CARGO_BUILD_RUSTC_WRAPPER=""` (the same switch the project's own Windows CI
  uses).
- The workspace has ~50 crates and 910 registry dependencies; the local device
  has only **~1.35 GB available RAM**, so a full local release build is not
  practical. **CI is the right call.**

---

## Appendix — command reference

```bash
# Local (Termux, this device)
apt-get update
apt-get install -y rust rust-std-armv7-linux-androideabi clang binutils
rustc --version                       # 1.99.1
rustc --print target-list | grep armv7  # armv7-linux-androideabi
rustc --target armv7-linux-androideabi hello.rs -o hello && ./hello

# CI (Linux x86_64)
rustup target add armv7-linux-androideabi
cargo build --release --target armv7-linux-androideabi
```
