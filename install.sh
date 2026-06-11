#!/usr/bin/env bash
#
# install.sh — build and install the Render engine (render).
#
# Layout:
#   $PREFIX/lib/render/render   the release binary
#   $PREFIX/lib/render/library/     shaders, fonts, LUTs (resolved
#                                       executable-relative at runtime)
#   $PREFIX/bin/render              symlink to the binary
#
# Usage:
#   ./install.sh [--prefix DIR] [--uninstall]
#
# The prefix defaults to ~/.local (no sudo needed). Pass --prefix /usr/local
# for a system-wide install.

set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
UNINSTALL=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix)
            [[ $# -ge 2 ]] || { echo "error: --prefix requires a directory" >&2; exit 1; }
            PREFIX="$2"; shift 2 ;;
        --prefix=*)
            PREFIX="${1#--prefix=}"; shift ;;
        --uninstall)
            UNINSTALL=1; shift ;;
        -h|--help)
            sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)
            echo "error: unknown option '$1' (try --help)" >&2; exit 1 ;;
    esac
done

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$PREFIX/lib/render"
BIN_LINK="$PREFIX/bin/render"

if [[ $UNINSTALL -eq 1 ]]; then
    echo "Removing $APP_DIR and $BIN_LINK"
    rm -rf "$APP_DIR"
    rm -f "$BIN_LINK"
    echo "Uninstalled."
    exit 0
fi

# ── Prerequisites ─────────────────────────────────────────────────────────────

command -v cargo >/dev/null 2>&1 || {
    echo "error: cargo not found — install Rust ≥ 1.75 from https://rustup.rs" >&2
    exit 1
}

if ! command -v ffmpeg >/dev/null 2>&1; then
    echo "warning: ffmpeg not found on PATH — required only for .mp4 output" >&2
fi

# ── Build ─────────────────────────────────────────────────────────────────────

echo "Building release binary..."
cargo build --release --locked --manifest-path "$REPO_DIR/Cargo.toml"

# ── Install ───────────────────────────────────────────────────────────────────

echo "Installing to $APP_DIR"
mkdir -p "$APP_DIR" "$PREFIX/bin"
install -m 755 "$REPO_DIR/target/release/render" "$APP_DIR/render"

# Sync the library (shaders/fonts/LUTs); remove a stale copy first so deleted
# files don't linger.
rm -rf "$APP_DIR/library"
cp -R "$REPO_DIR/library" "$APP_DIR/library"

ln -sf "$APP_DIR/render" "$BIN_LINK"

# ── Verify ────────────────────────────────────────────────────────────────────

"$BIN_LINK" --help >/dev/null
echo "Installed: $("$BIN_LINK" --version 2>/dev/null || echo "render → $BIN_LINK")"

case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "note: $PREFIX/bin is not on your PATH — add it to your shell profile:" >&2
       echo "      export PATH=\"$PREFIX/bin:\$PATH\"" >&2 ;;
esac
