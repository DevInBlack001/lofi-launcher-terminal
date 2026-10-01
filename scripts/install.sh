#!/usr/bin/env sh
set -eu

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/lofi-launcher"
MARKER_START="# >>> lofi-launcher-terminal >>>"
MARKER_END="# <<< lofi-launcher-terminal <<<"

echo "Building release binaries..."
(cd "$REPO_DIR" && cargo build --release)

mkdir -p "$BIN_DIR"
cp "$REPO_DIR/target/release/lofi" "$BIN_DIR/lofi"
cp "$REPO_DIR/target/release/lofi-daemon" "$BIN_DIR/lofi-daemon"
echo "Installed binaries to $BIN_DIR"

mkdir -p "$CONFIG_DIR"
if [ ! -f "$CONFIG_DIR/config.toml" ]; then
    cp "$REPO_DIR/config.default.toml" "$CONFIG_DIR/config.toml"
    echo "Created default config at $CONFIG_DIR/config.toml"
else
    echo "Existing config at $CONFIG_DIR/config.toml left untouched"
fi

case "${SHELL:-}" in
    */zsh) RC_FILE="$HOME/.zshrc" ;;
    */bash) RC_FILE="$HOME/.bashrc" ;;
    *)
        echo "Could not detect bash or zsh from \$SHELL ($SHELL); add scripts/lofi-launcher.sh.in to your shell rc file manually."
        RC_FILE=""
        ;;
esac

if [ -n "$RC_FILE" ]; then
    if [ -f "$RC_FILE" ] && grep -qF "$MARKER_START" "$RC_FILE"; then
        echo "Shell integration already present in $RC_FILE"
    else
        {
            echo "$MARKER_START"
            cat "$REPO_DIR/scripts/lofi-launcher.sh.in"
            echo "$MARKER_END"
        } >> "$RC_FILE"
        echo "Added shell integration to $RC_FILE"
    fi
fi

if ! command -v mpv >/dev/null 2>&1; then
    echo "Warning: mpv not detected on PATH, install it via your distro's package manager for playback to work."
fi

if [ ! -e /usr/share/mpv/scripts/mpris.so ] && [ ! -e /usr/lib/mpv/scripts/mpris.so ] && [ ! -e /usr/local/share/mpv/scripts/mpris.so ] && [ -z "${LOFI_MPV_MPRIS_SCRIPT:-}" ]; then
    echo "Note: mpv-mpris not detected, now-playing widgets (quickshell, playerctl, etc.) won't see this player. Playback still works without it."
fi

echo "Install complete. Open a new terminal to start playback automatically."
