#!/usr/bin/env bash
# Prints the canonical content digest of the generated native guest tree
# (native/guest/** plus native/dispatch.rs).
#
# The digest is a SHA-256 over one normalized `<sha256>  <relpath>` line per
# file, in a stable byte order (LC_ALL=C sort). It hashes file CONTENT, not the
# tar/gzip framing that ships it, so it is identical across platforms and
# reproducible from the pinned inputs (see scripts/native-guest.lock). The line
# is formatted here rather than taken from the hashing tool's own output, so GNU
# `sha256sum` (Linux) and `shasum -a 256` (macOS) yield the same digest.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
native_dir="$repo_root/native"

if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1"; }
else
  sha256() { shasum -a 256 "$1"; }
fi

cd "$native_dir"
# Feed each file's hash+path line through the outer hash. `find | sort` fixes the
# file set and order; the inner `awk` keeps only the hash column so the tool's
# text/binary marker never leaks into the stream.
while IFS= read -r f; do
  printf '%s  %s\n' "$(sha256 "$f" | awk '{print $1}')" "$f"
done < <(find guest dispatch.rs -type f | LC_ALL=C sort) | {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi
} | awk '{print $1}'
