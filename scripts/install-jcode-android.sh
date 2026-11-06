#!/data/data/com.termux/files/usr/bin/bash
#
# install-jcode-android.sh - install jcode on Android (Termux).
#
# Part of the Android/Termux fork maintained by @SehonYT:
#   https://github.com/SehonYT/jcode-termux
# Upstream jcode: https://github.com/1jehuang/jcode
#
# Detects the device ABI, downloads the matching prebuilt binary, verifies the
# ELF class/machine, installs it, and smoke-tests it.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/SehonYT/jcode-termux/master/scripts/install-jcode-android.sh | bash
#
# or from a checkout:
#   bash scripts/install-jcode-android.sh [--from-file /path/to/jcode] [--abi <target>]
#
set -euo pipefail

REPO="SehonYT/jcode-termux"
BIN_NAME="jcode"
INSTALL_DIR="${JCODE_INSTALL_DIR:-$HOME/.local/bin}"
TMPDIR_JC="$(mktemp -d "${TMPDIR:-/tmp}/jcode-install.XXXXXX")"
FROM_FILE=""
FORCE_ABI=""

info() { printf '\033[1;34m%s\033[0m\n' "$*"; }
warn() { printf '\033[1;33mwarning: %s\033[0m\n' "$*" >&2; }
die()  { printf '\033[1;31merror: %s\033[0m\n' "$*" >&2; exit 1; }

cleanup() { rm -rf "$TMPDIR_JC"; }
trap cleanup EXIT

while [ $# -gt 0 ]; do
  case "$1" in
    --from-file) FROM_FILE="${2:-}"; shift 2 ;;
    --from-file=*) FROM_FILE="${1#*=}"; shift ;;
    --abi) FORCE_ABI="${2:-}"; shift 2 ;;
    --abi=*) FORCE_ABI="${1#*=}"; shift ;;
    -h|--help) sed -n "2,18p" "$0" 2>/dev/null | sed "s/^# \{0,1\}//"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

if [ -z "${PREFIX:-}" ] || [ "${PREFIX}" != "/data/data/com.termux/files/usr" ]; then
  if [ -d /data/data/com.termux/files/usr ]; then
    warn "Termux prefix exists but PREFIX is not set; continuing."
  else
    die "this installer is for Termux on Android. Use the upstream installer elsewhere."
  fi
fi

# Pick the ABI for THIS device. abilist is ordered best-first, so the first
# entry we can build for is the right one. This is what makes the installer work
# on every Android device rather than only one.
ABILIST="$(getprop ro.product.cpu.abilist 2>/dev/null || echo "")"
ABILIST64="$(getprop ro.product.cpu.abilist64 2>/dev/null || echo "")"
ARCH="$(uname -m)"

info "device: uname -m=$ARCH abilist=${ABILIST:-unknown} abilist64=${ABILIST64:-<empty>}"

if [ -n "$FORCE_ABI" ]; then
  TARGET="$FORCE_ABI"
  info "using forced ABI: $TARGET"
else
  TARGET=""
  IFS=","
  for abi in $ABILIST; do
    case "$abi" in
      arm64-v8a)   TARGET="aarch64-linux-android"; break ;;
      armeabi-v7a) TARGET="armv7-linux-androideabi"; break ;;
      armeabi)     TARGET="armv7-linux-androideabi"; break ;;
      x86_64)      TARGET="x86_64-linux-android"; break ;;
      x86)         TARGET="i686-linux-android"; break ;;
    esac
  done
  unset IFS

  if [ -z "$TARGET" ]; then
    case "$ARCH" in
      aarch64|arm64) TARGET="aarch64-linux-android" ;;
      armv8l|armv7l|armv7*|arm*) TARGET="armv7-linux-androideabi" ;;
      x86_64) TARGET="x86_64-linux-android" ;;
      i686|i386) TARGET="i686-linux-android" ;;
      *) die "unsupported architecture: $ARCH (abilist=${ABILIST:-})" ;;
    esac
  fi
fi

info "target ABI: $TARGET"

if [ -z "$ABILIST64" ]; then
  info "note: this device is 32-bit only (no 64-bit ABI), so a 32-bit build is correct."
fi

if [ -n "$FROM_FILE" ]; then
  [ -f "$FROM_FILE" ] || die "--from-file: no such file: $FROM_FILE"
  info "using local binary: $FROM_FILE"
  cp "$FROM_FILE" "$TMPDIR_JC/$BIN_NAME"
