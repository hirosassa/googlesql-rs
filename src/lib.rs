//! GoogleSQL (ZetaSQL) の Rust バインディング。
//!
//! goccy/googlesql-wasm が公開する prebuilt WebAssembly モジュールを
//! wasmtime 上で駆動して GoogleSQL のパーサ機能を提供する。

mod ast;
mod error;
mod parser;
mod pb;
mod runtime;

pub use ast::AstNode;
pub use error::Error;
pub use parser::ParsedStatement;
pub use runtime::Module;
