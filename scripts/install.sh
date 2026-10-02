#!/usr/bin/env sh
set -eu

# Resolve paths relative to this script's own location and the environment,
# never hardcoded, so this works on any machine/user regardless of where
# the repo was cloned to.
REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/lofi-launcher"
# Markers bracket the shell-rc block we add below, so a second install run
# can detect it's already there instead of duplicating it.
MARKER_START="# >>> lofi-launcher-terminal >>>"
MARKER_END="# <<< lofi-launcher-terminal <<<"

echo "Building release binaries..."
(cd "$REPO_DIR" && cargo build --release)

mkdir -p "$BIN_DIR"
# install unlinks the destination before writing, so replacing a binary that
# is currently running works; cp writes into the running inode and fails
# with "Text file busy".
install -m755 "$REPO_DIR/target/release/lofi" "$BIN_DIR/lofi"
install -m755 "$REPO_DIR/target/release/lofi-daemon" "$BIN_DIR/lofi-daemon"
echo "Installed binaries to $BIN_DIR"

# Only seed a default config if the user doesn't already have one, so a
# re-run of this script never clobbers moods/sources someone has added.
mkdir -p "$CONFIG_DIR"
if [ ! -f "$CONFIG_DIR/config.toml" ]; then
    cp "$REPO_DIR/config.default.toml" "$CONFIG_DIR/config.toml"
    echo "Created default config at $CONFIG_DIR/config.toml"
else
    echo "Existing config at $CONFIG_DIR/config.toml left untouched"
fi

# Detect the user's actual shell from $SHELL rather than assuming one, so
# this works for both bash and zsh users without extra flags.
case "${SHELL:-}" in
    */zsh) RC_FILE="$HOME/.zshrc" ;;
    */bash) RC_FILE="$HOME/.bashrc" ;;
    *)
        echo "Could not detect bash or zsh from \$SHELL ($SHELL); add scripts/lofi-launcher.sh.in to your shell rc file manually."
        RC_FILE=""
        ;;
esac

if [ -n "$RC_FILE" ]; then
    # Marker check makes this idempotent: running install.sh again never
    # appends a second copy of the snippet.
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

# mpv and mpv-mpris are external dependencies this script doesn't install;
# warn rather than fail, since the rest of the install still succeeds.
if ! command -v mpv >/dev/null 2>&1; then
    echo "Warning: mpv not detected on PATH, install it via your distro's package manager for playback to work."
fi

# Kept in sync with crates/lofi-daemon/src/mpv.rs's MPRIS_SCRIPT_CANDIDATES
# and user-config-dir fallback, since this is a separate shell-side check
# only used to print an install-time hint, not the actual runtime detection.
USER_MPV_SCRIPTS_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/mpv/scripts"
if [ ! -e /usr/share/mpv/scripts/mpris.so ] \
    && [ ! -e /usr/lib/mpv/scripts/mpris.so ] \
    && [ ! -e /usr/local/share/mpv/scripts/mpris.so ] \
    && [ ! -e /usr/lib/mpv-mpris/mpris.so ] \
    && [ ! -e /etc/mpv/scripts/mpris.so ] \
    && [ ! -e "$USER_MPV_SCRIPTS_DIR/mpris.so" ] \
    && [ -z "${LOFI_MPV_MPRIS_SCRIPT:-}" ]; then
    echo "Note: mpv-mpris not detected, now-playing widgets (quickshell, playerctl, etc.) won't see this player. Playback still works without it."
fi

echo "Install complete. Open a new terminal to start playback automatically."
