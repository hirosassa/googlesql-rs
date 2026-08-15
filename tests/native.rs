//! Public-API smoke test for the native (wasm2rs) backend.
//!
//! This exercises the same entry point an external crate uses —
//! `Module::new_native` — so it fails to compile if that constructor is not
//! `pub`. It complements the in-crate differential tests (which reach the native
//! engine through crate-internal paths) by pinning the public surface itself.
//!
//! Built only when both `native` (the engine under test, which pulls in the
//! generated `guest` crate) and `wasmtime` (the baseline it is compared against
//! via `Module::new`) are enabled; run it with `--features wasmtime,native`.
//! docs.rs and the single-backend builds never see this file.
#![cfg(all(feature = "native", feature = "wasmtime"))]
#![allow(
    clippy::expect_used,
    reason = "test code: a failed setup step should surface as a panic"
)]

use googlesql::Module;

/// The native engine is constructible from the public API and, driven through
/// the same backend-agnostic methods as the default engine, agrees with it.
#[test]
fn new_native_is_usable_from_the_public_api() {
    let mut native = Module::new_native().expect("construct the native module");
    let mut wasmtime = Module::new().expect("construct the wasmtime module");

    let sql = "select 1 as n";
    assert_eq!(
        native.format_sql(sql).expect("native format_sql"),
        wasmtime.format_sql(sql).expect("wasmtime format_sql"),
        "the native backend must format identically to wasmtime through the public API",
    );
}
