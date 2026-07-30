# googlesql-rs

[![Crates.io](https://img.shields.io/crates/v/googlesql.svg)](https://crates.io/crates/googlesql)
[![Documentation](https://docs.rs/googlesql/badge.svg)](https://docs.rs/googlesql)
[![build](https://github.com/hirosassa/googlesql-rs/actions/workflows/test.yaml/badge.svg?branch=main)](https://github.com/hirosassa/googlesql-rs/actions/workflows/test.yaml)
[![codecov](https://codecov.io/gh/hirosassa/googlesql-rs/branch/main/graph/badge.svg)](https://codecov.io/gh/hirosassa/googlesql-rs)

Rust bindings for GoogleSQL (ZetaSQL).

Drives the prebuilt WebAssembly module published by
[goccy/googlesql-wasm](https://github.com/goccy/googlesql-wasm) on top of
[wasmtime](https://wasmtime.dev/), giving you GoogleSQL's parser, formatter, and
analyzer without requiring a massive C++ / Bazel toolchain.

## Features

- **No C++ build required** — just run the pre-compiled WASM artifact of GoogleSQL
- **No cgo-style FFI needed** — `unsafe` is `forbid`
- `googlesql.wasm` is automatically fetched from GitHub Releases at build time (with SHA256 verification)

## Usage

```rust
use googlesql::Module;

fn main() -> Result<(), googlesql::Error> {
    let mut module = Module::new()?;
    let parsed = module.parse_statement("select a,b from t where a>1")?;
    println!("{}", parsed.canonical_sql());
    // SELECT
    //   a,
    //   b
    // FROM
    //   t
    // WHERE
    //   a > 1
    Ok(())
}
```

Syntax errors are returned as `Error::GoogleSql`.

### Traversing the AST

The parse result is returned as a self-contained AST tree; you can inspect the
type name, byte range within the source, and children of each node. Node text
is extracted from the byte range of the original SQL.

```rust
use googlesql::{AstNode, Module};

fn dump(node: &AstNode, sql: &str, depth: usize) {
    println!("{}{} {:?}", "  ".repeat(depth), node.kind(), node.text(sql));
    for child in node.children() {
        dump(child, sql, depth + 1);
    }
}

let mut module = Module::new()?;
let sql = "SELECT a, 42 FROM t";
let parsed = module.parse_statement(sql)?;
dump(parsed.root(), sql, 0);
// ASTQueryStatement None
//   ASTQuery None
//     ASTSelect None
//       ASTSelectList Some("a, 42")
//         ...
//           ASTIdentifier Some("a")
//         ASTSelectColumn Some("42")
//           ASTIntLiteral Some("42")
//       ASTFromClause Some("FROM t")
```

Upper-level container nodes may carry no position information (`byte_range()` returns `None`).

### Formatting

`format_sql` pretty-prints a statement into GoogleSQL's canonical layout.

```rust
use googlesql::Module;

let mut module = Module::new()?;
let formatted = module.format_sql("select a,b from t where a>1")?;
println!("{formatted}");
// SELECT
//   a,
//   b
// FROM
//   t
// WHERE
//   a > 1;
```

Like the parser, invalid SQL is returned as `Error::GoogleSql`.

### Analyzing

`analyze_statement` runs the GoogleSQL analyzer (type inference and name
resolution) over a statement. GoogleSQL's builtin functions and operators are
registered, so expressions resolve; the catalog otherwise has no user-defined
tables. It reports success or failure only.

```rust
use googlesql::Module;

let mut module = Module::new()?;

// Literals and builtin operators resolve successfully.
module.analyze_statement("SELECT 1 + 2 AS x")?;

// With no user tables in the catalog, table references fail name resolution.
assert!(module.analyze_statement("SELECT x FROM missing_table").is_err());
```

Syntax errors and unresolved names are returned as `Error::GoogleSql`.

To resolve real queries, register your tables as a catalog of `TableDef`s. The
richer analyzer APIs below all take `&[TableDef]`.

#### Output schema

`analyze_output_columns` returns the columns a query produces, each with its
(aliased) name, resolved type, and unique resolved-column id.

```rust
use googlesql::{ColumnDef, ColumnType, Module, TableDef};

let mut module = Module::new()?;
let users = TableDef {
    name: "users".to_string(),
    columns: vec![
        ColumnDef { name: "id".to_string(), ty: ColumnType::Int64 },
        ColumnDef { name: "name".to_string(), ty: ColumnType::String },
    ],
};

let columns = module.analyze_output_columns("SELECT id, name AS full_name FROM users", &[users])?;
for col in &columns {
    println!("{} : {} (id {})", col.name(), col.type_name(), col.id());
}
// id : INT64 (id 1)
// full_name : STRING (id 2)
```

#### Table and column lineage

`referenced_tables` reports the tables a query reads, each with the columns it
actually references (pruned to what the query needs).

```rust
let tables = module.referenced_tables(
    "SELECT u.name FROM users u JOIN orders o ON o.user_id = u.id",
    &[users, orders],
)?;
for table in &tables {
    println!("{} reads: {}", table.name(), table.columns().join(", "));
}
```

#### Resolved AST

`resolved_tree` returns the analyzer's fully typed output as a self-contained
tree of `ResolvedNode`s. Each node exposes its kind, resolved type, children,
and kind-specific details (column references, literal values, function and table
names, join/set-operation kinds, and more).

```rust
use googlesql::ResolvedNode;

fn print_tree(node: &ResolvedNode, depth: usize) {
    println!("{}{}", "  ".repeat(depth), node.kind());
    for child in node.children() {
        print_tree(child, depth + 1);
    }
}

if let Some(root) = module.resolved_tree("SELECT id FROM users WHERE id > 0", &[users])? {
    print_tree(&root, 0);
}
// ResolvedQueryStmt
//   ResolvedOutputColumn
//   ResolvedProjectScan
//     ResolvedFilterScan
//       ResolvedTableScan
//       ResolvedFunctionCall
//         ...
```

### Error handling

Every fallible call returns `Error`. A problem reported by GoogleSQL itself
surfaces as `Error::GoogleSql`, carrying a `SqlError` whose `location()` gives
the offending line and column when GoogleSQL supplied one.

```rust
use googlesql::Error;

if let Err(Error::GoogleSql(err)) = module.analyze_output_columns("SELECT missing_col FROM users", &[users]) {
    println!("{}", err.message());        // Unrecognized name: missing_col [at 1:8]
    if let Some(loc) = err.location() {
        println!("line {}, column {}", loc.line(), loc.column()); // line 1, column 8
    }
}
```

## Examples

Runnable examples live in [`examples/`](examples). Each builds its own catalog
and prints real output:

```sh
cargo run --example output_columns
cargo run --example referenced_tables
cargo run --example resolved_tree
cargo run --example error_location
```

## Status

- ✅ SQL statement parsing and normalization (`parse_statement` → `canonical_sql`)
- ✅ Typed access to AST nodes (type name, byte range, child traversal)
- ✅ SQL formatting (`format_sql`)
- ✅ Analyzer (`analyze_statement`) — statement validation via type inference and name resolution, with builtin functions/operators registered
- ✅ Analyzer: user-defined tables in the catalog (`TableDef`, `analyze_statement_with_catalog`)
- ✅ Analyzer: resolved query output schema (`analyze_output_columns` → `OutputColumn`)
- ✅ Analyzer: table and column lineage (`referenced_tables` → `TableRef`)
- ✅ Analyzer: typed access to the resolved AST (`resolved_tree` → `ResolvedNode`: kind, resolved type, column references, literal values, function/table names, cast types, parameters, aggregates, join/set-operation kinds, CTE names, and more)
- ✅ Structured error locations (`SqlError::location` → `ErrorLocation`: line and column of a GoogleSQL error)

## Building

The first build downloads `googlesql.wasm` (~14 MB).
For offline environments or a locally available wasm file, you can override the path via an environment variable.

```sh
# Use a local wasm file (skips the download)
GOOGLESQL_WASM=/path/to/googlesql.wasm cargo build
```

For details on the internal architecture and the WASM host ABI, see [`docs/SPIKE.md`](docs/SPIKE.md).

## License

Apache-2.0 (following GoogleSQL / ZetaSQL).
