//! End-to-end tests for the resolved AST tree (the analyzer's typed output tree).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use googlesql::{ColumnDef, ColumnType, Error, Module, ResolvedNode, TableDef};

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

/// True if `node` or any of its descendants has the given kind.
fn contains_kind(node: &ResolvedNode, kind: &str) -> bool {
    node.kind() == kind
        || node
            .children()
            .iter()
            .any(|child| contains_kind(child, kind))
}

#[test]
fn root_of_a_query_is_a_resolved_query_stmt() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .expect("a query should produce a resolved tree");

    assert_eq!(root.kind(), "ResolvedQueryStmt");
}

#[test]
fn tree_contains_the_scanned_table() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    assert!(
        contains_kind(&root, "ResolvedTableScan"),
        "expected a ResolvedTableScan somewhere in the tree"
    );
}

#[test]
fn literal_query_has_a_tree_without_a_table_scan() {
    let mut module = Module::new().unwrap();

    let root = module.resolved_tree("SELECT 1", &[]).unwrap().unwrap();

    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(
        !contains_kind(&root, "ResolvedTableScan"),
        "a literal query reads no tables"
    );
}

/// True if `node` or any of its descendants reports the given resolved type name.
fn contains_type(node: &ResolvedNode, type_name: &str) -> bool {
    node.type_name() == Some(type_name)
        || node
            .children()
            .iter()
            .any(|child| contains_type(child, type_name))
}

#[test]
fn expression_nodes_carry_their_resolved_type() {
    let mut module = Module::new().unwrap();

    let root = module.resolved_tree("SELECT 1", &[]).unwrap().unwrap();

    // The literal `1` resolves to INT64 and appears as an expression node, so
    // some node in the tree reports that type.
    assert!(
        contains_type(&root, "INT64"),
        "expected an INT64-typed expression node in the tree"
    );
}

#[test]
fn non_expression_nodes_have_no_type() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement node is not an expression, so it carries no resolved type.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.type_name(), None);
}

#[test]
fn propagates_analysis_error() {
    let mut module = Module::new().unwrap();

    let result = module.resolved_tree("SELECT x FROM missing_table", &[]);

    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected a GoogleSql error, got: {result:?}"
    );
}
