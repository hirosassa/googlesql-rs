//! End-to-end tests for the resolved AST tree (the analyzer's typed output tree).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "test code"
)]

use googlesql::{
    ColumnDef, ColumnType, Error, JoinType, LiteralValue, Module, ResolvedNode, SetOperation,
    SubqueryKind, TableDef,
};

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

fn orders_table() -> TableDef {
    TableDef {
        name: "orders".to_string(),
        columns: vec![ColumnDef {
            name: "user_id".to_string(),
            ty: ColumnType::Int64,
        }],
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

/// Collects the `is_correlated()` flag of every column reference in the tree.
fn collect_correlated_flags(node: &ResolvedNode, out: &mut Vec<bool>) {
    if node.kind() == "ResolvedColumnRef" {
        out.push(
            node.is_correlated()
                .expect("a ResolvedColumnRef reports its correlation"),
        );
    }
    for child in node.children() {
        collect_correlated_flags(child, out);
    }
}

#[test]
fn correlated_subquery_reference_is_flagged() {
    let mut module = Module::new().unwrap();

    // The inner subquery references `users.id` from the enclosing query, so that
    // reference is correlated; the same column read by the outer query is not.
    let root = module
        .resolved_tree(
            "SELECT (SELECT COUNT(*) FROM orders WHERE orders.user_id = users.id) FROM users",
            &[users_table(), orders_table()],
        )
        .unwrap()
        .unwrap();

    let mut flags = Vec::new();
    collect_correlated_flags(&root, &mut flags);

    assert!(
        flags.contains(&true),
        "the correlated reference to the outer `users.id` should be flagged"
    );
    assert!(
        flags.contains(&false),
        "a reference within its own query should not be flagged"
    );
}

#[test]
fn plain_column_reference_is_not_correlated() {
    let mut module = Module::new().unwrap();

    // A reference within its own query is never correlated. proto3 omits that
    // false value on the wire; it must still decode as false, not an error.
    let root = module
        .resolved_tree("SELECT id + 1 FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let column_ref =
        find_kind(&root, "ResolvedColumnRef").expect("`id + 1` produces a ResolvedColumnRef");
    assert_eq!(column_ref.is_correlated(), Some(false));
}

#[test]
fn non_column_ref_nodes_have_no_correlation() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a column reference.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.is_correlated(), None);
}

#[test]
fn null_literal_carries_the_null_value() {
    let mut module = Module::new().unwrap();

    // A bare NULL resolves to a ResolvedLiteral holding a NULL value. Reading a
    // typed value out of it would trap in the wasm module, so it must decode as
    // the NULL variant instead.
    let root = module.resolved_tree("SELECT NULL", &[]).unwrap().unwrap();

    let literal = find_kind(&root, "ResolvedLiteral")
        .expect("a NULL constant in the SELECT list produces a ResolvedLiteral");
    assert_eq!(literal.literal_value(), Some(&LiteralValue::Null));
}

#[test]
fn typed_null_literal_keeps_its_type_and_null_value() {
    let mut module = Module::new().unwrap();

    // `CAST(NULL AS INT64)` is a typed NULL: its value is NULL, but the node
    // still reports its resolved type.
    let root = module
        .resolved_tree("SELECT CAST(NULL AS INT64)", &[])
        .unwrap()
        .unwrap();

    let literal = find_kind(&root, "ResolvedLiteral")
        .expect("a typed NULL constant produces a ResolvedLiteral");
    assert_eq!(literal.literal_value(), Some(&LiteralValue::Null));
    assert_eq!(literal.type_name(), Some("INT64"));
}

#[test]
fn aggregate_scan_reports_its_aggregate_column() {
    let mut module = Module::new().unwrap();

    // `COUNT(*)` becomes one aggregate output column on the ResolvedAggregateScan,
    // separate from the `name` grouping key.
    let root = module
        .resolved_tree("SELECT COUNT(*) FROM users GROUP BY name", &[users_table()])
        .unwrap()
        .unwrap();

    let scan = find_kind(&root, "ResolvedAggregateScan")
        .expect("a GROUP BY produces a ResolvedAggregateScan");
    let aggregates = scan
        .aggregate_columns()
        .expect("a ResolvedAggregateScan reports its aggregate columns");

    assert_eq!(aggregates.len(), 1);
    assert!(
        !aggregates[0].is_empty(),
        "each aggregate output column has a (synthetic) name"
    );
}

#[test]
fn each_aggregate_gets_its_own_column_distinct_from_grouping() {
    let mut module = Module::new().unwrap();

    // Two aggregates yield two aggregate columns; the grouping key stays in the
    // separate group-by list, so the two lists must not be conflated.
    let root = module
        .resolved_tree(
            "SELECT name, COUNT(*), SUM(id) FROM users GROUP BY name",
            &[users_table()],
        )
        .unwrap()
        .unwrap();

    let scan = find_kind(&root, "ResolvedAggregateScan")
        .expect("a GROUP BY produces a ResolvedAggregateScan");

    assert_eq!(scan.aggregate_columns().map(<[String]>::len), Some(2));
    assert_eq!(
        scan.group_by_columns(),
        Some(["name".to_string()].as_slice())
    );
}

#[test]
fn non_aggregate_scan_has_no_aggregate_columns() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not an aggregate scan.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.aggregate_columns(), None);
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
fn bytes_literal_carries_its_value() {
    let mut module = Module::new().unwrap();

    // A bytes constant resolves to a ResolvedLiteral holding raw BYTES.
    let root = module.resolved_tree("SELECT b'abc'", &[]).unwrap().unwrap();

    let literal =
        find_kind(&root, "ResolvedLiteral").expect("a bytes constant produces a ResolvedLiteral");
    assert_eq!(
        literal.literal_value(),
        Some(&LiteralValue::Bytes(b"abc".to_vec()))
    );
}

