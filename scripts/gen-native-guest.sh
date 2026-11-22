#!/usr/bin/env bash
# Regenerates the wasm2rs `native/` artifacts (gitignored) consumed by the
# `native` feature:
#   native/guest/     the googlesql.wasm transpiled to standalone Rust (package `guest`;
#                     lib.rs + funcs_N.rs + data_N.bin, which lib.rs include_bytes!s)
#   native/dispatch.rs  the (svc,mid)/name -> Instance-method dispatch table
#
# Prerequisites:
#   - a checkout of wasm2rs (default: ../wasm2rs relative to this repo)
#   - wasm-tools on PATH (used by scripts/gen-native-dispatch.py)
#   - the SHA-pinned googlesql.wasm at spike/googlesql.wasm
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
wasm="${GOOGLESQL_WASM:-$repo_root/spike/googlesql.wasm}"
wasm2rs_dir="${WASM2RS_DIR:-$repo_root/../wasm2rs}"
out_dir="$repo_root/native/guest"

if [[ ! -f "$wasm" ]]; then
  echo "error: wasm not found at $wasm (set GOOGLESQL_WASM)" >&2
  exit 1
fi

echo "==> building wasm2rs ($wasm2rs_dir)"
cargo build --release --manifest-path "$wasm2rs_dir/Cargo.toml"
wasm2rs_bin="$wasm2rs_dir/target/release/wasm2rs"

echo "==> transpiling $wasm -> $out_dir (split: 200 funcs/file)"
mkdir -p "$out_dir"
# wasm2rs overwrites Cargo.toml with its own manifest (e.g. no `panic = "abort"`);
# restore the committed one (also on failure) so the tree stays clean and the
# digest is stable. Backed up before the delete so a missing manifest aborts first.
[[ -f "$out_dir/Cargo.toml" ]] || { echo "error: $out_dir/Cargo.toml missing" >&2; exit 1; }
manifest_backup="$(mktemp)"
trap 'cp "$manifest_backup" "$out_dir/Cargo.toml"; rm -f "$manifest_backup"' EXIT
cp "$out_dir/Cargo.toml" "$manifest_backup"
# The set of funcs_N.rs / data_N.bin files varies with the wasm and wasm2rs
# version; an orphan from a previous run would skew native-guest-digest.sh,
# which hashes every file under native/guest.
find "$out_dir" -type f ! -name Cargo.toml -delete
"$wasm2rs_bin" "$wasm" "$out_dir" 200 0

echo "==> generating native/dispatch.rs from the wasm export section"
python3 "$repo_root/scripts/gen-native-dispatch.py"

echo "==> done. Build with: cargo build --features native"
