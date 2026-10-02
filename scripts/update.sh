#!/usr/bin/env sh
set -eu

# Same path resolution as install.sh: relative to this script and the
# environment, so this targets whatever the user actually installed to.
REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"

# Only pull if this is a real git checkout; a tarball/package install has
# no .git directory, and that's fine, we just skip the pull.
if [ -d "$REPO_DIR/.git" ]; then
    echo "Pulling latest source..."
    (cd "$REPO_DIR" && git pull)
fi

echo "Rebuilding release binaries..."
(cd "$REPO_DIR" && cargo build --release)

# Overwrite the binaries only. Config and the shell-rc snippet are never
# touched here, so an update never loses a user's moods or sources.
mkdir -p "$BIN_DIR"
# install unlinks the destination before writing, so replacing a binary that
# is currently running works; cp writes into the running inode and fails
# with "Text file busy".
install -m755 "$REPO_DIR/target/release/lofi" "$BIN_DIR/lofi"
install -m755 "$REPO_DIR/target/release/lofi-daemon" "$BIN_DIR/lofi-daemon"
echo "Updated binaries in $BIN_DIR. Existing config and shell integration are preserved."

# A running daemon keeps executing the old binary until it exits. Stop it so
# the next lofi command auto-spawns the new one. -x matches the exact process
# name only, same as uninstall.sh.
if pkill -u "$(id -u)" -x lofi-daemon 2>/dev/null; then
    # Same safety net as uninstall.sh for an old daemon build that lacked a
    # SIGTERM handler and would orphan its mpv. Wait briefly for the daemon to
    # exit, but also have a fallback to kill any orphaned mpv after a timeout
    # to minimize the race window.
    KILL_DEADLINE=$(($(date +%s) + 1))
    while [ "$(date +%s)" -lt "$KILL_DEADLINE" ]; do
        if ! pgrep -u "$(id -u)" -x lofi-daemon >/dev/null 2>&1; then
            break
        fi
        sleep 0.05
    done
    pkill -u "$(id -u)" -f -- "--input-ipc-server=.*lofi-mpv\.sock" 2>/dev/null || true
    echo "Restarted lofi-daemon to pick up the update: the old daemon was stopped, so playback"
    echo "has stopped. The new daemon starts on the next lofi command and playback resumes when"
    echo "a new terminal opens. Terminals that were already open are not counted by the new"
    echo "daemon, so playback may stop while some of them are still open."
fi
