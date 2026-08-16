# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2026-08-16

### Changed

- **The default execution backend is now `native-ffi`** (a prebuilt C-ABI
  cdylib) instead of wasmtime, and **the wasmtime engine is now optional**,
  gated behind a new `wasmtime` feature. A default `cargo add googlesql` no
  longer pulls the wasmtime/cranelift dependency tree; it links the prebuilt
  cdylib instead. `Module::new` now returns whichever engine the enabled
  features select — wasmtime when the `wasmtime` feature is on (it takes
  precedence), otherwise the `native-ffi` cdylib, otherwise the `native`
  (wasm2rs) engine.
  - **Breaking for two setups.** (1) Builds that relied on `Module::new` being
    wasmtime without enabling a feature must now add `--features wasmtime` (or
    `--no-default-features --features wasmtime`). (2) The default now requires a
    prebuilt cdylib: it ships for `aarch64-apple-darwin` and
    `x86_64-unknown-linux-gnu`; on any other target either build the cdylib from
    source and set `GUEST_FFI_LIB`, or switch to `--no-default-features
    --features wasmtime`.
  - docs.rs documents the `wasmtime` and `native-ffi` backends (via
    `package.metadata.docs.rs`), since its sandbox cannot fetch the default
    cdylib.

## [0.3.1] - 2026-08-11

### Fixed

- **`native-ffi`**: the prebuilt C-ABI `cdylib` is now relocatable. Its Mach-O
  install name is `@rpath/libguest_ffi.dylib` (and its ELF `SONAME` the bare
  `libguest_ffi.so`) instead of an absolute build-tree path, so a consumer can
  ship the library next to its binary and resolve it via an rpath. `build.rs`
  now publishes the resolved library directory to dependents as
  `DEP_GUEST_FFI_LIBDIR` for that purpose; see
  [`docs/NATIVE.md`](docs/NATIVE.md) for the consumer-side `build.rs` snippet.
- **`native-ffi`**: refresh the pinned prebuilt cdylibs to `native-ffi-v0.1.1`,
  the first Release built with the relocatable install name above.

## [0.3.0] - 2026-08-10

### Added

#### Native execution backends (optional)

- **`native`** — `Module::new_native` runs the module through the
  wasm2rs-transpiled guest compiled into your build, with no wasmtime runtime.
  The transpiled sources are large and provisioned separately; see
  [`docs/NATIVE.md`](docs/NATIVE.md).
- **`native-ffi`** — `Module::new_native_ffi` links a prebuilt C-ABI `cdylib`
  fetched from a GitHub Release (zstd-compressed, SHA256-verified) instead of
  compiling the guest, avoiding the multi-minute build. Prebuilt for
  `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu`; any other target builds
  the `cdylib` from source via `GUEST_FFI_LIB`.

The public API is identical across engines — the backend choice is invisible
past construction.

### Changed

- Precompile the bundled `googlesql.wasm` to a `.cwasm` at build time and
  deserialize it at startup instead of JIT-compiling on first use.
- Abstract the wasm engine behind an internal `GuestInstance` trait, isolating
  the wasmtime implementation and making the alternate backends possible.
- Parameterize the benchmarks by backend.
- Bump dependencies: `wasmtime-wasi` 47.0.3, `sha2` 0.11, and `criterion` 0.8.2.

### Fixed

- Free the wasm response buffer even when reading the response fails, so a
  failed read no longer leaks guest memory.

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
