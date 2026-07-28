//! Guards that multiple `Module` instances stay fully isolated.
//!
//! The runtime shares a single compiled wasm module across all instances for
//! speed; this test proves that sharing the immutable compiled code does not
//! leak any mutable state between live instances.
#![allow(clippy::unwrap_used)]

use googlesql::{Error, Module};

#[test]
fn instances_are_independent() {
    let mut a = Module::new().unwrap();
    let mut b = Module::new().unwrap();

    // Work is interleaved across both live instances: creating `b` must not
    // disturb `a`, and an error on one must not affect the other.
    assert!(a.analyze_statement("SELECT 1").is_ok());
    assert!(b.analyze_statement("SELECT 1 + 2 AS x").is_ok());

    assert!(matches!(
        a.analyze_statement("SELECT x FROM missing_table"),
        Err(Error::GoogleSql(_))
    ));

    // `a` still resolves correctly after its own failed call and after `b`'s use.
    assert!(a.analyze_statement("SELECT 1").is_ok());
    assert!(b.analyze_statement("SELECT 1").is_ok());
}
