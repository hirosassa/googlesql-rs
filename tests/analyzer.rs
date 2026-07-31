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
    Catalog, ColumnDef, ColumnType, ConstantDef, ConstantValue, EnumDef, EnumValue, Error,
    FunctionDef, FunctionKind, LanguageFeature, Module, NamedCatalog, NamedType, ProcedureDef,
    QueryParameter, StatementKind, StructField, TableDef, TvfArgument, TvfDef,
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
        constants: vec![],
        parameters: vec![],
        table_functions: vec![],
        catalogs: vec![],
        procedures: vec![],
        connections: vec![],
        types: vec![],
        enums: vec![],
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
        constants: vec![],
        parameters: vec![],
        table_functions: vec![],
        catalogs: vec![],
        procedures: vec![],
        connections: vec![],
        types: vec![],
        enums: vec![],
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
        constants: vec![],
        parameters: vec![],
        table_functions: vec![],
        catalogs: vec![],
        procedures: vec![],
        connections: vec![],
        types: vec![],
        enums: vec![],
    };

    let result = module.analyze_statement_in("SELECT needs_int('x')", &catalog);
    assert!(result.is_err(), "argument type mismatch must not resolve");
}

#[test]
fn resolves_a_registered_named_constant() {
    let mut module = Module::new().unwrap();

    // A named constant my_const of type INT64. Registering it lets the bare name
    // resolve as an expression yielding the constant's type; without it the name
    // is an unrecognized-column/name error.
    let catalog = Catalog {
        constants: vec![ConstantDef {
            name: "my_const".to_string(),
            value: ConstantValue::Int64(42),
        }],
        ..Catalog::default()
    };

    let columns = module
        .analyze_output_columns_in("SELECT my_const AS c", &catalog)
        .unwrap();

    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].type_name(), "INT64");
}

#[test]
fn registered_string_constant_resolves_with_its_type() {
    let mut module = Module::new().unwrap();

    let catalog = Catalog {
        constants: vec![ConstantDef {
            name: "greeting".to_string(),
            value: ConstantValue::String("hi".to_string()),
        }],
        ..Catalog::default()
    };

    let columns = module
        .analyze_output_columns_in("SELECT greeting AS g", &catalog)
        .unwrap();

    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].type_name(), "STRING");
}

#[test]
fn resolves_constants_of_each_scalar_value_type() {
    let mut module = Module::new().unwrap();

    // Exercises every ConstantValue variant, including a negative INT64 (the
    // full two's-complement varint path) and a DOUBLE (fixed64 encoding).
    let catalog = Catalog {
        constants: vec![
            ConstantDef {
                name: "neg".to_string(),
                value: ConstantValue::Int64(-7),
            },
            ConstantDef {
                name: "ratio".to_string(),
                value: ConstantValue::Double(3.5),
            },
            ConstantDef {
                name: "flag".to_string(),
                value: ConstantValue::Bool(true),
            },
        ],
        ..Catalog::default()
    };

    let columns = module
        .analyze_output_columns_in("SELECT neg AS a, ratio AS b, flag AS c", &catalog)
        .unwrap();

    assert_eq!(columns.len(), 3);
    assert_eq!(columns[0].type_name(), "INT64");
    assert_eq!(columns[1].type_name(), "DOUBLE");
    assert_eq!(columns[2].type_name(), "BOOL");
}

#[test]
fn declared_query_parameter_resolves_with_its_type() {
    let mut module = Module::new().unwrap();

    // Declaring @id as INT64 lets `SELECT @id` resolve to a known INT64 output
    // type, rather than an inferred/unknown parameter type.
    let catalog = Catalog {
        parameters: vec![QueryParameter {
            name: "id".to_string(),
            ty: ColumnType::Int64,
        }],
        ..Catalog::default()
    };

    let columns = module
        .analyze_output_columns_in("SELECT @id AS v", &catalog)
        .unwrap();

    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].type_name(), "INT64");
}