else
  command -v curl >/dev/null 2>&1 || die "curl is required; run: pkg install curl"

  ASSET="jcode-${TARGET}.tar.gz"
  info "resolving the latest release of $REPO"
  TAG="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null \
          | sed -n "s/.*\"tag_name\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" | head -1 || true)"
  if [ -n "$TAG" ]; then
    URL="https://github.com/$REPO/releases/download/$TAG/$ASSET"
  else
    warn "could not resolve the latest release tag; trying the latest redirect"
    URL="https://github.com/$REPO/releases/latest/download/$ASSET"
  fi

  info "downloading $URL"
  curl -fL --retry 3 --connect-timeout 15 -o "$TMPDIR_JC/jcode.tar.gz" "$URL" \
    || die "download failed ($ASSET). See https://github.com/$REPO/releases for available ABIs."

  tar xzf "$TMPDIR_JC/jcode.tar.gz" -C "$TMPDIR_JC" || die "failed to extract the archive"

  if [ -f "$TMPDIR_JC/jcode-${TARGET}" ]; then
    mv "$TMPDIR_JC/jcode-${TARGET}" "$TMPDIR_JC/$BIN_NAME"
  elif [ -f "$TMPDIR_JC/$BIN_NAME" ]; then
    :
  else
    FOUND="$(find "$TMPDIR_JC" -maxdepth 2 -type f -name "jcode*" ! -name "*.tar.gz" | head -1)"
    [ -n "$FOUND" ] || die "archive did not contain a jcode binary"
    mv "$FOUND" "$TMPDIR_JC/$BIN_NAME"
  fi
fi

chmod +x "$TMPDIR_JC/$BIN_NAME"

# Verify the ELF matches the ABI we asked for. An aarch64 binary on a 32-bit
# device fails at exec time with a confusing "No such file or directory", so
# catching a wrong-arch download here is worth the few lines.
if command -v readelf >/dev/null 2>&1; then
  HDR="$(readelf -h "$TMPDIR_JC/$BIN_NAME" 2>/dev/null || true)"
  case "$HDR" in
    *ELF32*ARM*)       GOT="armv7-linux-androideabi" ;;
    *ELF64*AArch64*)   GOT="aarch64-linux-android" ;;
    *ELF32*80386*)     GOT="i686-linux-android" ;;
    *ELF64*X86-64*)    GOT="x86_64-linux-android" ;;
    *)                 GOT="unknown" ;;
  esac
  if [ "$GOT" = "unknown" ]; then
    warn "could not determine the ELF class/machine; continuing without verification"
  elif [ "$GOT" != "$TARGET" ]; then
    die "downloaded a $GOT binary but this device needs $TARGET"
  else
    info "verified: $GOT binary"
  fi
else
  warn "readelf not found; skipping binary verification (pkg install binutils)"
fi

mkdir -p "$INSTALL_DIR"
STABLE_DIR="$HOME/.jcode/builds/android-$TARGET"
mkdir -p "$STABLE_DIR"

cp "$TMPDIR_JC/$BIN_NAME" "$STABLE_DIR/$BIN_NAME"
chmod +x "$STABLE_DIR/$BIN_NAME"

cat > "$INSTALL_DIR/$BIN_NAME" <<EOF
#!/data/data/com.termux/files/usr/bin/bash
unset LD_PRELOAD
exec "$STABLE_DIR/$BIN_NAME" "\$@"
EOF
chmod +x "$INSTALL_DIR/$BIN_NAME"

info "installed: $INSTALL_DIR/$BIN_NAME"
info "           $STABLE_DIR/$BIN_NAME"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    warn "$INSTALL_DIR is not on your PATH. Add it with:"
    warn "  echo \"export PATH=\\\"\\$HOME/.local/bin:\\$PATH\\\"\" >> ~/.bashrc && source ~/.bashrc"
    ;;
esac

info "running smoke test"
if OUT="$("$INSTALL_DIR/$BIN_NAME" --version 2>&1 | head -1)"; then
  info "jcode --version -> $OUT"
else
  warn "jcode --version exited non-zero. Output was:"
  printf "%s\n" "$OUT" >&2
  warn "If this mentions a missing file or interpreter, the binary may not match this device."
fi

info "done. Run jcode login then jcode to start."