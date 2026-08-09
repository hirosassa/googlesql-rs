# Native backend (wasm2rs) — design and Phase 4 spike results

Target: replace the wasmtime engine with **googlesql.wasm transpiled to standalone
Rust** by [`wasm2rs`](https://github.com/hirosassa/wasm2rs), linked directly with no
wasm runtime and no JIT. This is the opt-in `native` engine of the two-backend
roadmap; the default engine stays wasmtime.

## Conclusion: viable — proven byte-for-byte against wasmtime for parse, format, and analyze

The wasm2rs output type-checks and runs. With a hand-written host-imports shim and a
generated export dispatch, the native engine drives the same `GuestInstance` trait as
wasmtime and produces **identical results** across the parser, formatter, and analyzer.
The analyzer's timezone dependency is resolved by rooting the guest's single WASI preopen
at `/` (see *Timezone*), matching the wasmtime backend's read-only `/` preopen.

The engine is wired behind the opt-in `native` cargo feature (`Module::new_native`); the
default build is unchanged and uses wasmtime only.

## Architecture

```
googlesql.wasm ──wasm2rs──▶ native/guest/         (crate `guest`, ~122MB / 2.6M LOC)
                            pub struct Instance<H: Imports>
                            pub trait  Imports          (28 host imports: 25 C++ env + wasmify + 2 WASI)
                            pub fn     func{N}(&mut self, …)   (one per wasm function)

src/native_backend.rs  (feature = "native")
  HostImports         impl guest::Imports    — C++ runtime stubs (zero results; throw traps)
  NativeInstance      impl crate::GuestInstance — alloc/free/read/write/call_rpc/call_named
  include!("../native/dispatch.rs")           — (svc,mid)/name → func{N} fn-pointer dispatch
```

The engine surface is the crate's existing `GuestInstance` trait (`src/backend.rs`):
`alloc/free/write/read` + `call_rpc(svc,mid,ptr,len)` + `call_named(name,ptr,len)`. All
request marshaling and the packed-response decode stay engine-agnostic in `Module`, so
the native engine is purely the six raw operations. `Module::from_backend` is the
injection seam; the native engine plugs in as `Module::new_native()`.

The stub semantics mirror the wasmtime `env` stubs exactly (zero result values; a C++
`__cxa_throw` traps; `wasmify::callback_invoke` returns 0). That parity is what lets the
differential tests demand byte-for-byte agreement.

## Reproduction

Prerequisites: a `wasm2rs` checkout (default `../wasm2rs`), `wasm-tools` on `PATH`, and
the SHA-pinned `spike/googlesql.wasm` (or any `GOOGLESQL_WASM`-pointed v0.3.4 wasm — it
must match the SHA in `build.rs` so native and wasmtime compare the same module).

```bash
scripts/gen-native-guest.sh          # transpile + generate native/guest and native/dispatch.rs
cargo test --features native --lib 'native_backend::'   # differential tests vs wasmtime
```

`native/`'s generated sources are gitignored (~122MB, regenerated); only the guest crate's
tiny `native/guest/Cargo.toml` is committed, so Cargo can resolve the optional `guest`
path-dependency (and a fresh checkout / default build works) without the generated code
present. `scripts/gen-native-dispatch.py` emits the 26,316-arm `(svc,mid) → func{N}` table
plus the by-name table from the wasm export section.

## Using the prebuilt guest (no toolchain)

Generating the guest needs a `wasm2rs` checkout, `wasm-tools`, and `python3`. To skip that,
fetch the pre-generated tree from a GitHub Release instead:

```bash
scripts/fetch-native-guest.sh        # download + extract + verify native/guest and native/dispatch.rs
cargo build --release --features native
```

`scripts/native-guest.lock` pins the two inputs the tree is generated from — the
SHA-pinned `googlesql.wasm` (`v0.3.4`, via `build.rs`) and a `wasm2rs` commit — together
with `CONTENT_SHA256`, the canonical hash of the generated files
(`scripts/native-guest-digest.sh`). wasm2rs output is deterministic, so that digest is
reproducible from the pins; the fetch script verifies the extracted tree against it, so
integrity rests on file **content**, not the tar/gzip framing. `GUEST_TARBALL=/path.tar.gz`
uses a local tarball instead of downloading (offline / CI). The release workflow
(`.github/workflows/release-native-guest.yaml`, manual) regenerates from the pins, asserts
the digest, and publishes the tarball; the `prebuilt` job in `native.yaml` exercises the
pack → fetch → verify roundtrip on every native-touching PR.

What this does **not** remove is the one-time optimized compile: the `guest` crate builds at
`opt-level = 3` for real native performance (its `[profile.release]`), which takes tens of
minutes on first build and is then cached. A precompiled `rlib` is tied to an exact
rustc/target and so cannot be distributed portably — only the generated **source** is
prebuilt, not its object code. The `native-ffi` feature below removes that compile too.

## Prebuilt native-ffi (no compile) — the C-ABI cdylib

The `native-ffi` feature reaches the *same* wasm2rs engine as `native`, but through a flat
C ABI exported by a prebuilt shared library (`native/guest-ffi`, a `cdylib`) instead of the
`guest` path-dependency. It unlocks `Module::new_native_ffi()`; every method past
construction is backend-agnostic, so the engine choice is invisible. Because the optimized
(opt-level-3) object code ships in the cdylib, a consumer links it in seconds instead of
paying the tens-of-minutes `guest` compile.

Why a cdylib and not a `staticlib` (`.a`)? An `rlib` is rustc-version-locked; a C-ABI object
archive is not, but a Rust `staticlib` still **leaks one fixed-name std symbol**
(`rust_eh_personality`), so linking it into a consumer built with a *different* rustc
collides on that symbol — a warning on macOS `ld`, a hard error on GNU `ld`. A `cdylib` keeps
its bundled `std` internal and exports only the flat C entry points, so it links cleanly
under **any** consumer toolchain (verified across stable 1.94, 1.96, and nightly). The C
boundary is per-query (six ops: alloc/write/call/read/free), so the indirection is
negligible — single-threaded parse/format/analyze latencies match the directly-linked
`native` engine.

```bash
# Default: build.rs downloads the prebuilt cdylib for your target and links it.
cargo build --release --features native-ffi

# Local development, or a target with no published prebuilt: build the cdylib
# yourself and point GUEST_FFI_LIB at the output directory.
cargo build --release --manifest-path native/guest-ffi/Cargo.toml
GUEST_FFI_LIB=native/guest-ffi/target/release cargo build --release --features native-ffi
```

`build.rs` resolves the cdylib in priority order, mirroring the wasm above:

1. **`GUEST_FFI_LIB`** — a directory holding a locally built cdylib (`libguest_ffi.{dylib,so}`
   / `guest_ffi.dll`). Used as-is; nothing is downloaded.
2. A **verified cached copy** in `OUT_DIR` from an earlier build.
3. **Download** the target's asset from the `native-ffi-v*` GitHub Release, zstd-decompress
   it, and verify its SHA256 (pinned per target in `build_helpers.rs`) before caching and
   linking it. `GUEST_FFI_URL_BASE` overrides the download base (a mirror, or a `file://`
   directory for offline builds).

An `rpath` to the chosen directory is baked in so the binary finds the shared library at
runtime. Note the relocatability limitation: on the download path that directory is `OUT_DIR`
(under `target/`), so a binary built with `native-ffi` is **not** self-contained — moving it,
`cargo install`, or `cargo clean` breaks the link. For a relocatable binary, keep `OUT_DIR`
in place, or use the `GUEST_FFI_LIB` path and ship `libguest_ffi.{dylib,so}` alongside the
executable (with an `@loader_path`/`$ORIGIN` rpath).

A target absent from the pin table in `build_helpers.rs`, or one whose pin is still the
`PENDING_RELEASE` sentinel, has no usable download: the build fails with a clear message and
you must build from source and set `GUEST_FFI_LIB`. Shipped targets: `aarch64-apple-darwin`,
`x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`.

**Publishing.** `.github/workflows/native-ffi-release.yml` (triggered by pushing a
`native-ffi-v*` tag) rebuilds the cdylib for every target from the pinned inputs — the same
`googlesql.wasm` and `wasm2rs` commit as `native-guest.lock`, digest-asserted — compresses
each with zstd, and uploads them to that Release. It then prints the published checksums;
feed them into `scripts/update-native-ffi-pins.sh <tag>` to rewrite `NATIVE_FFI_TAG`
(`build.rs`) and the per-target pins (`build_helpers.rs`), and commit the result before
downstream builds can fetch. The `native-ffi` job in `native.yaml` exercises the whole
fetch → decompress → verify → link → differential-test chain on every native-touching PR.

## Differential results

`src/native_backend.rs` (`#[cfg(test)]`) constructs both engines in one process and
compares output for the same SQL:

| Test | Result |
|---|---|
| `format_sql_matches_wasmtime` | ✅ native `format_sql` == wasmtime, byte for byte |
| `parse_statement_matches_wasmtime` | ✅ native canonical SQL == wasmtime |
| `parse_error_matches_wasmtime` | ✅ a syntax error is an `Err`, not a panic/success |
| `analyze_expression_matches_wasmtime` | ✅ native inferred type == wasmtime, byte for byte (timezone path) |
| `analyze_statement_matches_wasmtime` | ✅ native analysis succeeds where wasmtime does |

## Build gotchas (both are worked around via `[profile.dev.package.*]`)

1. **Optimizing `guest` is ruinously slow.** At the crate-wide dependency `opt-level = 3`
   the ~2.6M LOC build runs for tens of minutes. The spike only needs correctness, so
   `guest` is built at `opt-level = 0`. (Real native performance is a later, prebuilt-
   artifact concern.)
2. **Debug info on the inlined `guest` crashes rustc.** Monomorphizing/inlining the huge
   `guest` generics into this crate overflows LLVM's DWARF scope emitter
   (`DwarfCompileUnit::createAndAddScopeChildren`, SIGBUS). Building *this crate* with
   `debug = 0` avoids it without touching the deps' cached artifacts.

## Timezone (resolved)

The analyzer's `AnalyzerOptions` constructor resolves a default timezone via absl cctz,
which reads absolute paths under `/usr/share/zoneinfo` (and may consult `/etc/localtime`).
The wasm2rs-generated WASI shim originally pre-opened only the process CWD (fd 3) and
rejected any absolute path with `ENOTCAPABLE`, so the read failed and the analyzer trapped.

wasm2rs now supports a configurable WASI pre-open root (`Instance::with_preopen_root`), and
`NativeInstance::new` roots the single preopen at `/` — the same read-only `/` preopen the
wasmtime backend already relies on. Those absolute reads now resolve, and the analyzer
matches wasmtime (the resolved *value* of the default zone is irrelevant to the analyzed
type, which is what the differential tests compare). Containment is unchanged: the shim
still refuses `..` escapes lexically, so `/` grants read-only reach, not traversal out.

## Landing status

Landed behind the opt-in `native` feature. `Module::new()` is unchanged (wasmtime); the
native engine is the public `Module::new_native()` (a public-API smoke test in
`tests/native.rs` pins that surface, alongside the in-crate differential tests). Only the
guest crate's `native/guest/Cargo.toml`
is committed (the ~122MB generated sources stay gitignored), which is enough for Cargo to
resolve the optional path-dependency so a fresh checkout and the default build/CI are
unaffected. The default `test` workflow no longer runs `--all-features`; the dedicated
`native` workflow regenerates `guest` from the pinned wasm and runs clippy + the
differential tests.

The generated guest is also published as a Release asset so the `native` feature can be
built without the wasm2rs toolchain — see [Using the prebuilt guest](#using-the-prebuilt-guest-no-toolchain).

No remaining follow-ups from the spike: the native engine is now public
(`Module::new_native()`), and the response-buffer leak on the read-error path was fixed in
 #140. A precompiled (optimized) guest artifact is out of scope — an `rlib` is not portably
distributable, so only the generated source is prebuilt (see the note above).