#[test]
fn declared_query_parameter_type_checks_its_uses() {
    let mut module = Module::new().unwrap();

    // @n is declared INT64, so using it where a STRING is required must fail to
    // resolve — the declared type is enforced.
    let catalog = Catalog {
        parameters: vec![QueryParameter {
            name: "n".to_string(),
            ty: ColumnType::Int64,
        }],
        ..Catalog::default()
    };

    // `NOT` requires BOOL, so applying it to the INT64 parameter must fail.
    let result = module.analyze_statement_in("SELECT NOT @n", &catalog);
    assert!(result.is_err(), "declared parameter type must be enforced");
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

#[test]
fn restricts_analysis_to_the_supported_statement_kinds() {
    let mut module = Module::new().unwrap();

    // Only query statements are allowed; DML/DDL kinds fall outside the set.
    module.set_supported_statement_kinds(&[StatementKind::Query]);

    let t = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "a".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    // A query is in the allowed set, so it resolves.
    let query = module.analyze_statement_with_catalog("SELECT a FROM t", std::slice::from_ref(&t));
    assert!(
        query.is_ok(),
        "expected the query to be accepted, got: {query:?}"
    );

    // INSERT is a DML kind outside the allowed set, so the analyzer rejects it.
    let insert = module.analyze_statement_with_catalog("INSERT INTO t (a) VALUES (1)", &[t]);
    assert!(
        matches!(insert, Err(Error::GoogleSql(_))),
        "expected INSERT to be rejected, got: {insert:?}"
    );
}

#[test]
fn empty_supported_statement_kinds_allows_every_kind() {
    let mut module = Module::new().unwrap();

    // An empty set mirrors ZetaSQL's `SetSupportedStatementKinds({})`, which
    // accepts every kind, so DDL keeps resolving.
    module.set_supported_statement_kinds(&[]);

    let result = module.analyze_statement_with_catalog("CREATE TABLE t (a INT64)", &[]);
    assert!(
        result.is_ok(),
        "expected CREATE TABLE to be accepted, got: {result:?}"
    );
}

#[test]
fn supported_statement_kinds_can_allow_multiple_kinds() {
    let mut module = Module::new().unwrap();

    // Restrict to query and INSERT; UPDATE stays outside the set.
    module.set_supported_statement_kinds(&[StatementKind::Query, StatementKind::Insert]);

    let t = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "a".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    let insert = module
        .analyze_statement_with_catalog("INSERT INTO t (a) VALUES (1)", std::slice::from_ref(&t));
    assert!(
        insert.is_ok(),
        "expected INSERT to be accepted, got: {insert:?}"
    );

    let update = module.analyze_statement_with_catalog("UPDATE t SET a = 1 WHERE a = 2", &[t]);
    assert!(
        matches!(update, Err(Error::GoogleSql(_))),
        "expected UPDATE to be rejected, got: {update:?}"
    );
}

#[test]
fn disabling_the_qualify_feature_rejects_the_qualify_clause() {
    let mut module = Module::new().unwrap();

    // QUALIFY is gated behind its own language feature; disabling it makes the
    // clause fail to resolve even though the maximum feature set is otherwise on.
    module.disable_language_features(&[LanguageFeature::Qualify]);

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

    let qualified = module.analyze_statement_with_catalog(
        "SELECT a FROM t QUALIFY ROW_NUMBER() OVER (PARTITION BY b ORDER BY a) = 1",
        std::slice::from_ref(&t),
    );
    assert!(
        matches!(qualified, Err(Error::GoogleSql(_))),
        "expected QUALIFY to be rejected, got: {qualified:?}"
    );

    // A plain query does not use the disabled feature, so it still resolves.
    let plain = module.analyze_statement_with_catalog("SELECT a FROM t", std::slice::from_ref(&t));
    assert!(
        plain.is_ok(),
        "expected the plain query to be accepted, got: {plain:?}"
    );
}

#[test]
fn disabling_analytic_functions_rejects_window_functions() {
    let mut module = Module::new().unwrap();

    module.disable_language_features(&[LanguageFeature::AnalyticFunctions]);

    let t = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "a".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    let windowed =
        module.analyze_statement_with_catalog("SELECT ROW_NUMBER() OVER (ORDER BY a) FROM t", &[t]);
    assert!(
        matches!(windowed, Err(Error::GoogleSql(_))),
        "expected the window function to be rejected, got: {windowed:?}"
    );
}

#[test]
fn empty_disabled_feature_set_keeps_the_maximum_features() {
    let mut module = Module::new().unwrap();

    // An empty set disables nothing, so gated syntax such as QUALIFY still
    // resolves exactly as it does by default.
    module.disable_language_features(&[]);

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

    let result = module.analyze_statement_with_catalog(
        "SELECT a FROM t QUALIFY ROW_NUMBER() OVER (PARTITION BY b ORDER BY a) = 1",
        &[t],
    );
    assert!(
        result.is_ok(),
        "expected QUALIFY to be accepted, got: {result:?}"
    );
}

#[test]
fn enabling_only_analytic_functions_starts_from_a_minimal_feature_set() {
    let mut module = Module::new().unwrap();

    // Start from the minimal feature set (every feature off) and turn on only
    // analytic functions. Window functions then resolve, but any other gated
    // feature stays off, proving the base is minimal rather than maximal.
    module.enable_only_language_features(&[LanguageFeature::AnalyticFunctions]);

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

    let windowed = module.analyze_statement_with_catalog(
        "SELECT ROW_NUMBER() OVER (ORDER BY a) FROM t",
        std::slice::from_ref(&t),
    );
    assert!(
        windowed.is_ok(),
        "expected the window function to be accepted, got: {windowed:?}"
    );

    // QUALIFY is gated behind its own feature, which was not enabled, so it is
    // rejected even though analytic functions are on.
    let qualified = module.analyze_statement_with_catalog(
        "SELECT a FROM t QUALIFY ROW_NUMBER() OVER (PARTITION BY b ORDER BY a) = 1",
        std::slice::from_ref(&t),
    );
    assert!(
        matches!(qualified, Err(Error::GoogleSql(_))),
        "expected QUALIFY to be rejected, got: {qualified:?}"
    );
}

#[test]
fn empty_enabled_feature_set_disables_every_optional_feature() {
    let mut module = Module::new().unwrap();

    // An empty enable list leaves the minimal set untouched: every optional
    // feature is off, so gated syntax fails while a plain query still resolves.
    module.enable_only_language_features(&[]);

    let t = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "a".to_string(),
            ty: ColumnType::Int64,
        }],
    };

    let plain = module.analyze_statement_with_catalog("SELECT a FROM t", std::slice::from_ref(&t));
    assert!(
        plain.is_ok(),
        "expected the plain query to be accepted, got: {plain:?}"
    );

    let windowed = module.analyze_statement_with_catalog(
        "SELECT ROW_NUMBER() OVER (ORDER BY a) FROM t",
        std::slice::from_ref(&t),
    );
    assert!(
        matches!(windowed, Err(Error::GoogleSql(_))),
        "expected the window function to be rejected, got: {windowed:?}"
    );
}

