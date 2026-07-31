//! End-to-end tests for the analyzer (`AnalyzeStatement`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "test code"
)]

use googlesql::{
    Catalog, ColumnDef, ColumnType, Error, FunctionDef, FunctionKind, Module, StructField, TableDef,
};

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
fn analysis_error_carries_its_source_location() {
    let mut module = Module::new().unwrap();

    let users = TableDef {
        name: "users".to_string(),
        columns: vec![ColumnDef {
            name: "id".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    // `missing_col` starts at column 8 of the single-line query; GoogleSQL
    // reports the error there, and we expose that position structurally.
    let Err(Error::GoogleSql(err)) =
        module.analyze_output_columns("SELECT missing_col FROM users", &[users])
    else {
        panic!("expected an unresolved-name error");
    };

    assert_eq!(
        err.location().map(|loc| (loc.line(), loc.column())),
        Some((1, 8))
    );
    assert!(
        err.message().starts_with("Unrecognized name: missing_col"),
        "unexpected message: {}",
        err.message()
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
fn analyzes_qualify_clause() {
    let mut module = Module::new().unwrap();

    let t = TableDef {
        name: "t".to_string(),
        columns: vec![
            ColumnDef {
                name: "a".to_string(),
                ty: ColumnType::Int64,
            },
            ColumnDef {
                name: "b".to_string(),
                ty: ColumnType::Int64,
            },
        ],
    };

    // `QUALIFY` is gated behind a GoogleSQL language feature; the analyzer
    // enables the maximum language feature set so the clause resolves.
    let result = module.analyze_statement_with_catalog(
        "SELECT a FROM t QUALIFY ROW_NUMBER() OVER (PARTITION BY b ORDER BY a) = 1",
        &[t],
    );

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
fn resolves_columns_of_each_scalar_type() {
    let mut module = Module::new().unwrap();

    // Each ColumnType maps to a TypeFactory getter; registering a column of that
    // type and reading back the resolved output column's type name proves the
    // round-trip (SQL type name == the type we asked the factory for).
    let cases = [
        (ColumnType::Bytes, "BYTES"),
        (ColumnType::Date, "DATE"),
        (ColumnType::Datetime, "DATETIME"),
        (ColumnType::Time, "TIME"),
        (ColumnType::Timestamp, "TIMESTAMP"),
        (ColumnType::Numeric, "NUMERIC"),
        (ColumnType::BigNumeric, "BIGNUMERIC"),
        (ColumnType::Json, "JSON"),
        (ColumnType::Interval, "INTERVAL"),
        (ColumnType::Geography, "GEOGRAPHY"),
    ];

    for (ty, expected) in cases {
        let table = TableDef {
            name: "t".to_string(),
            columns: vec![ColumnDef {
                name: "c".to_string(),
                ty,
            }],
        };

        let columns = module
            .analyze_output_columns("SELECT c FROM t", &[table])
            .unwrap_or_else(|e| panic!("analysis failed for {expected}: {e:?}"));

        assert_eq!(
            columns.len(),
            1,
            "expected one output column for {expected}"
        );
        assert_eq!(
            columns[0].type_name(),
            expected,
            "resolved type name mismatch for {expected}"
        );
    }
}

#[test]
fn resolves_an_array_column() {
    let mut module = Module::new().unwrap();

    // An ARRAY<STRING> column: the element type is built first, then wrapped in
    // an array type, and the resolved output column's type name proves the
    // round-trip end to end.
    let table = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "tags".to_string(),
            ty: ColumnType::Array(Box::new(ColumnType::String)),
        }],
    };

    let columns = module
        .analyze_output_columns("SELECT tags FROM t", &[table])
        .unwrap();

    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].type_name(), "ARRAY<STRING>");
}

#[test]
fn unnests_an_array_column() {
    let mut module = Module::new().unwrap();

    // The array column is usable as a table source via UNNEST, and each element
    // resolves to the array's element type.
    let table = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "scores".to_string(),
            ty: ColumnType::Array(Box::new(ColumnType::Int64)),
        }],
    };

    let columns = module
        .analyze_output_columns("SELECT s FROM t, UNNEST(scores) AS s", &[table])
        .unwrap();

    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].type_name(), "INT64");
}

#[test]
fn resolves_a_struct_column() {
    let mut module = Module::new().unwrap();

    // A STRUCT<x INT64, y INT64> column: each field type is built and named,
    // then assembled into a struct type. The resolved output column's type name
    // proves fields keep their names, types, and order.
    let table = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "point".to_string(),
            ty: ColumnType::Struct(vec![
                StructField {
                    name: "x".to_string(),
                    ty: ColumnType::Int64,
                },
                StructField {
                    name: "y".to_string(),
                    ty: ColumnType::Int64,
                },
            ]),
        }],
    };

    let columns = module
        .analyze_output_columns("SELECT point FROM t", &[table])
        .unwrap();

    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].type_name(), "STRUCT<x INT64, y INT64>");
}

#[test]
fn resolves_a_field_of_a_struct_column() {
    let mut module = Module::new().unwrap();

    // A struct field is reachable by name and resolves to its declared type.
    let table = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "p".to_string(),
            ty: ColumnType::Struct(vec![StructField {
                name: "label".to_string(),
                ty: ColumnType::String,
            }]),
        }],
    };

    let columns = module
        .analyze_output_columns("SELECT p.label FROM t", &[table])
        .unwrap();

    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].type_name(), "STRING");
}

