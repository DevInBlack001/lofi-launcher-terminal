#!/usr/bin/env sh
set -eu

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/lofi-launcher"
MARKER_START="# >>> lofi-launcher-terminal >>>"
MARKER_END="# <<< lofi-launcher-terminal <<<"
PURGE=0

for arg in "$@"; do
    if [ "$arg" = "--purge" ]; then
        PURGE=1
    fi
done

pkill -u "$(id -u)" -f 'lofi-daemon' 2>/dev/null || true

rm -f "$BIN_DIR/lofi" "$BIN_DIR/lofi-daemon"
echo "Removed binaries from $BIN_DIR"

for RC_FILE in "$HOME/.bashrc" "$HOME/.zshrc"; do
    if [ -f "$RC_FILE" ] && grep -qF "$MARKER_START" "$RC_FILE"; then
        sed -i "/$MARKER_START/,/$MARKER_END/d" "$RC_FILE"
        echo "Removed shell integration from $RC_FILE"
    fi
done

if [ "$PURGE" = "1" ]; then
    rm -rf "$CONFIG_DIR"
    echo "Removed config directory $CONFIG_DIR"
else
    echo "Config directory $CONFIG_DIR left in place; re-run with --purge to remove it."
fi