#[test]
fn date_literal_carries_its_day_number() {
    let mut module = Module::new().unwrap();

    // A DATE constant resolves to a ResolvedLiteral holding the day count since
    // the 1970-01-01 epoch, so the day after the epoch is day 1.
    let root = module
        .resolved_tree("SELECT DATE '1970-01-02'", &[])
        .unwrap()
        .unwrap();

    let literal =
        find_kind(&root, "ResolvedLiteral").expect("a DATE constant produces a ResolvedLiteral");
    assert_eq!(literal.literal_value(), Some(&LiteralValue::Date(1)));
}

#[test]
fn timestamp_literal_carries_its_unix_micros() {
    let mut module = Module::new().unwrap();

    // A TIMESTAMP constant resolves to a ResolvedLiteral holding the count of
    // microseconds since the 1970-01-01 epoch, so one second past the epoch is
    // 1_000_000 microseconds. The offset is fixed to UTC to keep the value
    // independent of the analyzer's default time zone.
    let cases: [(&str, i64); 2] = [
        ("SELECT TIMESTAMP '1970-01-01 00:00:00+00'", 0),
        ("SELECT TIMESTAMP '1970-01-01 00:00:01+00'", 1_000_000),
    ];

    for (sql, micros) in cases {
        let root = module.resolved_tree(sql, &[]).unwrap().unwrap();
        let literal = find_kind(&root, "ResolvedLiteral")
            .unwrap_or_else(|| panic!("{sql} should produce a ResolvedLiteral"));
        assert_eq!(
            literal.literal_value(),
            Some(&LiteralValue::Timestamp(micros)),
            "for {sql}"
        );
    }
}

