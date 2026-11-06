# Codemode (QuickJS) on Android

Codemode is the opt-in `codemode` tool: the model writes a short JavaScript
program that orchestrates harness tool calls inside an embedded QuickJS sandbox
(`crates/jcode-codemode`). This note records why it used to be compiled out on
Android, what actually fixed it, and what the build host must provide.

## Why it was gated off (the wrong diagnosis)

The original Termux port gated the `rquickjs` dependency behind
`cfg(not(any(target_os = "freebsd", target_os = "android")))` with the comment
that rquickjs "would need libclang plus a working QuickJS C build for that ABI".

The first half is right, the second half is not. Reading
`rquickjs-sys-0.14.0/build.rs`:

```rust
#[cfg(not(feature = "bindgen"))]
fn bindgen(...) {
    // if src/bindings/<TARGET>.rs does not exist:
    println!("cargo:warning=rquickjs probably doesn't ship bindings for platform ...");
    // then writes a bindings.rs containing ONLY:
    //   macro_rules! bindings_env { ("TARGET") => { "..." }; }
}
```

and `src/lib.rs` does `include!(concat!(env!("OUT_DIR"), "/bindings.rs"))`.
So on a target with no pregenerated binding the crate does not merely lack
bindings — it fails to compile, because that stub defines no `js_*` items.

`rquickjs-sys` ships pregenerated bindings for glibc/musl/desktop triples only
(`src/bindings/` has `armv7-unknown-linux-gnueabi`, `aarch64-unknown-linux-gnu`,
…). There is no `armv7-linux-androideabi.rs` and no `aarch64-linux-android.rs`.

## The fix

`rquickjs-sys` already vendors the complete QuickJS C sources (`quickjs/`,
~5 MB) and its `bindgen` feature switches `build.rs` to the real path:

1. run `bindgen 0.72` against `quickjs.bind.h` **for the target triple**,
2. compile `libquickjs.a` with `cc-rs` (which honours the NDK `CC_<target>`),
3. `include!` the generated bindings.

That path is target-agnostic, so codemode is enabled on Android by:

```toml
[target.'cfg(not(target_os = "freebsd"))'.dependencies]
rquickjs = { version = "0.14", features = ["futures", "bindgen"] }
```

Feature propagation is `rquickjs/bindgen` → `rquickjs-core/bindgen` →
`rquickjs-sys/bindgen` → `rquickjs-sys/bindgen-rs`. FreeBSD keeps the gate: it
has no pregenerated binding either, and the FreeBSD smoke VM does not install
libclang.

## Why enabling `bindgen` everywhere is safe

`build.rs` short-circuits before invoking bindgen when `target == host` and a
bundled `src/bindings/<triple>.rs` exists:

```rust
if target == host && env::var("CARGO_FEATURE_UPDATE_BINDINGS").is_err() {
    if bundled.exists() { fs::copy(&bundled, out_dir.join("bindings.rs")); return; }
}
```

So x86_64/aarch64 glibc, macOS and Windows desktop builds keep using the
pregenerated bindings and never need libclang. Only cross targets without a
bundled binding (the Android ABIs) actually run bindgen.

## Build-host requirements

* **libclang** — bindgen links against it. CI installs
  `libclang-dev clang llvm` (already present in the Termux armv7 workflow).
  bindgen's clang-sys loads `libclang.so` via `LIBCLANG_PATH` if set, otherwise
  via the usual loader search path. No `LIBCLANG_PATH` is required on the
  ubuntu-latest runner or on Termux, where `$PREFIX/lib/libclang.so` is found.
  If a host has multiple clang versions, point `LIBCLANG_PATH` at the matching
  one: libclang must be the same major family as the clang driver used to parse
  the headers, otherwise bindgen fails with a libclang/clang version mismatch.
* **NDK sysroot for the target** — bindgen must parse bionic headers, not the
  host's. The Termux workflow already exports
  `BINDGEN_EXTRA_CLANG_ARGS_armv7_linux_androideabi=--sysroot=... --target=armv7a-linux-androideabi24`,
  which bindgen reads and appends to its clang args. **Any new Android ABI lane
  must export the equivalent `BINDGEN_EXTRA_CLANG_ARGS_<triple>`**, otherwise
  bindgen parses the host glibc headers and emits bindings with the wrong
  `size_t`/struct layout for the target.
* **A C compiler for the target** — `cc-rs` uses `CC_<target>`; the workflow
  already sets `CC_armv7_linux_androideabi`.

## Version pin

None needed. `Cargo.lock` already resolves `bindgen v0.72.1`, which is exactly
the `^0.72` that `rquickjs-sys 0.14` declares for its optional `bindgen-rs`
dependency. Enabling the feature requires no new crate version beyond what the
lock already contained.

Enabling the feature does cause cargo to add `rquickjs-macro v0.14.0` and
`convert_case v0.12.0` to `Cargo.lock`. This is a **lockfile-resolution
artifact only**: `rquickjs-macro` is an optional dependency behind the `macro`
feature, which this workspace does not enable. `cargo tree -e normal --target
armv7-linux-androideabi -p jcode --no-default-features -i rquickjs-macro`
reports "nothing to print", i.e. it is not in the build graph and is never
compiled.

## Architecture assumptions

QuickJS is portable C and supports 32-bit and 64-bit ARM. `rquickjs-sys`
selects its pointer helpers by `target_pointer_width` (`inlines/ptr_32_nan_boxing.rs`
vs `inlines/ptr_64.rs`) and calls `bindgen .size_t_is_usize(false)`, so the
generated bindings use the target's real `size_t` on both `armv7-linux-androideabi`
(32-bit) and `aarch64-linux-android` (64-bit). `jcode-codemode/src/engine.rs`
contains no architecture-specific code.

Note that Android armv7 is **softfp**: never add `-mfloat-abi=hard` to
`CFLAGS_<target>`. It links but crashes at runtime.

## Verifying

```sh
# Feature actually reaches the sys crate for a target with no bundled binding:
cargo tree -e features --target armv7-linux-androideabi -p jcode \
  --no-default-features -i rquickjs-sys
# expect a "rquickjs-sys feature \"bindgen-rs\"" edge

# The macro dep must NOT be built:
cargo tree -e normal --target armv7-linux-androideabi -p jcode \
  --no-default-features -i rquickjs-macro   # -> "nothing to print"
```

`rquickjs::AVAILABLE` is now `true` on Android, so `codemode` shows up whenever
`[tools] codemode = true` (the default) and `JCODE_CODEMODE` is not disabled.
