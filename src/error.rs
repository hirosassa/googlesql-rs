//! クレート共通のエラー型。

/// GoogleSQL バインディングの操作で発生しうるエラー。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// wasm モジュールのロード・インスタンス化に失敗した。
    #[error("failed to instantiate googlesql wasm: {0}")]
    Instantiate(String),

    /// wasm ランタイムの実行(export 呼び出し等)に失敗した。
    #[error("wasm runtime error: {0}")]
    Wasm(String),

    /// wasm 線形メモリへの読み書きに失敗した。
    #[error("wasm memory access error: {0}")]
    Memory(String),

    /// GoogleSQL 側が返したエラー(構文エラー等)。
    #[error("googlesql error: {0}")]
    GoogleSql(String),
}
