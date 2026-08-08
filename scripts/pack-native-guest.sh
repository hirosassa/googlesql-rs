#!/usr/bin/env bash
# Packs the generated native guest tree (native/guest/** + native/dispatch.rs)
# into a distributable tarball. Used by the release workflow to publish the
# asset, and by the CI validation job to exercise the fetch/extract roundtrip
# offline.
#
# The tarball's integrity anchor is the content digest (scripts/native-guest-digest.sh),
# not its bytes, so no reproducible-tar flags are needed. Prints the tarball path.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
native_dir="$repo_root/native"
out="${1:-$repo_root/native-guest.tar.gz}"

for required in guest/Cargo.toml dispatch.rs; do
  if [[ ! -e "$native_dir/$required" ]]; then
    echo "error: $native_dir/$required missing; run scripts/gen-native-guest.sh first" >&2
    exit 1
  fi
done

tar -C "$native_dir" -czf "$out" guest dispatch.rs
echo "$out"
