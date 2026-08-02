#!/usr/bin/env bash
# Update the bundled googlesql.wasm pin in build.rs to a given release.
#
# Usage: scripts/update-wasm.sh vX.Y.Z
#
# Downloads the goccy/googlesql-wasm release asset for the requested version,
# recomputes its SHA256, and rewrites WASM_VERSION / WASM_URL / WASM_SHA256 in
# build.rs. Safe to re-run: passing the current version reproduces the same
# values and leaves build.rs unchanged.
set -euo pipefail

ver="${1:?usage: update-wasm.sh vX.Y.Z}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build_rs="${repo_root}/build.rs"
url="https://github.com/goccy/googlesql-wasm/releases/download/${ver}/googlesql.wasm"

# Compute the SHA256 of the release asset (sha256sum on Linux, shasum on macOS).
if command -v sha256sum >/dev/null 2>&1; then
  hasher=(sha256sum)
else
  hasher=(shasum -a 256)
fi
sha="$(curl -fsSL "$url" | "${hasher[@]}" | cut -d' ' -f1)"

if [ -z "$sha" ]; then
  echo "failed to compute sha256 for ${url}" >&2
  exit 1
fi

sed -i.bak -E \
  -e "s|(WASM_VERSION: &str = \")[^\"]*|\1${ver}|" \
  -e "s|(WASM_SHA256: &str = \")[^\"]*|\1${sha}|" \
  -e "s|(download/)v[0-9][0-9A-Za-z.-]*(/googlesql\.wasm)|\1${ver}\2|" \
  "$build_rs"
rm -f "${build_rs}.bak"

echo "updated build.rs -> ${ver} (sha256=${sha})"
