# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
