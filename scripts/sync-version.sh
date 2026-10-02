#!/usr/bin/env sh
set -eu

# version.json is the single source of truth for the project's version.
# Cargo can't read an external JSON file for its own `version` field, and
# PKGBUILD is a plain shell script, so this script propagates version.json
# into the places that actually need a literal version string. Run this
# after editing version.json, then review and commit the resulting diff.

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
VERSION_JSON="$REPO_DIR/version.json"

VERSION="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$VERSION_JSON")"
if [ -z "$VERSION" ]; then
    echo "Could not read a version from $VERSION_JSON" >&2
    exit 1
fi

echo "Syncing version $VERSION from version.json..."

# Workspace-level Cargo version; every crate inherits it via version.workspace = true.
sed -i "s/^version = \".*\"/version = \"$VERSION\"/" "$REPO_DIR/Cargo.toml"

# PKGBUILD's pkgver, and reset sha256sums since a new version means a new
# tarball with a different hash, only known once that version is released.
sed -i "s/^pkgver=.*/pkgver=$VERSION/" "$REPO_DIR/PKGBUILD"
sed -i "s/^sha256sums=(.*/sha256sums=('SKIP')/" "$REPO_DIR/PKGBUILD"

echo "Updating Cargo.lock..."
(cd "$REPO_DIR" && cargo build --workspace >/dev/null)

if command -v makepkg >/dev/null 2>&1; then
    echo "Regenerating .SRCINFO..."
    (cd "$REPO_DIR" && makepkg --printsrcinfo > .SRCINFO)
else
    echo "makepkg not found, skipping .SRCINFO regeneration (do this on an Arch machine before release)."
fi

echo "Done. Review the diff, then commit."
