//! End-to-end tests for typed access to the analyzer's resolved output.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use googlesql::{ColumnDef, ColumnType, Error, Module, TableDef};

#[test]
fn output_columns_of_aliased_literal() {
    let mut module = Module::new().unwrap();

    // A bare literal projected under an alias becomes a single output column.
    let columns = module.analyze_output_columns("SELECT 1 AS x", &[]).unwrap();

    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].name(), "x");
    assert_eq!(columns[0].type_name(), "INT64");
}

#[test]
fn output_columns_of_user_table_query() {
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

    // Selecting the table's columns yields their names and resolved types in order.
    let columns = module
        .analyze_output_columns("SELECT id, name FROM users", &[users])
        .unwrap();

    assert_eq!(columns.len(), 2);
    assert_eq!(columns[0].name(), "id");
    assert_eq!(columns[0].type_name(), "INT64");
    assert_eq!(columns[1].name(), "name");
    assert_eq!(columns[1].type_name(), "STRING");
}

#[test]
fn output_columns_propagates_analysis_error() {
    let mut module = Module::new().unwrap();

    // Referencing an unknown table fails name resolution, and the error surfaces.
    let result = module.analyze_output_columns("SELECT x FROM missing_table", &[]);

    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected a GoogleSql error, got: {result:?}"
    );
}
