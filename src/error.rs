//! Crate-wide error type.

/// Errors that can occur when using the GoogleSQL bindings.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Failed to load or instantiate the wasm module.
    #[error("failed to instantiate googlesql wasm: {0}")]
    Instantiate(String),

    /// A wasm runtime error (e.g. failed export call).
    #[error("wasm runtime error: {0}")]
    Wasm(String),

    /// Failed to read from or write to wasm linear memory.
    #[error("wasm memory access error: {0}")]
    Memory(String),

    /// An error returned by GoogleSQL itself (e.g. a syntax error).
    #[error("googlesql error: {0}")]
    GoogleSql(String),
}