#[test]
fn resolves_an_array_of_structs_column() {
    let mut module = Module::new().unwrap();

    // Composition: the recursive builder handles ARRAY<STRUCT<...>>.
    let table = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "items".to_string(),
            ty: ColumnType::Array(Box::new(ColumnType::Struct(vec![StructField {
                name: "n".to_string(),
                ty: ColumnType::Int64,
            }]))),
        }],
    };

    let columns = module
        .analyze_output_columns("SELECT items FROM t", &[table])
        .unwrap();

    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].type_name(), "ARRAY<STRUCT<n INT64>>");
}

#[test]
fn resolves_a_registered_scalar_function() {
    let mut module = Module::new().unwrap();

    // A user-defined scalar function my_add(INT64, INT64) -> INT64. Without the
    // registration the call is an unrecognized-function error.
    let catalog = Catalog {
        tables: vec![],
        functions: vec![FunctionDef {
            name: "my_add".to_string(),
            arguments: vec![ColumnType::Int64, ColumnType::Int64],
            return_type: ColumnType::Int64,
            kind: FunctionKind::Scalar,
        }],
    };

    let columns = module
        .analyze_output_columns_in("SELECT my_add(1, 2) AS s", &catalog)
        .unwrap();

    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].type_name(), "INT64");
}

#[test]
fn resolves_a_registered_aggregate_function() {
    let mut module = Module::new().unwrap();

    // A user-defined aggregate function my_agg(INT64) -> INT64. Registering it
    // with aggregate mode lets it be called with aggregate semantics over a
    // table column; a scalar registration would reject the aggregate call.
    let catalog = Catalog {
        tables: vec![TableDef {
            name: "t".to_string(),
            columns: vec![ColumnDef {
                name: "n".to_string(),
                ty: ColumnType::Int64,
            }],
        }],
        functions: vec![FunctionDef {
            name: "my_agg".to_string(),
            arguments: vec![ColumnType::Int64],
            return_type: ColumnType::Int64,
            kind: FunctionKind::Aggregate,
        }],
    };

    let columns = module
        .analyze_output_columns_in("SELECT my_agg(n) AS s FROM t", &catalog)
        .unwrap();

    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].type_name(), "INT64");
}

#[test]
fn registered_function_type_checks_its_arguments() {
    let mut module = Module::new().unwrap();

    // The signature's argument types are enforced: passing a STRING where INT64
    // is declared must fail to resolve.
    let catalog = Catalog {
        tables: vec![],
        functions: vec![FunctionDef {
            name: "needs_int".to_string(),
            arguments: vec![ColumnType::Int64],
            return_type: ColumnType::Bool,
            kind: FunctionKind::Scalar,
        }],
    };

    let result = module.analyze_statement_in("SELECT needs_int('x')", &catalog);
    assert!(result.is_err(), "argument type mismatch must not resolve");
}

#[test]
fn unregistered_function_does_not_resolve() {
    let mut module = Module::new().unwrap();

    let empty = Catalog::default();
    let result = module.analyze_statement_in("SELECT my_add(1, 2)", &empty);
    assert!(result.is_err(), "unregistered function must not resolve");
}

#[test]
fn analyzes_insert_statement() {
    let mut module = Module::new().unwrap();

    let t = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "a".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    // INSERT is a DML statement kind; the analyzer must be told to accept
    // statement kinds beyond query for it to resolve.
    let result = module.analyze_statement_with_catalog("INSERT INTO t (a) VALUES (1)", &[t]);

    assert!(
        result.is_ok(),
        "expected analysis to succeed, got: {result:?}"
    );
}

#[test]
fn analyzes_update_statement() {
    let mut module = Module::new().unwrap();

    let t = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "a".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    let result = module.analyze_statement_with_catalog("UPDATE t SET a = 1 WHERE a = 2", &[t]);

    assert!(
        result.is_ok(),
        "expected analysis to succeed, got: {result:?}"
    );
}

#[test]
fn analyzes_delete_statement() {
    let mut module = Module::new().unwrap();

    let t = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "a".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    let result = module.analyze_statement_with_catalog("DELETE FROM t WHERE a = 1", &[t]);

    assert!(
        result.is_ok(),
        "expected analysis to succeed, got: {result:?}"
    );
}

#[test]
fn analyzes_create_table_statement() {
    let mut module = Module::new().unwrap();

    // CREATE TABLE is a DDL statement kind and needs no catalog entries.
    let result = module.analyze_statement_with_catalog("CREATE TABLE t (a INT64)", &[]);

    assert!(
        result.is_ok(),
        "expected analysis to succeed, got: {result:?}"
    );
}

#[test]
fn output_columns_of_dml_is_empty_for_now() {
    let mut module = Module::new().unwrap();

    let t = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "a".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    // Output-column extraction is defined only for `ResolvedQueryStmt`. A DML
    // statement now analyzes successfully but projects no query output columns,
    // so the result is empty rather than an error. Surfacing DML-specific
    // structure is left to a later slice.
    let columns = module
        .analyze_output_columns("INSERT INTO t (a) VALUES (1)", &[t])
        .unwrap();

    assert!(
        columns.is_empty(),
        "expected no output columns for DML, got {} column(s)",
        columns.len()
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
