//! Rust bindings for GoogleSQL (ZetaSQL).
//!
//! Drives the prebuilt WebAssembly module published by goccy/googlesql-wasm on
//! top of wasmtime, so you get GoogleSQL's parser, formatter, and analyzer
//! without building the C++ library or writing any `unsafe` FFI.
//!
//! # Quick start
//!
//! Everything hangs off a single [`Module`], which owns the wasm instance.
//! Create one and reuse it across queries:
//!
//! ```rust,no_run
//! use googlesql::Module;
//!
//! let mut module = Module::new()?;
//!
//! // Parse a statement, then re-print it in canonical form.
//! let stmt = module.parse_statement("SELECT 1 AS n")?;
//! println!("parsed: {}", stmt.canonical_sql());
//!
//! // Pretty-print (format) a query.
//! println!("formatted: {}", module.format_sql("select 1")?);
//! # Ok::<(), googlesql::Error>(())
//! ```
//!
//! # What you can do
//!
//! - **Parse** SQL into an untyped syntax tree — [`Module::parse_statement`]
//!   returns a [`ParsedStatement`] whose [`root`](ParsedStatement::root) is an
//!   [`AstNode`] you can walk. [`Module::parse_expression`] does the same for a
//!   bare expression fragment (e.g. `a + 1`), [`Module::parse_type`] for a
//!   type declaration (e.g. `ARRAY<INT64>`), and [`Module::parse_script`] for a
//!   full script with scripting constructs (`DECLARE`, `IF`, `WHILE`, `BEGIN
//!   ... END`).
//! - **Format** SQL — [`Module::format_sql`] canonicalizes and pretty-prints a
//!   statement.
//! - **Analyze** SQL against a catalog of [`TableDef`]s — check that a statement
//!   resolves ([`Module::analyze_statement`]), read its output schema
//!   ([`Module::analyze_output_columns`] → [`OutputColumn`]), find the tables and
//!   columns it reads ([`Module::referenced_tables`] → [`TableRef`]), or walk its
//!   fully typed resolved AST ([`Module::resolved_tree`] → [`ResolvedNode`]).
//!
//! # Errors
//!
//! Every fallible call returns [`Error`]. A problem reported by GoogleSQL itself
//! (a syntax error, an unresolved name, an unsupported feature) surfaces as
//! [`Error::GoogleSql`], carrying a [`SqlError`] whose
//! [`location`](SqlError::location) exposes the offending [`ErrorLocation`] when
//! GoogleSQL supplied one.
#![warn(missing_docs)]

mod analyzer;
mod ast;
mod error;
mod formatter;
mod parser;
mod pb;
mod resolved;
mod runtime;

pub use analyzer::{Catalog, ColumnDef, ColumnType, FunctionDef, StructField, TableDef};
pub use ast::{AstNode, BinaryOp, BinaryOperator, Literal, UnaryOp};
pub use error::{Error, ErrorLocation, SqlError, SqlErrorKind};
pub use parser::{ParsedExpression, ParsedScript, ParsedStatement, ParsedStatements, ParsedType};
pub use resolved::{
    CastInfo, ColumnReference, CreateMode, InsertMode, JoinType, LimitOffset, LiteralValue,
    MergeAction, MergeMatch, OutputColumn, ResolvedNode, SetOperation, SubqueryKind, TableRef,
};
pub use runtime::Module;
