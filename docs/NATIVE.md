# Native backend (wasm2rs) — design and Phase 4 spike results

Target: replace the wasmtime engine with **googlesql.wasm transpiled to standalone
Rust** by [`wasm2rs`](https://github.com/hirosassa/wasm2rs), linked directly with no
wasm runtime and no JIT. This is the opt-in `native` engine of the two-backend
roadmap; the default engine stays wasmtime.

## Conclusion: viable — proven byte-for-byte against wasmtime for parse + format

The wasm2rs output type-checks and runs. With a hand-written host-imports shim and a
generated export dispatch, the native engine drives the same `GuestInstance` trait as
wasmtime and produces **identical results** for the timezone-independent paths
(parser, formatter). The analyzer needs one more piece (see *Timezone limitation*).

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
the SHA-pinned `spike/googlesql.wasm`.

```bash
scripts/gen-native-guest.sh          # transpile + generate native/guest and native/dispatch.rs
cargo test --features native --lib 'native_backend::'   # differential tests vs wasmtime
```

`native/` is gitignored (122MB, regenerated). `scripts/gen-native-dispatch.py` emits the
26,316-arm `(svc,mid) → func{N}` table plus the by-name table from the wasm export
section.

## Differential results

`src/native_backend.rs` (`#[cfg(test)]`) constructs both engines in one process and
compares output for the same SQL:

| Test | Result |
|---|---|
| `format_sql_matches_wasmtime` | ✅ native `format_sql` == wasmtime, byte for byte |
| `parse_statement_matches_wasmtime` | ✅ native canonical SQL == wasmtime |
| `parse_error_matches_wasmtime` | ✅ a syntax error is an `Err`, not a panic/success |

## Build gotchas (both are worked around via `[profile.dev.package.*]`)

1. **Optimizing `guest` is ruinously slow.** At the crate-wide dependency `opt-level = 3`
   the ~2.6M LOC build runs for tens of minutes. The spike only needs correctness, so
   `guest` is built at `opt-level = 0`. (Real native performance is a later, prebuilt-
   artifact concern.)
2. **Debug info on the inlined `guest` crashes rustc.** Monomorphizing/inlining the huge
   `guest` generics into this crate overflows LLVM's DWARF scope emitter
   (`DwarfCompileUnit::createAndAddScopeChildren`, SIGBUS). Building *this crate* with
   `debug = 0` avoids it without touching the deps' cached artifacts.

## Timezone limitation (blocks the native analyzer)

The analyzer's `AnalyzerOptions` constructor resolves a default timezone via absl cctz,
which reads `/usr/share/zoneinfo`. The wasmtime backend satisfies this by pre-opening
that directory into the WASI sandbox. The wasm2rs-generated WASI shim, however, pre-opens
only the process CWD (fd 3) and rejects any absolute path with `ENOTCAPABLE`, with no way
to configure a pre-open root — so the zoneinfo read fails and the analyzer traps. Parser
and formatter never construct `AnalyzerOptions`, so they are unaffected.

Closing this needs either a small wasm2rs change (a configurable WASI pre-open root) or
forcing a timezone that resolves without file I/O. Tracked as follow-up work.

## Landing status

Phase 4 proved the native engine; the feature wiring itself is **not yet merged**. A
gitignored path-dependency to the generated `guest` crate breaks a fresh checkout (Cargo
requires the path manifest to exist even when the feature is off), and CI's
`--all-features` would compile the 122MB crate. Making the engine buildable in CI —
generating or fetching `guest` as part of the build — is the next phase. This document
plus `scripts/` reproduce the working spike; the full shim is preserved on the
`spike/native-backend` branch.
