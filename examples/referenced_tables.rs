//! Find the tables and columns a query reads with `referenced_tables`.
//!
//! Run with: `cargo run --example referenced_tables`

use googlesql::{ColumnDef, ColumnType, Module, TableDef};

fn main() -> Result<(), googlesql::Error> {
    let mut module = Module::new()?;

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
            ColumnDef {
                name: "email".to_string(),
                ty: ColumnType::String,
            },
        ],
    };
    let orders = TableDef {
        name: "orders".to_string(),
        columns: vec![
            ColumnDef {
                name: "user_id".to_string(),
                ty: ColumnType::Int64,
            },
            ColumnDef {
                name: "amount".to_string(),
                ty: ColumnType::Int64,
            },
        ],
    };

    let sql = "SELECT u.name, o.amount FROM users u JOIN orders o ON o.user_id = u.id";
    let tables = module.referenced_tables(sql, &[users, orders])?;

    println!("{sql}\n");
    for table in &tables {
        // The reported columns are pruned to those the query actually reads, so
        // `email` never shows up even though the catalog declares it.
        println!("{} reads: {}", table.name(), table.columns().join(", "));
    }
    Ok(())
}
