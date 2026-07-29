//! Rust bindings for GoogleSQL (ZetaSQL).
//!
//! Drives the prebuilt WebAssembly module published by goccy/googlesql-wasm on
//! top of wasmtime to provide GoogleSQL parser and formatter functionality.

mod analyzer;
mod ast;
mod error;
mod formatter;
mod parser;
mod pb;
mod resolved;
mod runtime;

pub use analyzer::{ColumnDef, ColumnType, TableDef};
pub use ast::AstNode;
pub use error::Error;
pub use parser::ParsedStatement;
pub use resolved::{ColumnReference, OutputColumn, ResolvedNode, TableRef};
pub use runtime::Module;
