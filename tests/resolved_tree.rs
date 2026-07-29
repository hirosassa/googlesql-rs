//! End-to-end tests for the resolved AST tree (the analyzer's typed output tree).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use googlesql::{ColumnDef, ColumnType, Error, LiteralValue, Module, ResolvedNode, TableDef};

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

/// Returns the first node in the tree whose kind matches, if any.
fn find_kind<'a>(node: &'a ResolvedNode, kind: &str) -> Option<&'a ResolvedNode> {
    if node.kind() == kind {
        return Some(node);
    }
    node.children()
        .iter()
        .find_map(|child| find_kind(child, kind))
}

#[test]
fn column_ref_nodes_carry_their_source_column() {
    let mut module = Module::new().unwrap();

    // `id` used inside an expression becomes a ResolvedColumnRef pointing back
    // at the `users.id` column it reads.
    let root = module
        .resolved_tree("SELECT id + 1 AS x FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let column_ref = find_kind(&root, "ResolvedColumnRef")
        .expect("an expression over a column should produce a ResolvedColumnRef");
    let reference = column_ref
        .column_ref()
        .expect("a ResolvedColumnRef node exposes its source column");

    assert_eq!(reference.table(), "users");
    assert_eq!(reference.name(), "id");
}

#[test]
fn non_column_ref_nodes_have_no_column_reference() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a column reference.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.column_ref().is_none());
}

/// Collects every column reference in the tree as `(table, name)` pairs.
fn collect_column_refs(node: &ResolvedNode, out: &mut Vec<(String, String)>) {
    if let Some(reference) = node.column_ref() {
        out.push((reference.table().to_string(), reference.name().to_string()));
    }
    for child in node.children() {
        collect_column_refs(child, out);
    }
}

#[test]
fn column_ref_to_a_computed_column_names_its_synthetic_table() {
    let mut module = Module::new().unwrap();

    // `ORDER BY a` references the computed column `a`, which is produced by the
    // projection rather than a user table. The analyzer gives such intermediate
    // columns a synthetic table name (e.g. `$query`), so the reference still
    // resolves with a non-empty table and name.
    let root = module
        .resolved_tree("SELECT id + 1 AS a FROM users ORDER BY a", &[users_table()])
        .unwrap()
        .unwrap();

    let mut refs = Vec::new();
    collect_column_refs(&root, &mut refs);

    // The base column read from the user table.
    assert!(refs.contains(&("users".to_string(), "id".to_string())));
    // The computed column referenced by ORDER BY: a synthetic, non-empty table.
    let computed = refs
        .iter()
        .find(|(_, name)| name == "a")
        .expect("ORDER BY references the computed column `a`");
    assert!(
        !computed.0.is_empty(),
        "a computed column still reports a (synthetic) table name, got empty"
    );
}

#[test]
fn literal_nodes_carry_their_integer_value() {
    let mut module = Module::new().unwrap();

    // A bare integer constant resolves to a ResolvedLiteral holding an INT64.
    let root = module.resolved_tree("SELECT 42", &[]).unwrap().unwrap();

    let literal = find_kind(&root, "ResolvedLiteral")
        .expect("a constant in the SELECT list produces a ResolvedLiteral");
    assert_eq!(literal.literal_value(), Some(&LiteralValue::Int64(42)));
}

#[test]
fn literal_nodes_carry_bool_string_and_double_values() {
    let mut module = Module::new().unwrap();

    // Each constant resolves to a ResolvedLiteral of the matching scalar type,
    // so its value comes back as the corresponding LiteralValue variant.
    let cases: [(&str, LiteralValue); 3] = [
        ("SELECT TRUE", LiteralValue::Bool(true)),
        ("SELECT 'hi'", LiteralValue::String("hi".to_string())),
        ("SELECT 2.5", LiteralValue::Double(2.5)),
    ];

    for (sql, expected) in cases {
        let root = module.resolved_tree(sql, &[]).unwrap().unwrap();
        let literal = find_kind(&root, "ResolvedLiteral")
            .unwrap_or_else(|| panic!("{sql} should produce a ResolvedLiteral"));
        assert_eq!(literal.literal_value(), Some(&expected), "for {sql}");
    }
}

#[test]
fn non_literal_nodes_have_no_literal_value() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a literal.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.literal_value().is_none());
}

