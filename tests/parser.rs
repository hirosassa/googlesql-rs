//! End-to-end tests for parser functionality (parse_statement → canonical SQL).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "test code"
)]

use googlesql::{Error, Module};

/// A valid SQL statement can be parsed and the canonical SQL string extracted.
#[test]
fn parses_and_canonicalizes_select() {
    let mut module = Module::new().unwrap();
    let parsed = module.parse_statement("select 1").unwrap();

    let canonical = parsed.canonical_sql();
    assert!(
        canonical.to_uppercase().contains("SELECT"),
        "canonical SQL must contain SELECT: {canonical:?}"
    );
    assert!(
        canonical.contains('1'),
        "canonical SQL must contain 1: {canonical:?}"
    );
}

/// A SQL statement with a syntax error returns a GoogleSql error.
#[test]
fn returns_error_for_invalid_sql() {
    let mut module = Module::new().unwrap();
    let err = module.parse_statement("SELECT FROM").unwrap_err();
    assert!(
        matches!(err, Error::GoogleSql(_)),
        "a syntax error must produce Error::GoogleSql: {err:?}"
    );
}
