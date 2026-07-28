//! End-to-end tests for the SQL formatter (`FormatSql`).
#![allow(clippy::unwrap_used)]

use googlesql::{Error, Module};

#[test]
fn formats_select() {
    let mut module = Module::new().unwrap();

    let formatted = module.format_sql("select 1").unwrap();

    // FormatSql pretty-prints, uppercasing keywords and adding line breaks.
    assert!(
        formatted.contains("SELECT"),
        "expected uppercased keyword, got: {formatted:?}"
    );
    assert!(
        formatted.contains('1'),
        "expected the literal to survive, got: {formatted:?}"
    );
    assert!(
        formatted.contains('\n'),
        "expected multi-line output, got: {formatted:?}"
    );
}

#[test]
fn returns_error_for_invalid_sql() {
    let mut module = Module::new().unwrap();

    let result = module.format_sql("SELECT FROM");

    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected a GoogleSql error, got: {result:?}"
    );
}