#[test]
fn analyzes_a_table_valued_function() {
    let mut module = Module::new().unwrap();

    // A fixed-output-schema TVF taking no arguments: calling it in a FROM clause
    // resolves to its declared output columns.
    let catalog = Catalog {
        table_functions: vec![TvfDef {
            name: "my_tvf".to_string(),
            arguments: vec![],
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
        }],
        ..Default::default()
    };

    let columns = module
        .analyze_output_columns_in("SELECT * FROM my_tvf()", &catalog)
        .unwrap_or_else(|e| panic!("analysis failed: {e:?}"));

    assert_eq!(columns.len(), 2, "expected the TVF's two output columns");
    assert_eq!(columns[0].name(), "id");
    assert_eq!(columns[0].type_name(), "INT64");
    assert_eq!(columns[1].name(), "name");
    assert_eq!(columns[1].type_name(), "STRING");
}

#[test]
fn analyzes_a_table_valued_function_with_a_scalar_argument() {
    let mut module = Module::new().unwrap();

    // A TVF taking one INT64 argument and returning a fixed one-column schema:
    // the call type-checks the argument and resolves to the output column.
    let catalog = Catalog {
        table_functions: vec![TvfDef {
            name: "my_tvf".to_string(),
            arguments: vec![TvfArgument::Scalar(ColumnType::Int64)],
            columns: vec![ColumnDef {
                name: "v".to_string(),
                ty: ColumnType::Int64,
            }],
        }],
        ..Default::default()
    };

    let columns = module
        .analyze_output_columns_in("SELECT * FROM my_tvf(5)", &catalog)
        .unwrap_or_else(|e| panic!("analysis failed: {e:?}"));

    assert_eq!(columns.len(), 1, "expected the TVF's one output column");
    assert_eq!(columns[0].name(), "v");
    assert_eq!(columns[0].type_name(), "INT64");
}

#[test]
fn table_valued_function_type_checks_its_argument() {
    let mut module = Module::new().unwrap();

    // The argument is declared INT64, so passing a STRING must fail to resolve.
    let catalog = Catalog {
        table_functions: vec![TvfDef {
            name: "my_tvf".to_string(),
            arguments: vec![TvfArgument::Scalar(ColumnType::Int64)],
            columns: vec![ColumnDef {
                name: "v".to_string(),
                ty: ColumnType::Int64,
            }],
        }],
        ..Default::default()
    };

    let result = module.analyze_statement_in("SELECT * FROM my_tvf('x')", &catalog);
    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected an argument type error, got: {result:?}"
    );
}