#[test]
fn function_call_nodes_carry_their_function_name() {
    let mut module = Module::new().unwrap();

    // `id + 1` resolves to a scalar function call over the built-in add function,
    // whose catalog name is `$add`.
    let root = module
        .resolved_tree("SELECT id + 1 AS x FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let call =
        find_kind(&root, "ResolvedFunctionCall").expect("`id + 1` produces a ResolvedFunctionCall");
    assert_eq!(call.function_name(), Some("$add"));
}

#[test]
fn named_function_calls_report_their_catalog_name() {
    let mut module = Module::new().unwrap();

    // A named scalar function keeps its plain catalog name (no `$` prefix).
    let root = module
        .resolved_tree("SELECT lower(name) AS n FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let call = find_kind(&root, "ResolvedFunctionCall")
        .expect("`lower(name)` produces a ResolvedFunctionCall");
    assert_eq!(call.function_name(), Some("lower"));
}

#[test]
fn non_function_call_nodes_have_no_function_name() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a function call.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.function_name().is_none());
}

#[test]
fn table_scan_nodes_carry_their_table_name() {
    let mut module = Module::new().unwrap();

    // The FROM clause becomes a ResolvedTableScan that reads the `users` table.
    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let scan =
        find_kind(&root, "ResolvedTableScan").expect("`FROM users` produces a ResolvedTableScan");
    assert_eq!(scan.table_name(), Some("users"));
}

#[test]
fn aliased_table_scans_report_the_physical_table_name() {
    let mut module = Module::new().unwrap();

    // An alias renames the table for the query, but the scan still reads the
    // physical `users` table, so its node reports the catalog name, not `u`.
    let root = module
        .resolved_tree("SELECT u.id FROM users AS u", &[users_table()])
        .unwrap()
        .unwrap();

    let scan = find_kind(&root, "ResolvedTableScan")
        .expect("`FROM users AS u` produces a ResolvedTableScan");
    assert_eq!(scan.table_name(), Some("users"));
}

#[test]
fn non_table_scan_nodes_have_no_table_name() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a table scan.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.table_name().is_none());
}

#[test]
fn cast_nodes_carry_their_source_and_target_types() {
    let mut module = Module::new().unwrap();

    // `CAST(id AS STRING)` converts the INT64 column `id` to STRING, producing a
    // ResolvedCast whose node reports both ends of the conversion.
    let root = module
        .resolved_tree("SELECT CAST(id AS STRING) FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let cast =
        find_kind(&root, "ResolvedCast").expect("`CAST(id AS STRING)` produces a ResolvedCast");
    let info = cast
        .cast()
        .expect("a ResolvedCast node exposes its cast type information");

    assert_eq!(info.from_type(), "INT64");
    assert_eq!(info.to_type(), "STRING");
}

#[test]
fn non_cast_nodes_have_no_cast_info() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a cast.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.cast().is_none());
}

#[test]
fn parameter_nodes_carry_their_name() {
    let mut module = Module::new().unwrap();

    // A named query parameter `@p` resolves to a ResolvedParameter that reports
    // its name. Its type is inferred from the `+ 1` context (no declaration).
    let root = module
        .resolved_tree("SELECT @p + 1 AS x", &[])
        .unwrap()
        .unwrap();

    let parameter =
        find_kind(&root, "ResolvedParameter").expect("`@p` produces a ResolvedParameter");
    assert_eq!(parameter.parameter_name(), Some("p"));
}

#[test]
fn non_parameter_nodes_have_no_name() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a parameter reference.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.parameter_name().is_none());
}

#[test]
fn aggregate_function_calls_are_flagged_and_named() {
    let mut module = Module::new().unwrap();

    // `COUNT(id)` is an aggregate function call, a different node kind than a
    // scalar call, but it still exposes the catalog function name it invokes.
    let root = module
        .resolved_tree("SELECT COUNT(id) FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let call = find_kind(&root, "ResolvedAggregateFunctionCall")
        .expect("`COUNT(id)` produces a ResolvedAggregateFunctionCall");
    assert!(call.is_aggregate());
    assert_eq!(call.function_name(), Some("count"));
}

#[test]
fn scalar_function_calls_are_not_aggregate() {
    let mut module = Module::new().unwrap();

    // `id + 1` is a scalar function call, so it is named but not an aggregate.
    let root = module
        .resolved_tree("SELECT id + 1 AS x FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let call =
        find_kind(&root, "ResolvedFunctionCall").expect("`id + 1` produces a ResolvedFunctionCall");
    assert!(!call.is_aggregate());
    assert_eq!(call.function_name(), Some("$add"));
}

#[test]
fn non_function_nodes_are_not_aggregate() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a function call at all.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(!root.is_aggregate());
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
