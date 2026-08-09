//! Public-API differential test for the native-ffi (C-ABI staticlib) backend.
//!
//! Exercises the same entry point an external crate uses —
//! `Module::new_native_ffi` — and checks it agrees with the default wasmtime
//! engine through the backend-agnostic methods. The `analyze` case in
//! particular drives the WASI/timezone path across the C ABI, which is the part
//! most likely to break at the boundary.
//!
//! Only built under the `native-ffi` feature, which links the prebuilt
//! `libguest_ffi.a`; the default build and docs.rs never see this file.
#![cfg(feature = "native-ffi")]
#![allow(
    clippy::expect_used,
    reason = "test code: a failed setup step should surface as a panic"
)]

use googlesql::Module;

/// A non-trivial statement: join + filter + order + limit, so parse and format
/// both walk a deep tree rather than a single node.
const SQL: &str = "SELECT u.id, u.name FROM users AS u \
     WHERE u.id > 10 ORDER BY u.name LIMIT 100";

/// The native-ffi engine is constructible from the public API and, driven
/// through the same backend-agnostic methods as the default engine, agrees with
/// it byte for byte across parse, format, and analyze.
#[test]
fn new_native_ffi_matches_wasmtime() {
    let mut ffi = Module::new_native_ffi().expect("construct the native-ffi module");
    let mut wasmtime = Module::new().expect("construct the wasmtime module");

    assert_eq!(
        ffi.format_sql(SQL).expect("native-ffi format_sql"),
        wasmtime.format_sql(SQL).expect("wasmtime format_sql"),
        "native-ffi must format identically to wasmtime through the public API",
    );

    assert_eq!(
        ffi.parse_statement(SQL)
            .expect("native-ffi parse")
            .canonical_sql(),
        wasmtime
            .parse_statement(SQL)
            .expect("wasmtime parse")
            .canonical_sql(),
        "native-ffi must parse to the same canonical SQL as wasmtime",
    );

    // Analyze drives the AnalyzerOptions timezone lookup, which reads absolute
    // paths through the WASI preopen; comparing the Debug form avoids requiring
    // `PartialEq` on the output type while still catching any divergence.
    assert_eq!(
        format!(
            "{:?}",
            ffi.analyze_output_columns("SELECT 1", &[])
                .expect("native-ffi analyze")
        ),
        format!(
            "{:?}",
            wasmtime
                .analyze_output_columns("SELECT 1", &[])
                .expect("wasmtime analyze")
        ),
        "native-ffi must analyze identically to wasmtime (WASI/timezone path)",
    );
}

/// A guest-side failure (here, a syntactically invalid statement) must surface
/// as an `Err` across the C ABI — the staticlib turns the guest's error into a
/// status the shim maps to `Error`, rather than unwinding across `extern "C"` or
/// aborting the process. Both engines must reject the same input, so this also
/// pins that the error path, not just the happy path, agrees with wasmtime.
#[test]
fn native_ffi_surfaces_errors_like_wasmtime() {
    const INVALID: &str = "SELECT FROM WHERE ORDER BY";

    let mut ffi = Module::new_native_ffi().expect("construct the native-ffi module");
    let mut wasmtime = Module::new().expect("construct the wasmtime module");

    let ffi_result = ffi.parse_statement(INVALID);
    let wasmtime_result = wasmtime.parse_statement(INVALID);

    assert!(
        ffi_result.is_err(),
        "native-ffi must reject invalid SQL with an Err (not a crash), got {ffi_result:?}",
    );
    assert!(
        wasmtime_result.is_err(),
        "sanity: wasmtime must also reject the same invalid SQL, got {wasmtime_result:?}",
    );
}
