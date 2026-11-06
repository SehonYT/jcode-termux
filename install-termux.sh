#!/data/data/com.termux/files/usr/bin/bash
#
# install-termux.sh — install jcode on 32-bit ARM Android (Termux).
#
# jcode upstream ships no 32-bit ARM Android artifact, so this installs the
# armv7-linux-androideabi build produced by .github/workflows/termux-armv7.yml.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/SehonYT/jcode-termux/termux-armv7/install-termux.sh | bash
# or, from a checkout:
#   bash install-termux.sh [--from-file /path/to/jcode]
#
set -euo pipefail

REPO="SehonYT/jcode-termux"
BIN_NAME="jcode"
INSTALL_DIR="${JCODE_INSTALL_DIR:-$HOME/.local/bin}"
TMPDIR_JC="$(mktemp -d "${TMPDIR:-/tmp}/jcode-install.XXXXXX")"
FROM_FILE=""

cleanup() { rm -rf "$TMPDIR_JC"; }
trap cleanup EXIT

info() { printf '\033[1;34m%s\033[0m\n' "$*"; }
warn() { printf '\033[1;33mwarning: %s\033[0m\n' "$*" >&2; }
die()  { printf '\033[1;31merror: %s\033[0m\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    --from-file) FROM_FILE="${2:-}"; shift 2 ;;
    --from-file=*) FROM_FILE="${1#*=}"; shift ;;
    -h|--help)
      sed -n '2,14p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

# ---------------------------------------------------------------------------
# 1. Verify we are on Termux
# ---------------------------------------------------------------------------
if [ -z "${PREFIX:-}" ] || [ "${PREFIX}" != "/data/data/com.termux/files/usr" ]; then
  if [ -d /data/data/com.termux/files/usr ]; then
    warn "Termux prefix found but \$PREFIX is not set; continuing."
  else
    die "this installer is for Termux (Android). Use the upstream installer elsewhere."
  fi
fi

# ---------------------------------------------------------------------------
# 2. Verify the device is 32-bit ARM
# ---------------------------------------------------------------------------
ARCH="$(uname -m)"
ABILIST="$(getprop ro.product.cpu.abilist 2>/dev/null || echo '')"
ABILIST64="$(getprop ro.product.cpu.abilist64 2>/dev/null || echo '')"

info "detected: uname -m=$ARCH abilist=${ABILIST:-unknown} abilist64=${ABILIST64:-<empty>}"

case "$ARCH" in
  armv8l|armv7l|armv7*|arm*) ;;
  aarch64|arm64)
    die "this is a 64-bit ARM device; use the official installer" ;;
  x86_64|i686|i386)
    die "unsupported architecture: $ARCH" ;;
  *) die "unrecognised architecture: $ARCH" ;;
esac

if [ -n "$ABILIST64" ]; then
  warn "this device advertises 64-bit ABIs ($ABILIST64)."
  warn "The armv7 build will still run, but a native aarch64 build may be faster."
fi

# ---------------------------------------------------------------------------
# 3. Obtain the binary
# ---------------------------------------------------------------------------
if [ -n "$FROM_FILE" ]; then
  [ -f "$FROM_FILE" ] || die "--from-file: no such file: $FROM_FILE"
  info "using local binary: $FROM_FILE"
  cp "$FROM_FILE" "$TMPDIR_JC/$BIN_NAME"
