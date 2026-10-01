#!/usr/bin/env sh
set -eu

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"

if [ -d "$REPO_DIR/.git" ]; then
    echo "Pulling latest source..."
    (cd "$REPO_DIR" && git pull)
fi

echo "Rebuilding release binaries..."
(cd "$REPO_DIR" && cargo build --release)

mkdir -p "$BIN_DIR"
cp "$REPO_DIR/target/release/lofi" "$BIN_DIR/lofi"
cp "$REPO_DIR/target/release/lofi-daemon" "$BIN_DIR/lofi-daemon"
echo "Updated binaries in $BIN_DIR. Existing config and shell integration are preserved."
