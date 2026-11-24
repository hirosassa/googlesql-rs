#!/usr/bin/env bash
# Regenerates the wasm2rs `native/` artifacts (gitignored) consumed by the
# `native` feature:
#   native/guest/     the googlesql.wasm transpiled to standalone Rust (package `guest`;
#                     lib.rs + funcs_N.rs; wasm2rs >= 0.6 keeps data segments inline
#                     unless --data-files is passed, which this script does not)
#   native/dispatch.rs  the (svc,mid)/name -> Instance-method dispatch table
#
# Prerequisites:
#   - a checkout of wasm2rs (default: ../wasm2rs relative to this repo)
#   - wasm-tools on PATH (used by scripts/gen-native-dispatch.py)
#   - the SHA-pinned googlesql.wasm at spike/googlesql.wasm
#
# CALL_DEPTH_LIMIT: the wasm2rs `--call-depth-limit` the guest is generated with, so
# deeply nested SQL traps (surfacing as an ordinary `Error`) instead of overflowing
# the host's native stack and aborting the process. The default is the value pinned in
# scripts/native-guest.lock (the prebuilt guest's digest depends on it). Override it
# with `CALL_DEPTH_LIMIT=<n>`; `CALL_DEPTH_LIMIT=0` (or empty) generates a guest with no
# limit. Any value other than the pinned one yields a different content digest.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
wasm="${GOOGLESQL_WASM:-$repo_root/spike/googlesql.wasm}"
wasm2rs_dir="${WASM2RS_DIR:-$repo_root/../wasm2rs}"
out_dir="$repo_root/native/guest"
# shellcheck source=scripts/native-guest.lock
pinned_call_depth_limit="$(source "$repo_root/scripts/native-guest.lock" && echo "${CALL_DEPTH_LIMIT:?missing in native-guest.lock}")"
call_depth_limit="${CALL_DEPTH_LIMIT-$pinned_call_depth_limit}"
[[ "$call_depth_limit" == "0" ]] && call_depth_limit=""
if [[ -n "$call_depth_limit" && ! "$call_depth_limit" =~ ^[0-9]+$ ]]; then
  echo "error: CALL_DEPTH_LIMIT must be a non-negative integer, got '$call_depth_limit'" >&2
  exit 1
fi

if [[ ! -f "$wasm" ]]; then
  echo "error: wasm not found at $wasm (set GOOGLESQL_WASM)" >&2
  exit 1
fi

echo "==> building wasm2rs ($wasm2rs_dir)"
cargo build --release --manifest-path "$wasm2rs_dir/Cargo.toml"
wasm2rs_bin="$wasm2rs_dir/target/release/wasm2rs"

echo "==> transpiling $wasm -> $out_dir (split: 200 funcs/file, call-depth limit: ${call_depth_limit:-none})"
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
"$wasm2rs_bin" ${call_depth_limit:+--call-depth-limit="$call_depth_limit"} "$wasm" "$out_dir" 200 0

echo "==> generating native/dispatch.rs from the wasm export section"
python3 "$repo_root/scripts/gen-native-dispatch.py"

echo "==> done. Build with: cargo build --features native"