#[test]
fn analyzes_a_table_valued_function_with_an_any_relation_argument() {
    let mut module = Module::new().unwrap();

    // A TVF taking any table as input and returning a fixed schema: a `TABLE t`
    // argument referencing a registered table resolves.
    let catalog = Catalog {
        tables: vec![TableDef {
            name: "t".to_string(),
            columns: vec![ColumnDef {
                name: "a".to_string(),
                ty: ColumnType::Int64,
            }],
        }],
        table_functions: vec![TvfDef {
            name: "my_tvf".to_string(),
            arguments: vec![TvfArgument::AnyRelation],
            columns: vec![ColumnDef {
                name: "v".to_string(),
                ty: ColumnType::Int64,
            }],
        }],
        ..Default::default()
    };

    let columns = module
        .analyze_output_columns_in("SELECT * FROM my_tvf(TABLE t)", &catalog)
        .unwrap_or_else(|e| panic!("analysis failed: {e:?}"));

    assert_eq!(columns.len(), 1, "expected the TVF's one output column");
    assert_eq!(columns[0].name(), "v");
    assert_eq!(columns[0].type_name(), "INT64");
}

#[test]
fn analyzes_a_table_valued_function_with_a_typed_relation_argument() {
    let mut module = Module::new().unwrap();

    // A TVF whose input relation must match a schema of `(a INT64)`; a registered
    // table with that exact schema satisfies it.
    let catalog = Catalog {
        tables: vec![TableDef {
            name: "t".to_string(),
            columns: vec![ColumnDef {
                name: "a".to_string(),
                ty: ColumnType::Int64,
            }],
        }],
        table_functions: vec![TvfDef {
            name: "my_tvf".to_string(),
            arguments: vec![TvfArgument::Relation(vec![ColumnDef {
                name: "a".to_string(),
                ty: ColumnType::Int64,
            }])],
            columns: vec![ColumnDef {
                name: "v".to_string(),
                ty: ColumnType::Int64,
            }],
        }],
        ..Default::default()
    };

    let result = module.analyze_statement_in("SELECT * FROM my_tvf(TABLE t)", &catalog);
    assert!(
        result.is_ok(),
        "expected the matching-schema table argument to resolve, got: {result:?}"
    );
}

#[test]
fn rejects_unknown_table_valued_function() {
    let mut module = Module::new().unwrap();

    // No TVF named `missing_tvf` is registered, so the call fails to resolve.
    let result = module.analyze_statement_in("SELECT * FROM missing_tvf()", &Catalog::default());
    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected an unresolved-function error, got: {result:?}"
    );
}

#[test]
fn analyzes_a_query_against_a_nested_catalog() {
    let mut module = Module::new().unwrap();

    // Register a sub-catalog "ds" holding table "t"; the table then resolves
    // under the qualified name "ds.t", mirroring a BigQuery dataset.table.
    let catalog = Catalog {
        catalogs: vec![NamedCatalog {
            name: "ds".to_string(),
            catalog: Catalog {
                tables: vec![TableDef {
                    name: "t".to_string(),
                    columns: vec![ColumnDef {
                        name: "a".to_string(),
                        ty: ColumnType::Int64,
                    }],
                }],
                ..Catalog::default()
            },
        }],
        ..Catalog::default()
    };

    let result = module.analyze_statement_in("SELECT a FROM ds.t", &catalog);
    assert!(
        result.is_ok(),
        "expected the qualified table to resolve, got: {result:?}"
    );
}

#[test]
fn nested_catalog_tables_are_not_visible_unqualified() {
    let mut module = Module::new().unwrap();

    // The table lives only inside the "ds" sub-catalog, so referencing it
    // without the "ds." prefix must fail: the namespace is not flattened.
    let catalog = Catalog {
        catalogs: vec![NamedCatalog {
            name: "ds".to_string(),
            catalog: Catalog {
                tables: vec![TableDef {
                    name: "t".to_string(),
                    columns: vec![ColumnDef {
                        name: "a".to_string(),
                        ty: ColumnType::Int64,
                    }],
                }],
                ..Catalog::default()
            },
        }],
        ..Catalog::default()
    };

    let result = module.analyze_statement_in("SELECT a FROM t", &catalog);
    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected the unqualified table to be unresolved, got: {result:?}"
    );
}

