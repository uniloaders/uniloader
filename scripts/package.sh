#!/usr/bin/env bash
# Zip the release build into dist/ for a GitHub release (and the RaiPal mod list on
# uniloaders.github.io, which downloads these):
#
#   uniloader-<version>-win64.zip            dwmapi.dll, plugins/
#   uniloader-overlay-<version>-win64.zip    overlay.dll
#   uniloader-lua-bridge-<version>-win64.zip lua_bridge.dll
#   SHA256SUMS
#
# Extracting the first into a game's exe folder is a complete install in the standard layout
# (the loader makes uniloader/ for its config and logs); the plugins' DLLs go into plugins/.
# Build first: scripts/gate.sh (its last step is the release build).
#
#   scripts/package.sh            package what is built
#   scripts/package.sh --check    list what would be packaged, write nothing
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RELEASE="$ROOT/target/x86_64-pc-windows-msvc/release"
DIST="$ROOT/dist"

die() { echo "error: $*" >&2; exit 1; }

CHECK=0
case "${1:-}" in
    --check) CHECK=1 ;;
    "") ;;
    *) die "unknown argument: $1" ;;
esac

version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -n1)"
[[ -n "$version" ]] || die "no version in Cargo.toml"

for dll in uniloader overlay lua_bridge; do
    [[ -f "$RELEASE/$dll.dll" ]] || die "not built: $RELEASE/$dll.dll (run scripts/gate.sh)"
done

loader="uniloader-$version-win64.zip"
overlay="uniloader-overlay-$version-win64.zip"
lua="uniloader-lua-bridge-$version-win64.zip"
echo "dist/$loader: dwmapi.dll (uniloader.dll), plugins/"
echo "dist/$overlay: overlay.dll"
echo "dist/$lua: lua_bridge.dll"
(( CHECK )) && exit 0

stage="$(mktemp -d)"
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/loader/plugins" "$stage/overlay" "$stage/lua" "$DIST"
cp -- "$RELEASE/uniloader.dll" "$stage/loader/dwmapi.dll"
cp -- "$RELEASE/overlay.dll" "$stage/overlay/"
cp -- "$RELEASE/lua_bridge.dll" "$stage/lua/"

rm -f -- "$DIST/$loader" "$DIST/$overlay" "$DIST/$lua" "$DIST/SHA256SUMS"
# -X: no extra file attributes (Unix owners, extended timestamps), which Windows ignores.
(cd "$stage/loader" && zip -qrX "$DIST/$loader" dwmapi.dll plugins)
(cd "$stage/overlay" && zip -qX "$DIST/$overlay" overlay.dll)
(cd "$stage/lua" && zip -qX "$DIST/$lua" lua_bridge.dll)
(cd "$DIST" && sha256sum "$loader" "$overlay" "$lua" > SHA256SUMS)
cat "$DIST/SHA256SUMS"
