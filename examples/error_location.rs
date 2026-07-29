//! Read the source position of a GoogleSQL error from `SqlError`.
//!
//! Run with: `cargo run --example error_location`

use googlesql::{ColumnDef, ColumnType, Error, Module, TableDef};

fn main() -> Result<(), googlesql::Error> {
    let mut module = Module::new()?;

    let users = TableDef {
        name: "users".to_string(),
        columns: vec![ColumnDef {
            name: "id".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    // `missing_col` is not a column of `users`, so analysis fails with an
    // unresolved-name error pointing at column 8 (where `missing_col` starts).
    let sql = "SELECT missing_col FROM users";
    match module.analyze_output_columns(sql, &[users]) {
        Ok(_) => println!("unexpectedly analyzed: {sql}"),
        Err(Error::GoogleSql(err)) => {
            println!("message : {}", err.message());
            match err.location() {
                Some(loc) => println!("at      : line {}, column {}", loc.line(), loc.column()),
                None => println!("at      : (no location reported)"),
            }
        }
        Err(other) => println!("other error: {other}"),
    }
    Ok(())
}