#[test]
fn zero_value_literals_decode_as_zero() {
    let mut module = Module::new().unwrap();

    // A proto3 scalar equal to its zero default is omitted on the wire, so a
    // zero-valued literal must still decode to its zero variant, not vanish or
    // error. This covers every value type that reads a numeric or bytes field.
    let cases: [(&str, LiteralValue); 5] = [
        ("SELECT 0", LiteralValue::Int64(0)),
        ("SELECT 0.0", LiteralValue::Double(0.0)),
        ("SELECT DATE '1970-01-01'", LiteralValue::Date(0)),
        ("SELECT b''", LiteralValue::Bytes(Vec::new())),
        (
            "SELECT TIMESTAMP '1970-01-01 00:00:00+00'",
            LiteralValue::Timestamp(0),
        ),
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
fn safe_cast_is_flagged() {
    let mut module = Module::new().unwrap();

    // `SAFE_CAST` returns NULL instead of erroring on a failed conversion, which
    // the resolver records on the ResolvedCast as return-null-on-error.
    let root = module
        .resolved_tree(
            "SELECT SAFE_CAST(id AS STRING) FROM users",
            &[users_table()],
        )
        .unwrap()
        .unwrap();

    let cast = find_kind(&root, "ResolvedCast")
        .expect("`SAFE_CAST(id AS STRING)` produces a ResolvedCast");
    let info = cast
        .cast()
        .expect("a ResolvedCast node exposes its cast type information");

    assert!(info.is_safe());
}

#[test]
fn plain_cast_is_not_safe() {
    let mut module = Module::new().unwrap();

    // A plain `CAST` errors on a failed conversion, so return-null-on-error is
    // false. proto3 omits that false value on the wire; it must still decode as
    // false, not an error.
    let root = module
        .resolved_tree("SELECT CAST(id AS STRING) FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let cast =
        find_kind(&root, "ResolvedCast").expect("`CAST(id AS STRING)` produces a ResolvedCast");
    let info = cast
        .cast()
        .expect("a ResolvedCast node exposes its cast type information");

    assert!(!info.is_safe());
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
fn scalar_function_calls_report_their_argument_count() {
    let mut module = Module::new().unwrap();

    // `id + 1` is a binary scalar call, so `$add` takes two value arguments.
    let root = module
        .resolved_tree("SELECT id + 1 AS x FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let call =
        find_kind(&root, "ResolvedFunctionCall").expect("`id + 1` produces a ResolvedFunctionCall");
    assert_eq!(call.argument_count(), Some(2));
}

#[test]
fn unary_function_calls_report_a_single_argument() {
    let mut module = Module::new().unwrap();

    // `lower(name)` takes exactly one value argument.
    let root = module
        .resolved_tree("SELECT lower(name) AS n FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let call = find_kind(&root, "ResolvedFunctionCall")
        .expect("`lower(name)` produces a ResolvedFunctionCall");
    assert_eq!(call.argument_count(), Some(1));
}

#[test]
fn aggregate_function_calls_report_their_argument_count() {
    let mut module = Module::new().unwrap();

    // `COUNT(id)` takes one value argument; the count reflects value arguments,
    // not the node's children (an aggregate may also carry modifier children).
    let root = module
        .resolved_tree("SELECT COUNT(id) FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let call = find_kind(&root, "ResolvedAggregateFunctionCall")
        .expect("`COUNT(id)` produces a ResolvedAggregateFunctionCall");
    assert_eq!(call.argument_count(), Some(1));
}

#[test]
fn non_function_nodes_have_no_argument_count() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a function call.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.argument_count().is_none());
}

#[test]
fn table_scan_nodes_list_their_columns() {
    let mut module = Module::new().unwrap();

    // The scan over `users` exposes the columns it produces. `resolved_tree` does
    // not prune, so both table columns appear regardless of what the query reads.
    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let scan =
        find_kind(&root, "ResolvedTableScan").expect("`FROM users` produces a ResolvedTableScan");
    let columns = scan
        .scan_columns()
        .expect("a ResolvedTableScan node lists its columns");

    assert!(
        columns.contains(&"id".to_string()),
        "expected the scan to list `id`, got {columns:?}"
    );
    assert!(
        columns.contains(&"name".to_string()),
        "expected the scan to list `name`, got {columns:?}"
    );
}

#[test]
fn non_table_scan_nodes_have_no_scan_columns() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a table scan.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.scan_columns().is_none());
}

#[test]
fn table_scan_nodes_report_their_column_indexes() {
    let mut module = Module::new().unwrap();

    // Each scanned column maps to its ordinal position in the base table. `users`
    // declares `id` at 0 and `name` at 1, and `resolved_tree` does not prune, so
    // the scan lists both indexes in table order.
    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let scan =
        find_kind(&root, "ResolvedTableScan").expect("`FROM users` produces a ResolvedTableScan");
    assert_eq!(scan.column_index_list(), Some(&[0, 1][..]));

    // The index list is positionally aligned with the scanned-column names.
    let columns = scan
        .scan_columns()
        .expect("a ResolvedTableScan node lists its columns");
    assert_eq!(
        scan.column_index_list().map(<[i32]>::len),
        Some(columns.len())
    );
}

#[test]
fn non_table_scan_nodes_have_no_column_index_list() {
    let mut module = Module::new().unwrap();

    // A literal query has no table scan anywhere in its tree.
    let root = module.resolved_tree("SELECT 1 AS x", &[]).unwrap().unwrap();

    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.column_index_list().is_none());
}

#[test]
fn aggregate_distinct_calls_report_distinct() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT COUNT(DISTINCT id) FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let call = find_kind(&root, "ResolvedAggregateFunctionCall")
        .expect("`COUNT(DISTINCT id)` produces a ResolvedAggregateFunctionCall");
    assert_eq!(call.distinct(), Some(true));
}

#[test]
fn aggregate_non_distinct_calls_report_not_distinct() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT COUNT(id) FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let call = find_kind(&root, "ResolvedAggregateFunctionCall")
        .expect("`COUNT(id)` produces a ResolvedAggregateFunctionCall");
    assert_eq!(call.distinct(), Some(false));
}

#[test]
fn scalar_function_calls_have_no_distinct_flag() {
    let mut module = Module::new().unwrap();

    // DISTINCT applies only to aggregate calls; a scalar call carries no flag.
    let root = module
        .resolved_tree("SELECT id + 1 FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let call =
        find_kind(&root, "ResolvedFunctionCall").expect("`id + 1` produces a ResolvedFunctionCall");
    assert!(call.distinct().is_none());
}

#[test]
fn non_function_nodes_have_no_distinct_flag() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a function call.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.distinct().is_none());
}

/// Collects the parse-location text of every node that carries one.
fn collect_parse_texts<'a>(node: &ResolvedNode, sql: &'a str, out: &mut Vec<&'a str>) {
    if let Some(range) = node.parse_location() {
        out.push(&sql[range]);
    }
    for child in node.children() {
        collect_parse_texts(child, sql, out);
    }
}

#[test]
fn resolved_nodes_carry_their_parse_location() {
    let mut module = Module::new().unwrap();

    let sql = "SELECT id FROM users";
    let root = module
        .resolved_tree(sql, &[users_table()])
        .unwrap()
        .unwrap();

    let mut texts = Vec::new();
    collect_parse_texts(&root, sql, &mut texts);

    assert!(
        !texts.is_empty(),
        "at least one resolved node should carry a parse location"
    );
    // The table scan spans its source table name.
    assert!(
        texts.contains(&"users"),
        "expected a node whose parse location spans `users`, got {texts:?}"
    );
}

