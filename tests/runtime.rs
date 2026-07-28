//! ランタイム基盤(wasmインスタンス化とメモリ操作)のテスト。
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use googlesql::Module;

/// wasm がインスタンス化でき、アロケータ経由でメモリを往復できること。
#[test]
fn instantiates_and_roundtrips_memory() {
    let mut module = Module::new().unwrap();

    let data = b"hello googlesql wasm";
    let len = u32::try_from(data.len()).unwrap();

    let ptr = module.alloc(len).unwrap();
    assert_ne!(ptr, 0, "wasm_alloc が NULL を返した");

    module.write(ptr, data).unwrap();
    let back = module.read(ptr, len).unwrap();
    assert_eq!(back, data, "書き込んだバイト列を読み戻せること");

    module.free(ptr).unwrap();
}

/// 実 RPC(NewParserOptions = svc699/mid0, 空リクエスト)が疎通し、
/// 非空の応答(ParserOptions ハンドル)が返ること。
#[test]
fn invoke_new_parser_options_returns_response() {
    let mut module = Module::new().unwrap();
    let resp = module.invoke(699, 0, &[]).unwrap();
    assert!(!resp.is_empty(), "NewParserOptions の応答が空だった");
}
