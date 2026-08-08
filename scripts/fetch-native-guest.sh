#!/usr/bin/env bash
# Provisions the native guest tree from the prebuilt tarball instead of
# generating it, so `--features native` can be built without the wasm2rs /
# wasm-tools / python toolchain that scripts/gen-native-guest.sh needs.
#
# It downloads the pinned Release asset (scripts/native-guest.lock), or uses a
# local tarball when GUEST_TARBALL=/path/to.tar.gz is set (offline / CI), extracts
# it into native/, and verifies the extracted tree against the pinned content
# digest. A mismatch means the tarball does not match the pin — the tree is left
# in place for inspection but must not be trusted; regenerate or re-fetch.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
native_dir="$repo_root/native"
# shellcheck source=scripts/native-guest.lock
source "$repo_root/scripts/native-guest.lock"

tarball="${GUEST_TARBALL:-}"
downloaded=""
if [[ -z "$tarball" ]]; then
  tarball="$(mktemp)"
  downloaded="$tarball"
  echo "==> downloading $TARBALL_URL"
  curl -fsSL --retry 3 -o "$tarball" "$TARBALL_URL"
fi

echo "==> extracting into $native_dir"
mkdir -p "$native_dir"
tar -C "$native_dir" -xzf "$tarball"
[[ -n "$downloaded" ]] && rm -f "$downloaded"

echo "==> verifying content digest"
actual="$(bash "$repo_root/scripts/native-guest-digest.sh")"
if [[ "$actual" != "$CONTENT_SHA256" ]]; then
  echo "error: content digest mismatch — the tarball does not match the pin" >&2
  echo "  expected $CONTENT_SHA256" >&2
  echo "  actual   $actual" >&2
  exit 1
fi

echo "==> ok: native guest ready (digest $actual)"
echo "    build with: cargo build --release --features native"