#[test]
fn parse_location_is_none_without_a_recorded_range() {
    let mut module = Module::new().unwrap();

    // A literal-only query still resolves; every reported range must be a valid
    // slice of the source, and nodes without a recorded location report `None`.
    let sql = "SELECT 42";
    let root = module.resolved_tree(sql, &[]).unwrap().unwrap();

    for_each_node(&root, &mut |node| {
        if let Some(range) = node.parse_location() {
            assert!(range.start <= range.end, "range must be well-formed");
            assert!(range.end <= sql.len(), "range must stay within the source");
        }
    });
}

/// Applies `f` to `node` and every descendant.
fn for_each_node(node: &ResolvedNode, f: &mut impl FnMut(&ResolvedNode)) {
    f(node);
    for child in node.children() {
        for_each_node(child, f);
    }
}

#[test]
fn inner_join_scan_reports_inner() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree(
            "SELECT users.id FROM users JOIN orders ON users.id = orders.user_id",
            &[users_table(), orders_table()],
        )
        .unwrap()
        .unwrap();

    let join = find_kind(&root, "ResolvedJoinScan").expect("a JOIN produces a ResolvedJoinScan");
    assert_eq!(join.join_type(), Some(JoinType::Inner));
}

#[test]
fn left_join_scan_reports_left() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree(
            "SELECT users.id FROM users LEFT JOIN orders ON users.id = orders.user_id",
            &[users_table(), orders_table()],
        )
        .unwrap()
        .unwrap();

    let join =
        find_kind(&root, "ResolvedJoinScan").expect("a LEFT JOIN produces a ResolvedJoinScan");
    assert_eq!(join.join_type(), Some(JoinType::Left));
}

#[test]
fn right_join_scan_reports_right() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree(
            "SELECT users.id FROM users RIGHT JOIN orders ON users.id = orders.user_id",
            &[users_table(), orders_table()],
        )
        .unwrap()
        .unwrap();

    let join =
        find_kind(&root, "ResolvedJoinScan").expect("a RIGHT JOIN produces a ResolvedJoinScan");
    assert_eq!(join.join_type(), Some(JoinType::Right));
}

#[test]
fn full_join_scan_reports_full() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree(
            "SELECT users.id FROM users FULL JOIN orders ON users.id = orders.user_id",
            &[users_table(), orders_table()],
        )
        .unwrap()
        .unwrap();

    let join =
        find_kind(&root, "ResolvedJoinScan").expect("a FULL JOIN produces a ResolvedJoinScan");
    assert_eq!(join.join_type(), Some(JoinType::Full));
}

#[test]
fn non_join_nodes_have_no_join_type() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a join.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.join_type().is_none());
}

#[test]
fn descending_order_by_item_reports_descending() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users ORDER BY id DESC", &[users_table()])
        .unwrap()
        .unwrap();

    let item = find_kind(&root, "ResolvedOrderByItem")
        .expect("an ORDER BY produces a ResolvedOrderByItem");
    assert_eq!(item.is_descending(), Some(true));
}

#[test]
fn ascending_order_by_item_reports_not_descending() {
    let mut module = Module::new().unwrap();

    // Ascending is the default, so an item written without ASC/DESC still
    // reports `Some(false)`.
    let root = module
        .resolved_tree("SELECT id FROM users ORDER BY id", &[users_table()])
        .unwrap()
        .unwrap();

    let item = find_kind(&root, "ResolvedOrderByItem")
        .expect("an ORDER BY produces a ResolvedOrderByItem");
    assert_eq!(item.is_descending(), Some(false));
}

#[test]
fn non_order_by_nodes_have_no_direction() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not an ORDER BY item.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.is_descending().is_none());
}

/// Resolves `<SELECT id FROM users> <op> <SELECT id FROM users>` and returns
/// the set operation reported by the resulting `ResolvedSetOperationScan`.
fn set_operation_of(op: &str) -> SetOperation {
    let mut module = Module::new().unwrap();
    let sql = format!("SELECT id FROM users {op} SELECT id FROM users");
    let root = module
        .resolved_tree(&sql, &[users_table()])
        .unwrap()
        .unwrap();
    find_kind(&root, "ResolvedSetOperationScan")
        .expect("a set operation produces a ResolvedSetOperationScan")
        .set_operation()
        .expect("a ResolvedSetOperationScan node exposes its operation")
}

#[test]
fn set_operation_scans_report_their_operation() {
    assert_eq!(set_operation_of("UNION ALL"), SetOperation::UnionAll);
    assert_eq!(
        set_operation_of("UNION DISTINCT"),
        SetOperation::UnionDistinct
    );
    assert_eq!(
        set_operation_of("INTERSECT ALL"),
        SetOperation::IntersectAll
    );
    assert_eq!(
        set_operation_of("INTERSECT DISTINCT"),
        SetOperation::IntersectDistinct
    );
    assert_eq!(set_operation_of("EXCEPT ALL"), SetOperation::ExceptAll);
    assert_eq!(
        set_operation_of("EXCEPT DISTINCT"),
        SetOperation::ExceptDistinct
    );
}