#[test]
fn analyzes_a_query_against_a_two_level_nested_catalog() {
    let mut module = Module::new().unwrap();

    // Nest catalogs two deep ("a" -> "b" -> table "t") so the table resolves
    // under the fully qualified "a.b.t".
    let catalog = Catalog {
        catalogs: vec![NamedCatalog {
            name: "a".to_string(),
            catalog: Catalog {
                catalogs: vec![NamedCatalog {
                    name: "b".to_string(),
                    catalog: Catalog {
                        tables: vec![TableDef {
                            name: "t".to_string(),
                            columns: vec![ColumnDef {
                                name: "n".to_string(),
                                ty: ColumnType::Int64,
                            }],
                        }],
                        ..Catalog::default()
                    },
                }],
                ..Catalog::default()
            },
        }],
        ..Catalog::default()
    };

    let result = module.analyze_statement_in("SELECT n FROM a.b.t", &catalog);
    assert!(
        result.is_ok(),
        "expected the two-level qualified table to resolve, got: {result:?}"
    );
}

#[test]
fn resolves_a_range_column() {
    let mut module = Module::new().unwrap();

    // A RANGE<DATE> column: the element type (DATE) is built first, then wrapped
    // in a range type, and the resolved output column's type name proves the
    // round-trip end to end.
    let table = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "span".to_string(),
            ty: ColumnType::Range(Box::new(ColumnType::Date)),
        }],
    };

    let columns = module
        .analyze_output_columns("SELECT span FROM t", &[table])
        .unwrap();

    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].type_name(), "RANGE<DATE>");
}

#[test]
fn rejects_a_range_of_an_unsupported_element_type() {
    let mut module = Module::new().unwrap();

    // GoogleSQL only allows RANGE over DATE, DATETIME, or TIMESTAMP, so a
    // RANGE<INT64> is rejected when the type is built.
    let table = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "span".to_string(),
            ty: ColumnType::Range(Box::new(ColumnType::Int64)),
        }],
    };

    let result = module.analyze_output_columns("SELECT span FROM t", &[table]);
    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected RANGE<INT64> to be rejected, got: {result:?}"
    );
}

#[test]
fn resolves_a_map_column() {
    let mut module = Module::new().unwrap();

    let table = TableDef {
        name: "t".to_string(),
        columns: vec![ColumnDef {
            name: "m".to_string(),
            ty: ColumnType::Map(Box::new(ColumnType::String), Box::new(ColumnType::Int64)),
        }],
    };

    let columns = module
        .analyze_output_columns("SELECT m FROM t", &[table])
        .unwrap();

    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].type_name(), "MAP<STRING, INT64>");
}

#[test]
fn analyzes_a_call_to_a_registered_procedure() {
    let mut module = Module::new().unwrap();

    // A procedure taking one INT64 argument; CALL with a matching argument
    // resolves against it.
    let catalog = Catalog {
        procedures: vec![ProcedureDef {
            name: "my_proc".to_string(),
            arguments: vec![ColumnType::Int64],
        }],
        ..Catalog::default()
    };

    let result = module.analyze_statement_in("CALL my_proc(1)", &catalog);
    assert!(
        result.is_ok(),
        "expected the CALL to resolve, got: {result:?}"
    );
}

#[test]
fn procedure_call_type_checks_its_argument() {
    let mut module = Module::new().unwrap();

    // The procedure expects INT64; calling it with a STRING fails type checking.
    let catalog = Catalog {
        procedures: vec![ProcedureDef {
            name: "my_proc".to_string(),
            arguments: vec![ColumnType::Int64],
        }],
        ..Catalog::default()
    };

    let result = module.analyze_statement_in("CALL my_proc('x')", &catalog);
    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected a type-mismatch error, got: {result:?}"
    );
}

#[test]
fn rejects_a_call_to_an_unknown_procedure() {
    let mut module = Module::new().unwrap();

    let result = module.analyze_statement_in("CALL missing_proc()", &Catalog::default());
    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected an unknown-procedure error, got: {result:?}"
    );
}

