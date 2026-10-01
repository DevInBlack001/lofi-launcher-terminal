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
cp "$REPO_DIR/target/release/lofi" "$BIN_DIR/lofi"
cp "$REPO_DIR/target/release/lofi-daemon" "$BIN_DIR/lofi-daemon"
echo "Updated binaries in $BIN_DIR. Existing config and shell integration are preserved."
