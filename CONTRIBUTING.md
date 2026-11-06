# Contributing to jcode for Android (Termux)

> **This repository is a fork.** [SehonYT/jcode-termux](https://github.com/SehonYT/jcode-termux)
> is maintained by **[@SehonYT](https://github.com/SehonYT)** (KR父TAJ).
>
> - For the **agent itself**, contribute upstream: [1jehuang/jcode](https://github.com/1jehuang/jcode).
> - For **Android/Termux issues** — the port, the four-ABI build, the clipboard crate, mobile
>   hardening, or the mobile test suite — open an issue here.
>
> Changes here are expected to stay Android-scoped so upstream merges remain easy. See
> [docs/android/README.md](docs/android/README.md#how-the-fork-is-kept-mergeable).

## Android-specific contribution notes

- **Builds are cross-compiled in CI** (`.github/workflows/termux-armv7.yml`) for all four Android
  ABIs. Local builds on a phone are possible but memory-hungry: set
  `CARGO_BUILD_RUSTC_WRAPPER=''` and keep `CARGO_BUILD_JOBS` low.
- **Never pass `-mfloat-abi=hard`.** Android armv7 is softfp; a hard-float binary links but
  crashes at runtime on device.
- **Do not widen a gate beyond what the evidence supports.** Several crates are excluded on
  Android for specific, documented reasons; "it compiles on my desktop" is not evidence.
- **Run the mobile suite** before proposing runtime changes: `scripts/test-mobile.sh`.
- Read [docs/android/BUILD.md](docs/android/BUILD.md) and
  [docs/android/HARDENING.md](docs/android/HARDENING.md) first if you are touching platform code.

---

# Contributing to jcode (upstream)

Thanks for contributing.
Thanks for contributing.