#[test]
fn non_set_operation_nodes_have_no_operation() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a set operation.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.set_operation().is_none());
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

#[test]
fn aggregate_scan_lists_its_group_by_columns() {
    let mut module = Module::new().unwrap();

    // `GROUP BY name` produces a ResolvedAggregateScan whose group-by list holds
    // one computed column named after the grouping key.
    let root = module
        .resolved_tree(
            "SELECT name, COUNT(id) FROM users GROUP BY name",
            &[users_table()],
        )
        .unwrap()
        .unwrap();

    let scan = find_kind(&root, "ResolvedAggregateScan")
        .expect("`GROUP BY` produces a ResolvedAggregateScan");
    let columns = scan
        .group_by_columns()
        .expect("a ResolvedAggregateScan node lists its group-by columns");
    assert_eq!(columns, ["name"]);
}

#[test]
fn multi_key_group_by_lists_every_column() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree(
            "SELECT id, name FROM users GROUP BY id, name",
            &[users_table()],
        )
        .unwrap()
        .unwrap();

    let scan = find_kind(&root, "ResolvedAggregateScan")
        .expect("`GROUP BY` produces a ResolvedAggregateScan");
    let columns = scan
        .group_by_columns()
        .expect("a ResolvedAggregateScan node lists its group-by columns");
    assert!(columns.contains(&"id".to_string()), "got {columns:?}");
    assert!(columns.contains(&"name".to_string()), "got {columns:?}");
}

#[test]
fn non_aggregate_nodes_have_no_group_by_columns() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not an aggregate scan.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.group_by_columns().is_none());
}

#[test]
fn limit_offset_scan_reports_both_values() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users LIMIT 10 OFFSET 5", &[users_table()])
        .unwrap()
        .unwrap();

    let scan = find_kind(&root, "ResolvedLimitOffsetScan")
        .expect("`LIMIT ... OFFSET` produces a ResolvedLimitOffsetScan");
    let limit_offset = scan
        .limit_offset()
        .expect("a ResolvedLimitOffsetScan node exposes its limit and offset");
    assert_eq!(limit_offset.limit(), Some(10));
    assert_eq!(limit_offset.offset(), Some(5));
}

#[test]
fn limit_without_offset_reports_no_offset() {
    let mut module = Module::new().unwrap();

    // A bare `LIMIT` records no OFFSET expression, so the offset is absent.
    let root = module
        .resolved_tree("SELECT id FROM users LIMIT 3", &[users_table()])
        .unwrap()
        .unwrap();

    let scan = find_kind(&root, "ResolvedLimitOffsetScan")
        .expect("`LIMIT` produces a ResolvedLimitOffsetScan");
    let limit_offset = scan.limit_offset().expect("a limit/offset scan");
    assert_eq!(limit_offset.limit(), Some(3));
    assert_eq!(limit_offset.offset(), None);
}

#[test]
fn parameterized_limit_reports_no_literal_value() {
    let mut module = Module::new().unwrap();

    // `LIMIT @n` is a parameter, not a literal, so no concrete value is exposed
    // even though the node is still a ResolvedLimitOffsetScan.
    let root = module
        .resolved_tree("SELECT id FROM users LIMIT @n", &[users_table()])
        .unwrap()
        .unwrap();

    let scan = find_kind(&root, "ResolvedLimitOffsetScan")
        .expect("`LIMIT @n` produces a ResolvedLimitOffsetScan");
    let limit_offset = scan.limit_offset().expect("a limit/offset scan");
    assert_eq!(limit_offset.limit(), None);
    assert_eq!(limit_offset.offset(), None);
}

#[test]
fn non_limit_offset_nodes_have_no_limit_offset() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a limit/offset scan.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.limit_offset().is_none());
}

/// Resolves `sql` and returns the kind reported by the first
/// `ResolvedSubqueryExpr` in the tree.
fn subquery_kind_of(sql: &str) -> SubqueryKind {
    let mut module = Module::new().unwrap();
    let root = module
        .resolved_tree(sql, &[users_table(), orders_table()])
        .unwrap()
        .unwrap();
    find_kind(&root, "ResolvedSubqueryExpr")
        .expect("the query contains a subquery expression")
        .subquery_kind()
        .expect("a ResolvedSubqueryExpr node exposes its kind")
}

#[test]
fn scalar_subquery_reports_scalar() {
    assert_eq!(
        subquery_kind_of("SELECT (SELECT id FROM users LIMIT 1) AS x FROM users"),
        SubqueryKind::Scalar
    );
}

#[test]
fn array_subquery_reports_array() {
    assert_eq!(
        subquery_kind_of("SELECT ARRAY(SELECT user_id FROM orders) AS a FROM users"),
        SubqueryKind::Array
    );
}

