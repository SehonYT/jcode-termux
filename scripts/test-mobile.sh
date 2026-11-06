#!/data/data/com.termux/files/usr/bin/bash
#
# test-mobile.sh - run the adversarial mobile/Android robustness suite.
#
# Design goals:
#   * the suite itself must NEVER hang, so every test has an internal timeout
#     and this script additionally wraps each target in `timeout`;
#   * it must run on a phone (armv7/bionic) as well as in CI;
#   * it reports failures loudly - a failing test that exposes a real bug is a
#     SUCCESS for this suite, so we never "fix" a test by weakening it.
#
# Usage:
#   scripts/test-mobile.sh              # full suite (default)
#   scripts/test-mobile.sh --quick      # skip the heavy/ignored tests
#   scripts/test-mobile.sh --soak       # include long-running soak tests
#   scripts/test-mobile.sh --parallel   # high test-thread count (race hunting)
#
set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

# The repo pins a nightly-only rustc wrapper in .cargo/config.toml; a stable
# test build must not use it.
export CARGO_BUILD_RUSTC_WRAPPER=""
export JCODE_NO_TELEMETRY=1
export RUST_BACKTRACE=1

# Keep memory use bounded on-device. Each test spawns the real CLI, and some
# tests allocate deliberately large buffers.
JOBS="${CARGO_BUILD_JOBS:-2}"
export CARGO_BUILD_JOBS="$JOBS"

MODE="full"
THREADS=""
for arg in "$@"; do
  case "$arg" in
    --quick) MODE="quick" ;;
    --soak) MODE="soak" ;;
    --parallel) THREADS="--test-threads=8" ;;
    --serial) THREADS="--test-threads=1" ;;
    -h|--help) sed -n "2,20p" "$0" | sed "s/^# \{0,1\}//"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

# Per-target suites. Each runs in its own process so one hang cannot take the
# whole suite down.
SUITES=(
  "mobile_resource_exhaustion"
  "mobile_hang_and_injection"
  "mobile_concurrency"
  "mobile_thirtytwo_bit"
)

pass=0; fail=0; failed_names=()

echo "==================================================================="
echo " jcode mobile adversarial suite"
echo " mode=$MODE  jobs=$JOBS  arch=$(uname -m)  threads=${THREADS:-default}"
echo " which=$(command -v cargo)"
echo "==================================================================="

for suite in "${SUITES[@]}"; do
  echo
  echo "--- $suite ---"
  args=("--" )
  if [ "$MODE" = "quick" ]; then args+=("--skip" "ignored_heavy"); fi
  if [ "$MODE" = "soak" ]; then args+=("--include-ignored"); fi
  if [ -n "$THREADS" ]; then args+=("$THREADS"); fi

  # Outer timeout is a backstop: the tests have their own, but if the harness
  # itself wedges we still make progress instead of blocking CI forever.
  if timeout 900 cargo test --test "$suite" "${args[@]}" 2>&1; then
    echo "PASS: $suite"
    pass=$((pass+1))
  else
    echo "FAIL: $suite"
    fail=$((fail+1))
    failed_names+=("$suite")
  fi
done

echo
echo "==================================================================="
echo " suites passed: $pass   failed: $fail"
if [ "$fail" -ne 0 ]; then
  echo " failing suites:"
  for n in "${failed_names[@]}"; do echo "   - $n"; done
  echo
  echo " NOTE: a failure here is a FINDING, not necessarily a bad test."
  echo " Read the output above and file the underlying bug."
  exit 1
fi
echo " all mobile suites passed"
echo "==================================================================="