//! End-to-end tests for the analyzer (`AnalyzeStatement`).
#![allow(clippy::unwrap_used)]

use googlesql::{Error, Module};

#[test]
fn analyzes_literal_select() {
    let mut module = Module::new().unwrap();

    // A literal SELECT needs no catalog entries, so it resolves against an
    // empty catalog.
    let result = module.analyze_statement("SELECT 1");

    assert!(result.is_ok(), "expected analysis to succeed, got: {result:?}");
}

#[test]
fn analyzes_builtin_operator() {
    let mut module = Module::new().unwrap();

    // `+` resolves to the builtin `$add` function, which requires the builtin
    // functions to be registered in the catalog.
    let result = module.analyze_statement("SELECT 1 + 2 AS x");

    assert!(result.is_ok(), "expected analysis to succeed, got: {result:?}");
}

#[test]
fn returns_error_for_unknown_table() {
    let mut module = Module::new().unwrap();

    // The catalog is empty, so referencing a table fails name resolution.
    let result = module.analyze_statement("SELECT x FROM missing_table");

    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected a GoogleSql error, got: {result:?}"
    );
}

#[test]
fn returns_error_for_invalid_sql() {
    let mut module = Module::new().unwrap();

    let result = module.analyze_statement("SELECT FROM");

    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected a GoogleSql error, got: {result:?}"
    );
}
