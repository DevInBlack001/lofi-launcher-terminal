#!/usr/bin/env sh
set -eu

# Same path resolution as install.sh/update.sh: environment-relative, never
# hardcoded, so this removes exactly what install.sh put in place.
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

# Stop any running daemon for this user before removing its binary.
# -x matches the exact process name only, not any command line containing
# the substring "lofi-daemon", so this can't accidentally kill an unrelated
# process (e.g. someone's editor with that string open in a buffer).
pkill -u "$(id -u)" -x lofi-daemon 2>/dev/null || true

# Safety net for an already-running OLD daemon build without the SIGTERM
# handler: a bare SIGTERM skips Drop, so mpv is never asked to quit and is
# orphaned. Target it by its unique IPC socket path so this can't touch an
# unrelated mpv instance.
pkill -u "$(id -u)" -f -- "--input-ipc-server=.*lofi-mpv\.sock" 2>/dev/null || true

rm -f "$BIN_DIR/lofi" "$BIN_DIR/lofi-daemon"
echo "Removed binaries from $BIN_DIR"

# Check both rc files regardless of the current $SHELL: the user may have
# switched shells since installing, and the marker guard makes this safe
# even if only one of them actually has the block.
for RC_FILE in "$HOME/.bashrc" "$HOME/.zshrc"; do
    if [ -f "$RC_FILE" ] && grep -qF "$MARKER_START" "$RC_FILE"; then
        sed -i "/$MARKER_START/,/$MARKER_END/d" "$RC_FILE"
        echo "Removed shell integration from $RC_FILE"
    fi
done

# Config is kept by default since it holds the user's own moods/sources;
# only --purge removes it, an explicit, opt-in destructive action.
if [ "$PURGE" = "1" ]; then
    rm -rf "$CONFIG_DIR"
    echo "Removed config directory $CONFIG_DIR"
else
    echo "Config directory $CONFIG_DIR left in place; re-run with --purge to remove it."
fi
