//! End-to-end tests for the analyzer (`AnalyzeStatement`).
#![allow(clippy::unwrap_used)]

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