#[test]
fn exists_subquery_reports_exists() {
    assert_eq!(
        subquery_kind_of("SELECT id FROM users WHERE EXISTS(SELECT 1 FROM orders)"),
        SubqueryKind::Exists
    );
}

#[test]
fn in_subquery_reports_in() {
    assert_eq!(
        subquery_kind_of("SELECT id FROM users WHERE id IN (SELECT user_id FROM orders)"),
        SubqueryKind::In
    );
}

#[test]
fn non_subquery_nodes_have_no_subquery_kind() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a subquery expression.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.subquery_kind().is_none());
}

#[test]
fn with_entry_reports_its_cte_name() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree(
            "WITH t AS (SELECT id FROM users) SELECT id FROM t",
            &[users_table()],
        )
        .unwrap()
        .unwrap();

    let entry = find_kind(&root, "ResolvedWithEntry")
        .expect("a WITH query should produce a ResolvedWithEntry");
    assert_eq!(entry.with_query_name(), Some("t"));
}

#[test]
fn with_ref_scan_reports_referenced_cte_name() {
    let mut module = Module::new().unwrap();

    // `FROM t` inside a WITH query resolves to a ResolvedWithRefScan that reads
    // the CTE named `t`; its CTE name matches the entry that defines it.
    let root = module
        .resolved_tree(
            "WITH t AS (SELECT id FROM users) SELECT id FROM t",
            &[users_table()],
        )
        .unwrap()
        .unwrap();

    let ref_scan = find_kind(&root, "ResolvedWithRefScan")
        .expect("a reference to a CTE should produce a ResolvedWithRefScan");
    assert_eq!(ref_scan.with_query_name(), Some("t"));
}

#[test]
fn multiple_ctes_report_each_name() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree(
            "WITH a AS (SELECT id FROM users), b AS (SELECT id FROM a) SELECT id FROM b",
            &[users_table()],
        )
        .unwrap()
        .unwrap();

    // Collect the names the WITH entries define; a WithRefScan also carries a
    // CTE name (the one it reads), so restrict to the defining nodes here.
    let mut names = Vec::new();
    for_each_node(&root, &mut |node| {
        if node.kind() == "ResolvedWithEntry"
            && let Some(name) = node.with_query_name()
        {
            names.push(name.to_owned());
        }
    });

    assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn non_with_entry_nodes_have_no_cte_name() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // A query with no WITH clause carries no CTE names anywhere in its tree.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert!(root.with_query_name().is_none());
}

#[test]
fn table_scan_reports_its_alias() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT u.id FROM users AS u", &[users_table()])
        .unwrap()
        .unwrap();

    let scan = find_kind(&root, "ResolvedTableScan").expect("the query scans a table");
    assert_eq!(scan.alias(), Some("u"));
}

#[test]
fn table_scan_without_an_alias_reports_an_empty_alias() {
    let mut module = Module::new().unwrap();

    // No explicit alias: proto3 omits the empty string, which must decode as an
    // empty alias rather than an error, and never as `None` (this is a scan).
    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let scan = find_kind(&root, "ResolvedTableScan").expect("the query scans a table");
    assert_eq!(scan.alias(), Some(""));
}

#[test]
fn non_table_scan_nodes_have_no_alias() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a table scan.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.alias(), None);
}

#[test]
fn join_with_using_is_flagged() {
    let mut module = Module::new().unwrap();

    // `events` shares the `id` column name with `users`, so USING(id) is legal.
    let events = TableDef {
        name: "events".to_string(),
        columns: vec![ColumnDef {
            name: "id".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    let root = module
        .resolved_tree(
            "SELECT id FROM users JOIN events USING (id)",
            &[users_table(), events],
        )
        .unwrap()
        .unwrap();

    let join = find_kind(&root, "ResolvedJoinScan").expect("the query joins two tables");
    assert_eq!(join.has_using(), Some(true));
}

#[test]
fn join_with_on_condition_has_no_using() {
    let mut module = Module::new().unwrap();

    // An ON join uses no USING clause; proto3 omits that false flag, which must
    // decode as `Some(false)`, not `None` and not an error.
    let root = module
        .resolved_tree(
            "SELECT users.id FROM users JOIN orders ON users.id = orders.user_id",
            &[users_table(), orders_table()],
        )
        .unwrap()
        .unwrap();

    let join = find_kind(&root, "ResolvedJoinScan").expect("the query joins two tables");
    assert_eq!(join.has_using(), Some(false));
}

#[test]
fn non_join_scan_nodes_have_no_using_flag() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // A query with no join carries no USING flag anywhere in its tree.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.has_using(), None);
}

#[test]
fn cast_literal_has_an_explicit_type() {
    let mut module = Module::new().unwrap();

    // `CAST(NULL AS INT64)` folds to a literal whose type was stated explicitly.
    let root = module
        .resolved_tree("SELECT CAST(NULL AS INT64)", &[users_table()])
        .unwrap()
        .unwrap();

    let literal = find_kind(&root, "ResolvedLiteral").expect("the cast folds to a literal");
    assert_eq!(literal.has_explicit_type(), Some(true));
}

#[test]
fn inferred_literal_has_no_explicit_type() {
    let mut module = Module::new().unwrap();

    // A bare `1` has its INT64 type inferred, not stated; proto3 omits that false
    // flag, which must decode as `Some(false)`, not `None` and not an error.
    let root = module
        .resolved_tree("SELECT 1", &[users_table()])
        .unwrap()
        .unwrap();

    let literal = find_kind(&root, "ResolvedLiteral").expect("`1` is a literal");
    assert_eq!(literal.has_explicit_type(), Some(false));
}

#[test]
fn non_literal_nodes_have_no_explicit_type_flag() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a literal.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.has_explicit_type(), None);
}

