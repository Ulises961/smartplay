#!/usr/bin/env bash
# Fails unless the version of the crates (Cargo.toml), the one the plugin reports (its manifest) and, when given, the
# release tag are the same one:   scripts/check-version.sh [vX.Y.Z]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

crates="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml")"
plugin="$(sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' "$ROOT/crates/plugin/manifest.json" | head -n 1)"
echo "crates: $crates   plugin manifest: $plugin${1:+   tag: ${1#v}}"

[ -n "$crates" ] && [ -n "$plugin" ] || { echo "could not read a version" >&2; exit 1; }
[ "$crates" = "$plugin" ] || { echo "the crates and the plugin manifest have different versions" >&2; exit 1; }
if [ $# -ge 1 ] && [ "${1#v}" != "$crates" ]; then
  echo "the tag $1 is not the version $crates of the crates and the plugin" >&2
  exit 1
fi
