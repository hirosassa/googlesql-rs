//! Walk a query's fully typed resolved AST with `resolved_tree`.
//!
//! Run with: `cargo run --example resolved_tree`

use googlesql::{ColumnDef, ColumnType, Module, ResolvedNode, TableDef};

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

    let sql = "SELECT id FROM users WHERE id > 0";
    println!("{sql}\n");
    if let Some(root) = module.resolved_tree(sql, &[users])? {
        print_tree(&root, 0);
    }
    Ok(())
}

/// Prints each node's kind (and resolved type, when it has one), indented by depth.
fn print_tree(node: &ResolvedNode, depth: usize) {
    let indent = "  ".repeat(depth);
    match node.type_name() {
        Some(ty) => println!("{indent}{} : {ty}", node.kind()),
        None => println!("{indent}{}", node.kind()),
    }
    for child in node.children() {
        print_tree(child, depth.saturating_add(1));
    }
}