#[test]
fn value_table_query_is_flagged() {
    let mut module = Module::new().unwrap();

    // `SELECT AS VALUE` makes each row a single unnamed value, a value table.
    let root = module
        .resolved_tree("SELECT AS VALUE id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.is_value_table(), Some(true));
}

#[test]
fn ordinary_query_is_not_a_value_table() {
    let mut module = Module::new().unwrap();

    // A plain SELECT produces named-column rows; proto3 omits that false flag,
    // which must decode as `Some(false)`, not `None` and not an error.
    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.is_value_table(), Some(false));
}

#[test]
fn non_query_stmt_nodes_have_no_value_table_flag() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // A node below the statement root is not a query statement.
    let scan = find_kind(&root, "ResolvedTableScan").expect("the query scans a table");
    assert_eq!(scan.is_value_table(), None);
}

fn collect_column_ids(node: &ResolvedNode, out: &mut Vec<(String, i32)>) {
    if let Some(reference) = node.column_ref() {
        out.push((reference.name().to_string(), reference.id()));
    }
    for child in node.children() {
        collect_column_ids(child, out);
    }
}

#[test]
fn column_ref_carries_a_positive_column_id() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id + 1 FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let column_ref = find_kind(&root, "ResolvedColumnRef")
        .and_then(ResolvedNode::column_ref)
        .expect("`id + 1` reads a column");

    // ZetaSQL assigns every resolved column a unique positive id.
    assert!(column_ref.id() > 0);
}

#[test]
fn same_named_columns_from_distinct_scans_have_distinct_ids() {
    let mut module = Module::new().unwrap();

    // Two scans of `users` each produce their own `id` column; the shared name
    // does not make them the same column, and their column ids prove it.
    let root = module
        .resolved_tree(
            "SELECT a.id FROM users AS a JOIN users AS b ON a.id = b.id",
            &[users_table()],
        )
        .unwrap()
        .unwrap();

    let mut ids = Vec::new();
    collect_column_ids(&root, &mut ids);
    let id_column_ids: Vec<i32> = ids
        .into_iter()
        .filter(|(name, _)| name == "id")
        .map(|(_, id)| id)
        .collect();

    assert!(
        id_column_ids.len() >= 2,
        "both scans' `id` columns should be referenced"
    );
    assert!(
        id_column_ids.iter().any(|id| *id != id_column_ids[0]),
        "same-named columns from different scans should have distinct ids"
    );
}

#[test]
fn get_struct_field_reports_its_field_index() {
    let mut module = Module::new().unwrap();

    // `.b` reads the second field of the struct, at zero-based index 1.
    let root = module
        .resolved_tree("SELECT STRUCT(1 AS a, 2 AS b).b", &[users_table()])
        .unwrap()
        .unwrap();

    let field = find_kind(&root, "ResolvedGetStructField").expect("`.b` reads a struct field");
    assert_eq!(field.struct_field_index(), Some(1));
}

#[test]
fn get_struct_field_reports_a_zero_first_field_index() {
    let mut module = Module::new().unwrap();

    // `.a` reads the first field, index 0; proto3 omits that zero, which must
    // still decode as `Some(0)`, not `None` and not an error.
    let root = module
        .resolved_tree("SELECT STRUCT(1 AS a, 2 AS b).a", &[users_table()])
        .unwrap()
        .unwrap();

    let field = find_kind(&root, "ResolvedGetStructField").expect("`.a` reads a struct field");
    assert_eq!(field.struct_field_index(), Some(0));
}

#[test]
fn non_struct_field_nodes_have_no_field_index() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root does not read a struct field.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.struct_field_index(), None);
}

#[test]
fn array_scan_reports_its_element_column_name() {
    let mut module = Module::new().unwrap();

    // UNNEST binds each array element to the aliased column `x`.
    let root = module
        .resolved_tree("SELECT x FROM UNNEST([1, 2, 3]) AS x", &[users_table()])
        .unwrap()
        .unwrap();

    let scan = find_kind(&root, "ResolvedArrayScan").expect("UNNEST produces an array scan");
    assert_eq!(scan.array_element_name(), Some("x"));
}

#[test]
fn non_array_scan_nodes_have_no_element_column_name() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // A query with no UNNEST carries no element column anywhere in its tree.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.array_element_name(), None);
}

