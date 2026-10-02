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
        # The binary path is baked in absolute, not left as a bare "lofi",
        # since this block can end up running before any PATH additions
        # elsewhere in the rc file.
        {
            echo "$MARKER_START"
            sed "/^#/! s#\\blofi\\b#$BIN_DIR/lofi#g" "$REPO_DIR/scripts/lofi-launcher.sh.in"
            echo "$MARKER_END"
        } >> "$RC_FILE"
        echo "Added shell integration to $RC_FILE"
        echo "Note: if anything earlier in $RC_FILE execs into tmux, screen, or another"
        echo "shell (a bare 'tmux' line is common), this block won't run for a terminal"
        echo "that attaches there. Move the block (between the >>> and <<< markers) to"
        echo "before that line by hand, keeping it after any 'interactive shell only' check."
    fi
fi

# mpv and yt-dlp are external runtime dependencies this script doesn't
# build; offer to install whichever are missing via the detected package
# manager, since most users won't otherwise know these are required.
MISSING_DEPS=""
command -v mpv >/dev/null 2>&1 || MISSING_DEPS="$MISSING_DEPS mpv"
command -v yt-dlp >/dev/null 2>&1 || MISSING_DEPS="$MISSING_DEPS yt-dlp"

if [ -n "$MISSING_DEPS" ]; then
    # Detected by which package manager binary exists, not by distro name,
    # since that works the same way across a distro's derivatives too.
    # mpv-mpris is deliberately not auto-installed here, since it's optional
    # and the separate check below already covers it with its own notice.
    PKG_MANAGER=""
    if command -v pacman >/dev/null 2>&1; then
        PKG_MANAGER="pacman"
        INSTALL_CMD="sudo pacman -S --needed$MISSING_DEPS"
    elif command -v apt-get >/dev/null 2>&1; then
        PKG_MANAGER="apt-get"
        INSTALL_CMD="sudo apt-get install -y$MISSING_DEPS"
    elif command -v dnf >/dev/null 2>&1; then
        PKG_MANAGER="dnf"
        INSTALL_CMD="sudo dnf install -y$MISSING_DEPS"
    elif command -v zypper >/dev/null 2>&1; then
        PKG_MANAGER="zypper"
        INSTALL_CMD="sudo zypper install -y$MISSING_DEPS"
    elif command -v apk >/dev/null 2>&1; then
        PKG_MANAGER="apk"
        INSTALL_CMD="sudo apk add$MISSING_DEPS"
    fi

    if [ -n "$PKG_MANAGER" ] && [ -t 0 ]; then
        echo "Missing dependencies:$MISSING_DEPS"
        printf "Install with: %s\n" "$INSTALL_CMD"
        printf "Proceed? [y/N] "
        read -r REPLY
        case "$REPLY" in
            [yY]|[yY][eE][sS])
                if ! $INSTALL_CMD; then
                    echo "Warning: dependency install failed, continuing without it."
                fi
                ;;
            *)
                echo "Skipped. Install these yourself for playback to work."
                ;;
        esac
    elif [ -n "$PKG_MANAGER" ]; then
        echo "Warning: missing dependencies ($MISSING_DEPS) and not running in an interactive"
        echo "terminal, skipping the install prompt. Install with: $INSTALL_CMD"
    else
        echo "Warning: missing dependencies ($MISSING_DEPS) and no supported package manager"
        echo "(pacman, apt-get, dnf, zypper, apk) detected; install them yourself for playback to work."
    fi
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
