#!/usr/bin/env bash
# Fetches the Navidrome plugin SDK (nd-pdk, GPL-3.0; not on crates.io) into .deps/pdk at the pinned tag.
set -euo pipefail
TAG="${NAVIDROME_TAG:-v0.64.2}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
git clone --quiet --depth 1 --branch "$TAG" https://github.com/navidrome/navidrome.git "$TMP/navidrome"
rm -rf "$ROOT/.deps/pdk"
mkdir -p "$ROOT/.deps"
cp -r "$TMP/navidrome/plugins/pdk" "$ROOT/.deps/pdk"
rm -rf "$ROOT/.deps/pdk/go"
echo "nd-pdk from Navidrome $TAG is in .deps/pdk"
