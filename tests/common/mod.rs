//! Shared by the native-engine integration tests (`tests/native.rs`,
//! `tests/native_ffi.rs`) that check deeply nested SQL is rejected with an
//! ordinary `Error` instead of overflowing the host's native stack.
#![allow(
    clippy::expect_used,
    reason = "test code: a failed setup step should surface as a panic"
)]

use googlesql::{Error, Module};

/// Stack of the analysis thread, in KiB: 512 MiB unless `GOOGLESQL_TEST_STACK_KB`
/// overrides it.
///
/// The call-depth limit (`CALL_DEPTH_LIMIT` in `scripts/native-guest.lock`) is sized
/// for the *optimized* guest, whose deepest allowed recursion fits an 8 MiB thread.
/// CI exercises an unoptimized guest (about 100x bigger frames, ~100 MB at the
/// limit), so the default is generous: the test still tells the limit apart from an
/// overflow because [`PAST_THE_LIMIT`] is beyond the limit but far below what would
/// overflow even 512 MiB. To check the 8 MiB claim against an optimized build, run
/// with `GOOGLESQL_TEST_STACK_KB=8192`.
fn analysis_stack_bytes() -> usize {
    std::env::var("GOOGLESQL_TEST_STACK_KB")
        .ok()
        .and_then(|kb| kb.parse::<usize>().ok())
        .map_or(512 << 20, |kb| kb << 10)
}

/// Beyond what the call-depth limit admits (about 160 nested subqueries or 450
/// chained terms at a limit of 1800) but nowhere near a guest that has no limit
/// failing on its own, so only the limit can make these an error.
pub const PAST_THE_LIMIT: usize = 2_000;

/// Comfortably below the limit, so it must keep analyzing fine.
pub const WITHIN_THE_LIMIT: usize = 100;

pub fn nested_subqueries(depth: usize) -> String {
    format!("SELECT {}1{}", "(SELECT ".repeat(depth), ")".repeat(depth))
}

pub fn chained_terms(terms: usize) -> String {
    format!("SELECT 1{}", "+1".repeat(terms))
}

/// Analyzes `sql` on a fresh `Module` made by `new`, on a thread with a known
/// stack, so the result does not depend on the stack the test harness happens to
/// give the test.
#[expect(
    clippy::unwrap_in_result,
    reason = "test helper: a failed spawn/join/setup is a test failure, while the returned Result is the engine outcome under test"
)]
fn analyze_on_sized_stack(new: fn() -> Result<Module, Error>, sql: String) -> Result<usize, Error> {
    std::thread::Builder::new()
        .stack_size(analysis_stack_bytes())
        .spawn(move || {
            let mut module = new().expect("construct the module");
            module
                .analyze_output_columns(&sql, &[])
                .map(|columns| columns.len())
        })
        .expect("spawn the analysis thread")
        .join()
        .expect("analysis thread must not panic")
}

/// Asserts the depth-limit behavior of an engine built from the current guest:
/// too-deep SQL is an `Error::Wasm` (no abort, no panic), and SQL within the limit
/// still analyzes.
pub fn assert_deep_sql_is_an_error(new: fn() -> Result<Module, Error>) {
    for sql in [
        nested_subqueries(PAST_THE_LIMIT),
        chained_terms(PAST_THE_LIMIT),
    ] {
        let result = analyze_on_sized_stack(new, sql.clone());
        assert!(
            matches!(result, Err(Error::Wasm(_))),
            "too-deep SQL must be a guest-trap Error::Wasm, got {result:?} for {}…",
            sql.chars().take(40).collect::<String>(),
        );
    }
    for sql in [
        nested_subqueries(WITHIN_THE_LIMIT),
        chained_terms(WITHIN_THE_LIMIT),
    ] {
        let result = analyze_on_sized_stack(new, sql);
        assert_eq!(result.ok(), Some(1), "SQL within the limit must analyze");
    }
}
