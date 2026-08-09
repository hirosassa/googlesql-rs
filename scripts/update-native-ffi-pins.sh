#!/usr/bin/env bash
# Updates the native-ffi Release tag and per-target cdylib SHA256 pins from the
# checksums published by .github/workflows/native-ffi-release.yml.
#
# Usage:
#   scripts/update-native-ffi-pins.sh <native-ffi-tag> < checksums
# where each checksums line is "<target> <sha256>" (the decompressed-cdylib hash).
# Rewrites NATIVE_FFI_TAG in build.rs and the matching pins in build_helpers.rs,
# then leaves the changes for review (typically on a branch, opened as a PR).
#
# Uses awk (not `sed -i`) so it behaves identically on GNU/Linux and BSD/macOS.
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <native-ffi-tag>  (checksums on stdin: '<target> <sha256>' per line)" >&2
  exit 2
fi
tag="$1"
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
build_rs="$repo_root/build.rs"
helpers="$repo_root/build_helpers.rs"

# 1. Point NATIVE_FFI_TAG at the released tag.
awk -v tag="$tag" '
  /const NATIVE_FFI_TAG: &str =/ {
    print "const NATIVE_FFI_TAG: &str = \"" tag "\";"; next
  }
  { print }
' "$build_rs" >"$build_rs.tmp"
mv "$build_rs.tmp" "$build_rs"

# 2. Replace each target's pin with its published SHA256.
while read -r target sha; do
  [[ -z "$target" ]] && continue
  if [[ ! "$sha" =~ ^[0-9a-f]{64}$ ]]; then
    echo "error: bad sha256 for $target: '$sha'" >&2
    exit 1
  fi
  if ! grep -q "(\"$target\"," "$helpers"; then
    echo "error: no pin entry for $target in build_helpers.rs" >&2
    exit 1
  fi
  awk -v t="$target" -v sha="$sha" '
    index($0, "(\"" t "\",") > 0 {
      match($0, /^[[:space:]]*/); indent = substr($0, 1, RLENGTH)
      print indent "(\"" t "\", \"" sha "\"),"; next
    }
    { print }
  ' "$helpers" >"$helpers.tmp"
  mv "$helpers.tmp" "$helpers"
  echo "pinned $target -> $sha"
done

echo "updated NATIVE_FFI_TAG=$tag and pins in build_helpers.rs; run 'cargo fmt', review, and commit."
