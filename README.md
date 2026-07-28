# googlesql-rs

Rust bindings for GoogleSQL (ZetaSQL).

Drives the prebuilt WebAssembly module published by
[goccy/googlesql-wasm](https://github.com/goccy/googlesql-wasm) on top of
[wasmtime](https://wasmtime.dev/), giving you GoogleSQL parser and formatter
functionality without requiring a massive C++ / Bazel toolchain.

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

## Status

- ✅ SQL statement parsing and normalization (`parse_statement` → `canonical_sql`)
- ✅ Typed access to AST nodes (type name, byte range, child traversal)
- ✅ SQL formatting (`format_sql`)
- 🟡 Analyzer (`analyze_statement`) — statement validation via type inference and name resolution, with builtin functions/operators registered
- ⬜ Analyzer: user-defined tables in the catalog
- ⬜ Analyzer: typed access to the resolved AST

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
