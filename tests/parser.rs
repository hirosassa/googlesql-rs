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

/// A `QUALIFY` clause parses and round-trips through the canonical SQL.
///
/// `QUALIFY` is gated behind a GoogleSQL language feature; the parser enables
/// the maximum language feature set so the clause is accepted.
#[test]
fn parses_qualify_clause() {
    let mut module = Module::new().unwrap();
    let sql = "SELECT a FROM t QUALIFY ROW_NUMBER() OVER (PARTITION BY b ORDER BY a) = 1";
    let parsed = module.parse_statement(sql).unwrap();

    assert!(
        parsed.canonical_sql().to_uppercase().contains("QUALIFY"),
        "canonical SQL must contain QUALIFY: {:?}",
        parsed.canonical_sql()
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