#[test]
fn array_scan_reports_outer_for_left_join_unnest() {
    let mut module = Module::new().unwrap();

    // A LEFT JOIN against UNNEST keeps input rows whose array is empty, so the
    // array scan is marked outer.
    let root = module
        .resolved_tree(
            "SELECT id, x FROM users LEFT JOIN UNNEST([1, 2, 3]) AS x",
            &[users_table()],
        )
        .unwrap()
        .unwrap();

    let scan = find_kind(&root, "ResolvedArrayScan").expect("UNNEST produces an array scan");
    assert_eq!(scan.is_outer(), Some(true));
}

#[test]
fn array_scan_reports_inner_for_plain_unnest() {
    let mut module = Module::new().unwrap();

    // A bare FROM UNNEST is an inner scan: it drops input rows with empty arrays.
    let root = module
        .resolved_tree("SELECT x FROM UNNEST([1, 2, 3]) AS x", &[users_table()])
        .unwrap()
        .unwrap();

    let scan = find_kind(&root, "ResolvedArrayScan").expect("UNNEST produces an array scan");
    assert_eq!(scan.is_outer(), Some(false));
}

#[test]
fn non_array_scan_nodes_have_no_outer_flag() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // A query with no UNNEST carries no outer flag anywhere in its tree.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.is_outer(), None);
}

#[test]
fn project_scan_reports_its_projected_columns() {
    let mut module = Module::new().unwrap();

    // The projection computes a new column `n`, which appears in its column list.
    let root = module
        .resolved_tree("SELECT id + 1 AS n FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let project =
        find_kind(&root, "ResolvedProjectScan").expect("the query projects an expression");
    let columns = project
        .project_columns()
        .expect("a project scan lists its columns");
    assert!(columns.contains(&"n".to_string()));
}

#[test]
fn non_project_scan_nodes_have_no_projected_columns() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root is not a project scan.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.project_columns(), None);
}

#[test]
fn computed_column_reports_the_column_it_defines() {
    let mut module = Module::new().unwrap();

    // `id + 1 AS n` becomes a ResolvedComputedColumn defining the column `n`.
    let root = module
        .resolved_tree("SELECT id + 1 AS n FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    let computed =
        find_kind(&root, "ResolvedComputedColumn").expect("the projection computes a column");
    assert_eq!(computed.computed_column_name(), Some("n"));
}

#[test]
fn non_computed_column_nodes_have_no_computed_column_name() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root does not define a computed column.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.computed_column_name(), None);
}

#[test]
fn insert_statement_roots_the_tree_and_names_its_target_table() {
    let mut module = Module::new().unwrap();

    // An INSERT resolves to a ResolvedInsertStmt whose first child scans the
    // target table.
    let root = module
        .resolved_tree("INSERT INTO users (id) VALUES (1)", &[users_table()])
        .unwrap()
        .unwrap();

    assert_eq!(root.kind(), "ResolvedInsertStmt");
    let scan = find_kind(&root, "ResolvedTableScan").expect("an INSERT scans its target table");
    assert_eq!(scan.table_name(), Some("users"));
}

#[test]
fn update_statement_roots_the_tree_and_names_its_target_table() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("UPDATE users SET name = 'x' WHERE id = 1", &[users_table()])
        .unwrap()
        .unwrap();

    assert_eq!(root.kind(), "ResolvedUpdateStmt");
    let scan = find_kind(&root, "ResolvedTableScan").expect("an UPDATE scans its target table");
    assert_eq!(scan.table_name(), Some("users"));
}

#[test]
fn delete_statement_roots_the_tree_and_names_its_target_table() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("DELETE FROM users WHERE id = 1", &[users_table()])
        .unwrap()
        .unwrap();

    assert_eq!(root.kind(), "ResolvedDeleteStmt");
    let scan = find_kind(&root, "ResolvedTableScan").expect("a DELETE scans its target table");
    assert_eq!(scan.table_name(), Some("users"));
}

#[test]
fn create_table_statement_reports_its_column_definitions() {
    let mut module = Module::new().unwrap();

    // Each column of a CREATE TABLE becomes a ResolvedColumnDefinition that
    // names the column it declares.
    let root = module
        .resolved_tree("CREATE TABLE t (a INT64, b STRING)", &[])
        .unwrap()
        .unwrap();

    assert_eq!(root.kind(), "ResolvedCreateTableStmt");
    let names: Vec<&str> = root
        .children()
        .iter()
        .filter(|c| c.kind() == "ResolvedColumnDefinition")
        .map(|c| {
            c.column_definition_name()
                .expect("a column definition names its column")
        })
        .collect();
    assert_eq!(names, vec!["a", "b"]);
}

#[test]
fn non_column_definition_nodes_have_no_column_definition_name() {
    let mut module = Module::new().unwrap();

    let root = module
        .resolved_tree("SELECT id FROM users", &[users_table()])
        .unwrap()
        .unwrap();

    // The statement root does not declare a column.
    assert_eq!(root.kind(), "ResolvedQueryStmt");
    assert_eq!(root.column_definition_name(), None);
}
