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

// Note: the analyzer enables all statement kinds, so DML/DDL (INSERT,
// CREATE TABLE, ALTER MODEL, ...) now resolve instead of reporting
// "Statement not supported". The `Unsupported` classification itself — the
// `not supported` phrase heuristic — is exercised directly in the unit tests
// in `src/error.rs`, which is stable regardless of which features this
// particular GoogleSQL build implements.

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
fn error_offset_locates_the_token_after_a_multibyte_literal() {
    let mut module = Module::new().unwrap();
    // GoogleSQL reports the column as a code-point count, so `missing_col` sits
    // at a column that is smaller than its byte position. `offset` must recover
    // the real byte position against the original UTF-8 source.
    let sql = "SELECT '日本語' AS x, missing_col";
    let err = expect_sql_error(module.analyze_statement_with_catalog(sql, &[]));
    let offset = err
        .location()
        .expect("analysis error carries a location")
        .offset(sql)
        .expect("location resolves to a byte offset");
    assert!(
        sql[offset..].starts_with("missing_col"),
        "offset {offset} landed on {:?}",
        &sql[offset..]
    );
}

#[test]
fn error_caret_snippet_marks_the_syntax_error_position() {
    let mut module = Module::new().unwrap();
    // An incomplete statement analyzed against an empty catalog: the analyzer
    // parses first and reports the syntax error with a position (column 14, one
    // past the end of the 13-character input).
    let sql = "SELECT a FROM";
    let err = expect_sql_error(module.analyze_statement_with_catalog(sql, &[]));
    let snippet = err
        .caret_snippet(sql)
        .expect("located syntax error yields a snippet");
    // The caret sits under column 14: 13 spaces then `^`.
    assert_eq!(snippet, format!("{sql}\n{}^", " ".repeat(13)));
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
