# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-08-01

### Added

#### Parser

- Parse whole scripts, including scripting constructs, via `Module::parse_script`,
  and iterate their statements incrementally with `parse_script_statements`.
- Parse a bare SQL expression (`parse_expression`) or a standalone SQL type
  declaration (`parse_type`), not just full statements.
- Read semantic detail off syntax nodes: an `ASTIdentifier`'s unquoted name, the
  operator of an `ASTBinaryExpression` or `ASTUnaryExpression`, and the typed
  value of a literal node.

#### Analyzer — accepted statements and options

- Analyze DML, DDL, and script statements, not only queries.
- Analyze a standalone expression and report its inferred type
  (`analyze_expression`), optionally against named in-scope columns.
- Analyze every statement of a multi-statement script in one call.
- Resolve a type name to its canonical `Type` via `AnalyzeType`.
- Constrain analysis: restrict it to selected statement kinds, toggle individual
  language features off, or enable only a chosen set from a minimal baseline.
- Declare typed query parameters ahead of analysis, and infer the types of
  undeclared positional parameters.
- Select the product mode (internal ZetaSQL vs. external / BigQuery).

#### Analyzer — catalog registration

- Register `ARRAY`, `STRUCT`, `RANGE`, and `MAP` typed columns.
- Register user-defined scalar functions, aggregate functions, and named
  constants.
- Register table-valued functions: fixed-output-schema, with scalar arguments,
  and with relation arguments.
- Register user-defined procedures (for `CALL`), external connections, and
  nested sub-catalogs exposed as namespaces.
- Register user-defined named types, enum types, and proto message types.
- Register user-defined property graphs and their edge tables.

#### Resolved tree

- Expose DML/DDL structure through the resolved tree: `INSERT` conflict mode and
  target columns, `CREATE TABLE` column types and existence mode, `MERGE`
  `WHEN`-clause match and action types, and the `CreateMode` of `CREATE VIEW`,
  `CREATE TABLE AS SELECT`, and other `CREATE` forms.
- Read the value of resolved literal nodes: narrow and complex scalars, and
  composite `ARRAY`, `STRUCT`, `RANGE`, and `JSON` values.

### Changed

- Reuse a persistent wasm-side request region across RPCs, reducing per-call
  overhead on the hot parse path.

## [0.1.0] - 2026-07-30

### Added

- Parse SQL into a typed syntax tree (`Module::parse_statement` → `ParsedStatement` / `AstNode`).
- Canonicalize and pretty-print SQL (`Module::format_sql`).
- Analyze statements against a catalog of `TableDef`s:
  - validate a statement (`analyze_statement`, `analyze_statement_with_catalog`);
  - read a query's output schema (`analyze_output_columns` → `OutputColumn`);
  - report the tables and columns a query reads (`referenced_tables` → `TableRef`);
  - expose the fully typed resolved AST (`resolved_tree` → `ResolvedNode`).
- Structured error locations: `Error::GoogleSql` carries a `SqlError` whose
  `location()` returns an `ErrorLocation` (line and column).
- Coarse error classification: `SqlError::kind()` returns a `SqlErrorKind`
  (`Syntax`, `Unsupported`, or `Analysis`) derived from the message.

### Changed

- Internal wasm-ABI failures (a null handle, a missing response field, an
  unrecognized enum value) now surface as a new `Error::Protocol` variant
  instead of `Error::GoogleSql`, so a binding/module contract mismatch is no
  longer misclassified as a GoogleSQL query error.
- Enable integer overflow checks in release builds (`overflow-checks = true`),
  backing the compile-time ban on unchecked arithmetic with a runtime guard.
