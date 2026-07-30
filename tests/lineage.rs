//! End-to-end tests for query lineage (the tables and columns a query reads).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "test code"
)]

use googlesql::{ColumnDef, ColumnType, Error, Module, TableDef};

fn users_table() -> TableDef {
    TableDef {
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
    }
}

#[test]
fn reports_table_and_all_referenced_columns() {
    let mut module = Module::new().unwrap();

    let tables = module
        .referenced_tables("SELECT id, name FROM users", &[users_table()])
        .unwrap();

    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].name(), "users");
    assert_eq!(tables[0].columns(), ["id", "name"]);
}

#[test]
fn reports_only_columns_actually_read() {
    let mut module = Module::new().unwrap();

    // Only `id` is projected, so only `id` should appear in the lineage.
    let tables = module
        .referenced_tables("SELECT id FROM users", &[users_table()])
        .unwrap();

    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].name(), "users");
    assert_eq!(tables[0].columns(), ["id"]);
}

#[test]
fn reports_every_table_in_a_join() {
    let mut module = Module::new().unwrap();

    let orders = TableDef {
        name: "orders".to_string(),
        columns: vec![ColumnDef {
            name: "user_id".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    let tables = module
        .referenced_tables(
            "SELECT u.id FROM users AS u JOIN orders AS o ON u.id = o.user_id",
            &[users_table(), orders],
        )
        .unwrap();

    let names: Vec<&str> = tables.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"users"), "expected users, got: {names:?}");
    assert!(names.contains(&"orders"), "expected orders, got: {names:?}");
}

#[test]
fn propagates_analysis_error() {
    let mut module = Module::new().unwrap();

    let result = module.referenced_tables("SELECT x FROM missing_table", &[]);

    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected a GoogleSql error, got: {result:?}"
    );
}
