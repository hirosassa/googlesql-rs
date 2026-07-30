//! End-to-end tests for the analyzer (`AnalyzeStatement`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "test code"
)]

use googlesql::{ColumnDef, ColumnType, Error, Module, TableDef};

#[test]
fn analyzes_literal_select() {
    let mut module = Module::new().unwrap();

    // A literal SELECT needs no catalog entries, so it resolves against an
    // empty catalog.
    let result = module.analyze_statement("SELECT 1");

    assert!(
        result.is_ok(),
        "expected analysis to succeed, got: {result:?}"
    );
}

#[test]
fn analyzes_builtin_operator() {
    let mut module = Module::new().unwrap();

    // `+` resolves to the builtin `$add` function, which requires the builtin
    // functions to be registered in the catalog.
    let result = module.analyze_statement("SELECT 1 + 2 AS x");

    assert!(
        result.is_ok(),
        "expected analysis to succeed, got: {result:?}"
    );
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
fn analysis_error_carries_its_source_location() {
    let mut module = Module::new().unwrap();

    let users = TableDef {
        name: "users".to_string(),
        columns: vec![ColumnDef {
            name: "id".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    // `missing_col` starts at column 8 of the single-line query; GoogleSQL
    // reports the error there, and we expose that position structurally.
    let Err(Error::GoogleSql(err)) =
        module.analyze_output_columns("SELECT missing_col FROM users", &[users])
    else {
        panic!("expected an unresolved-name error");
    };

    assert_eq!(
        err.location().map(|loc| (loc.line(), loc.column())),
        Some((1, 8))
    );
    assert!(
        err.message().starts_with("Unrecognized name: missing_col"),
        "unexpected message: {}",
        err.message()
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

#[test]
fn analyzes_query_against_user_table() {
    let mut module = Module::new().unwrap();

    let users = TableDef {
        name: "users".to_string(),
        columns: vec![
            ColumnDef {
                name: "id".to_string(),
                ty: ColumnType::Int64,
            },
            ColumnDef {
                name: "name".to_string(),
                ty: ColumnType::String,
            },
        ],
    };

    // With the table registered in the catalog, its columns resolve.
    let result = module.analyze_statement_with_catalog("SELECT id, name FROM users", &[users]);

    assert!(
        result.is_ok(),
        "expected analysis to succeed, got: {result:?}"
    );
}

#[test]
fn returns_error_for_unknown_column_in_user_table() {
    let mut module = Module::new().unwrap();

    let users = TableDef {
        name: "users".to_string(),
        columns: vec![ColumnDef {
            name: "id".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    // The table exists but the column does not, so name resolution fails.
    let result = module.analyze_statement_with_catalog("SELECT missing_col FROM users", &[users]);

    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected a GoogleSql error, got: {result:?}"
    );
}

#[test]
fn resolves_columns_of_each_scalar_type() {
    let mut module = Module::new().unwrap();

    // Each ColumnType maps to a TypeFactory getter; registering a column of that
    // type and reading back the resolved output column's type name proves the
    // round-trip (SQL type name == the type we asked the factory for).
    let cases = [
        (ColumnType::Bytes, "BYTES"),
        (ColumnType::Date, "DATE"),
        (ColumnType::Datetime, "DATETIME"),
        (ColumnType::Time, "TIME"),
        (ColumnType::Timestamp, "TIMESTAMP"),
        (ColumnType::Numeric, "NUMERIC"),
        (ColumnType::BigNumeric, "BIGNUMERIC"),
        (ColumnType::Json, "JSON"),
        (ColumnType::Interval, "INTERVAL"),
        (ColumnType::Geography, "GEOGRAPHY"),
    ];

    for (ty, expected) in cases {
        let table = TableDef {
            name: "t".to_string(),
            columns: vec![ColumnDef {
                name: "c".to_string(),
                ty,
            }],
        };

        let columns = module
            .analyze_output_columns("SELECT c FROM t", &[table])
            .unwrap_or_else(|e| panic!("analysis failed for {expected}: {e:?}"));

        assert_eq!(
            columns.len(),
            1,
            "expected one output column for {expected}"
        );
        assert_eq!(
            columns[0].type_name(),
            expected,
            "resolved type name mismatch for {expected}"
        );
    }
}

#[test]
fn analyze_statement_with_empty_catalog_matches_phase_one() {
    let mut module = Module::new().unwrap();

    // An empty catalog behaves exactly like `analyze_statement`: a bare literal
    // resolves, but any table reference fails.
    assert!(
        module
            .analyze_statement_with_catalog("SELECT 1", &[])
            .is_ok()
    );
    assert!(matches!(
        module.analyze_statement_with_catalog("SELECT x FROM missing_table", &[]),
        Err(Error::GoogleSql(_))
    ));
}