#[test]
fn analyzes_create_external_table_with_a_registered_connection() {
    let mut module = Module::new().unwrap();

    // The CREATE EXTERNAL TABLE statement references connection "conn"; with it
    // registered, the connection resolves and the statement analyzes.
    let catalog = Catalog {
        connections: vec!["conn".to_string()],
        ..Catalog::default()
    };

    let result = module.analyze_statement_in(
        "CREATE EXTERNAL TABLE t WITH CONNECTION conn OPTIONS(uris=['gs://b/f'], format='CSV')",
        &catalog,
    );
    assert!(
        result.is_ok(),
        "expected the connection to resolve, got: {result:?}"
    );
}

#[test]
fn rejects_a_reference_to_an_unknown_connection() {
    let mut module = Module::new().unwrap();

    let result = module.analyze_statement_in(
        "CREATE EXTERNAL TABLE t WITH CONNECTION conn OPTIONS(uris=['gs://b/f'], format='CSV')",
        &Catalog::default(),
    );
    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected a connection-not-found error, got: {result:?}"
    );
}

#[test]
fn resolves_a_named_type_in_a_cast() {
    let mut module = Module::new().unwrap();

    // Register "point" as an alias for STRUCT<x FLOAT64, y FLOAT64>; the name is
    // then usable as a type, and the CAST resolves to the underlying struct.
    let catalog = Catalog {
        types: vec![NamedType {
            name: "point".to_string(),
            ty: ColumnType::Struct(vec![
                StructField {
                    name: "x".to_string(),
                    ty: ColumnType::Float64,
                },
                StructField {
                    name: "y".to_string(),
                    ty: ColumnType::Float64,
                },
            ]),
        }],
        ..Catalog::default()
    };

    let columns = module
        .analyze_output_columns_in("SELECT CAST(NULL AS point) AS p", &catalog)
        .unwrap();

    assert_eq!(columns.len(), 1);
    // FLOAT64 renders as DOUBLE in the resolved type name (internal product mode).
    assert_eq!(columns[0].type_name(), "STRUCT<x DOUBLE, y DOUBLE>");
}

#[test]
fn rejects_a_cast_to_an_unknown_type() {
    let mut module = Module::new().unwrap();

    let result = module.analyze_statement_in("SELECT CAST(NULL AS my_type)", &Catalog::default());
    assert!(
        matches!(result, Err(Error::GoogleSql(_))),
        "expected a type-not-found error, got: {result:?}"
    );
}

#[test]
fn resolves_an_enum_type_in_a_cast() {
    let mut module = Module::new().unwrap();

    // Register "Color" as an enum with two values; the name is then usable as a
    // type, and a CAST to it resolves.
    let catalog = Catalog {
        enums: vec![EnumDef {
            name: "Color".to_string(),
            values: vec![
                EnumValue {
                    name: "RED".to_string(),
                    number: 0,
                },
                EnumValue {
                    name: "GREEN".to_string(),
                    number: 1,
                },
            ],
        }],
        ..Catalog::default()
    };

    let columns = module
        .analyze_output_columns_in("SELECT CAST(1 AS Color) AS c", &catalog)
        .unwrap();

    assert_eq!(columns.len(), 1);
    // An enum type resolves to its ENUM<name> spelling; casting a string literal
    // to it also resolves (GoogleSQL defers value validation to runtime).
    assert_eq!(columns[0].type_name(), "ENUM<Color>");
    assert!(
        module
            .analyze_statement_in("SELECT CAST('RED' AS Color)", &catalog)
            .is_ok()
    );
}

#[test]
fn resolves_multiple_enum_types() {
    let mut module = Module::new().unwrap();

    // Two enums are built into one shared descriptor pool (each in its own
    // uniquely named file), and both become usable as types.
    let catalog = Catalog {
        enums: vec![
            EnumDef {
                name: "Color".to_string(),
                values: vec![EnumValue {
                    name: "RED".to_string(),
                    number: 0,
                }],
            },
            EnumDef {
                name: "Size".to_string(),
                values: vec![
                    EnumValue {
                        name: "SMALL".to_string(),
                        number: 0,
                    },
                    EnumValue {
                        name: "LARGE".to_string(),
                        number: 1,
                    },
                ],
            },
        ],
        ..Catalog::default()
    };

    let columns = module
        .analyze_output_columns_in(
            "SELECT CAST(0 AS Color) AS c, CAST(1 AS Size) AS s",
            &catalog,
        )
        .unwrap();

    assert_eq!(columns.len(), 2);
    assert_eq!(columns[0].type_name(), "ENUM<Color>");
    assert_eq!(columns[1].type_name(), "ENUM<Size>");
}
