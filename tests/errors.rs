//! End-to-end error-handling tests: classification and edge-case inputs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "test code"
)]

use googlesql::{ColumnDef, ColumnType, Error, Module, SqlErrorKind, TableDef};

fn users() -> Vec<TableDef> {
    vec![TableDef {
        name: "users".to_string(),
        columns: vec![ColumnDef {
            name: "id".to_string(),
            ty: ColumnType::Int64,
        }],
    }]
}

/// Extracts the `SqlError` from a result that must be an `Error::GoogleSql`.
fn expect_sql_error<T: std::fmt::Debug>(result: Result<T, Error>) -> googlesql::SqlError {
    match result {
        Err(Error::GoogleSql(e)) => e,
        other => panic!("expected Error::GoogleSql, got: {other:?}"),
    }
}

#[test]
fn syntax_error_is_classified_as_syntax() {
    let mut module = Module::new().unwrap();
    let err = expect_sql_error(module.parse_statement("SELECT FROM"));
    assert_eq!(err.kind(), SqlErrorKind::Syntax);
}

#[test]
fn empty_input_is_a_syntax_error() {
    let mut module = Module::new().unwrap();
    let err = expect_sql_error(module.parse_statement(""));
    assert_eq!(err.kind(), SqlErrorKind::Syntax);
}

#[test]
fn whitespace_only_input_is_a_syntax_error() {
    let mut module = Module::new().unwrap();
    let err = expect_sql_error(module.parse_statement("   \n\t  "));
    assert_eq!(err.kind(), SqlErrorKind::Syntax);
}

#[test]
fn unresolved_name_is_classified_as_analysis_with_a_location() {
    let mut module = Module::new().unwrap();
    let err = expect_sql_error(
        module.analyze_statement_with_catalog("SELECT x FROM missing_table", &users()),
    );
    assert_eq!(err.kind(), SqlErrorKind::Analysis);
    // Analysis errors carry a source position, unlike parser syntax errors.
    assert!(err.location().is_some(), "message: {}", err.message());
}

#[test]
fn recursive_cte_is_classified_as_unsupported() {
    let mut module = Module::new().unwrap();
    let err = expect_sql_error(module.analyze_statement_with_catalog(
        "WITH RECURSIVE r AS (SELECT 1) SELECT * FROM r",
        &users(),
    ));
    assert_eq!(err.kind(), SqlErrorKind::Unsupported);
}

#[test]
fn like_any_is_classified_as_unsupported() {
    let mut module = Module::new().unwrap();
    let err = expect_sql_error(module.analyze_statement_with_catalog(
        "SELECT id FROM users WHERE id LIKE ANY ('a', 'b')",
        &users(),
    ));
    assert_eq!(err.kind(), SqlErrorKind::Unsupported);
}

#[test]
fn multiline_query_reports_the_error_line_and_column() {
    let mut module = Module::new().unwrap();
    // `missing_col` sits on the second line, third column.
    let err = expect_sql_error(
        module.analyze_statement_with_catalog("SELECT\n  missing_col\nFROM users", &users()),
    );
    let location = err.location().expect("analysis error carries a location");
    assert_eq!((location.line(), location.column()), (2, 3));
}

#[test]
fn unicode_string_literal_analyzes_without_panicking() {
    let mut module = Module::new().unwrap();
    // A multi-byte string literal must round-trip through wasm linear memory
    // without corrupting byte offsets.
    let result = module.analyze_statement_with_catalog("SELECT '日本語のリテラル' AS label", &[]);
    assert!(result.is_ok(), "expected success, got: {result:?}");
}

#[test]
fn message_is_preserved_verbatim_for_every_kind() {
    let mut module = Module::new().unwrap();
    let err = expect_sql_error(module.parse_statement("SELECT FROM"));
    // Whatever the classification, the raw GoogleSQL text stays accessible.
    assert!(
        err.message().starts_with("Syntax error"),
        "message: {}",
        err.message()
    );
}