else
  command -v curl >/dev/null 2>&1 || die "curl is required; run: pkg install curl"

  info "resolving latest armv7 release of $REPO"
  LATEST_URL="https://api.github.com/repos/$REPO/releases/latest"
  TAG="$(curl -fsSL "$LATEST_URL" 2>/dev/null | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1 || true)"

  ASSET="jcode-armv7-linux-androideabi.tar.gz"
  if [ -n "$TAG" ]; then
    URL="https://github.com/$REPO/releases/download/$TAG/$ASSET"
  else
    warn "could not resolve the latest release; trying the 'latest' redirect"
    URL="https://github.com/$REPO/releases/latest/download/$ASSET"
  fi

  info "downloading $URL"
  curl -fL --retry 3 --connect-timeout 15 -o "$TMPDIR_JC/jcode.tar.gz" "$URL" \
    || die "download failed. If no release exists yet, build one via the GitHub Actions workflow."

  tar xzf "$TMPDIR_JC/jcode.tar.gz" -C "$TMPDIR_JC" \
    || die "failed to extract the release archive"

  # The archive contains jcode-armv7-linux-androideabi (or a bare jcode).
  if [ -f "$TMPDIR_JC/jcode-armv7-linux-androideabi" ]; then
    mv "$TMPDIR_JC/jcode-armv7-linux-androideabi" "$TMPDIR_JC/$BIN_NAME"
  elif [ -f "$TMPDIR_JC/$BIN_NAME" ]; then
    :
  else
    FOUND="$(find "$TMPDIR_JC" -maxdepth 2 -type f -name 'jcode*' ! -name '*.tar.gz' | head -1)"
    [ -n "$FOUND" ] || die "archive did not contain a jcode binary"
    mv "$FOUND" "$TMPDIR_JC/$BIN_NAME"
  fi
fi

chmod +x "$TMPDIR_JC/$BIN_NAME"

# ---------------------------------------------------------------------------
# 4. Verify the ELF really is 32-bit ARM before installing it
# ---------------------------------------------------------------------------
if command -v readelf >/dev/null 2>&1; then
  ELF_HDR="$(readelf -h "$TMPDIR_JC/$BIN_NAME" 2>/dev/null || true)"
  case "$ELF_HDR" in
    *"ELF32"*"ARM"*) info "verified: ELF32 ARM binary" ;;
    *"ELF64"*)
      case "$ELF_HDR" in
        *aarch64*|*AArch64*) die "downloaded an aarch64 binary, which cannot run on this 32-bit device" ;;
        *) die "downloaded an ELF64 binary, which cannot run on this 32-bit device" ;;
      esac ;;
    *) warn "could not confirm the ELF class; continuing" ;;
  esac
else
  warn "readelf not found; skipping binary verification (pkg install binutils)"
fi

# ---------------------------------------------------------------------------
# 5. Install
# ---------------------------------------------------------------------------
mkdir -p "$INSTALL_DIR"
STABLE_DIR="$HOME/.jcode/builds/armv7"
mkdir -p "$STABLE_DIR"

# Keep an immutable copy plus a stable symlink, mirroring upstream's layout.
cp "$TMPDIR_JC/$BIN_NAME" "$STABLE_DIR/$BIN_NAME"
chmod +x "$STABLE_DIR/$BIN_NAME"

# Termux launcher: unset LD_PRELOAD so a Termux glibc shim cannot interfere with
# a bionic binary, and exec the stable path.
cat > "$INSTALL_DIR/$BIN_NAME" <<EOF
#!/data/data/com.termux/files/usr/bin/bash
unset LD_PRELOAD
exec "$STABLE_DIR/$BIN_NAME" "\$@"
EOF
chmod +x "$INSTALL_DIR/$BIN_NAME"

info "installed: $INSTALL_DIR/$BIN_NAME"
info "           $STABLE_DIR/$BIN_NAME"

# ---------------------------------------------------------------------------
# 6. PATH
# ---------------------------------------------------------------------------
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    warn "$INSTALL_DIR is not on your PATH. Add it with:"
    warn "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.bashrc && source ~/.bashrc"
    ;;
esac

# ---------------------------------------------------------------------------
# 7. Smoke test
# ---------------------------------------------------------------------------
info "running smoke test"
if OUT="$("$INSTALL_DIR/$BIN_NAME" --version 2>&1 | head -1)"; then
  info "jcode --version -> $OUT"
else
  warn "'jcode --version' exited non-zero. Output was:"
  printf '%s\n' "$OUT" >&2
  warn "If this mentions a missing file/interpreter, the binary may not match this device."
fi

info "done. Run 'jcode' to start."
