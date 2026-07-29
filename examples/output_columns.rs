//! Discover the output schema of a query with `analyze_output_columns`.
//!
//! Run with: `cargo run --example output_columns`

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
        ],
    };

    let sql = "SELECT id, name AS full_name FROM users";
    let columns = module.analyze_output_columns(sql, &[users])?;

    println!("{sql}\n");
    for col in &columns {
        // Each output column exposes its (aliased) name, resolved type, and the
        // unique id of the underlying resolved column.
        println!(
            "  {} : {} (column id {})",
            col.name(),
            col.type_name(),
            col.id()
        );
    }
    Ok(())
}
