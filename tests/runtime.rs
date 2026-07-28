//! Tests for the runtime foundation (wasm instantiation and memory operations).
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use googlesql::Module;

/// The wasm module can be instantiated and memory can be round-tripped through the allocator.
#[test]
fn instantiates_and_roundtrips_memory() {
    let mut module = Module::new().unwrap();

    let data = b"hello googlesql wasm";
    let len = u32::try_from(data.len()).unwrap();

    let ptr = module.alloc(len).unwrap();
    assert_ne!(ptr, 0, "wasm_alloc returned NULL");

    module.write(ptr, data).unwrap();
    let back = module.read(ptr, len).unwrap();
    assert_eq!(
        back, data,
        "written bytes must round-trip through wasm memory"
    );

    module.free(ptr).unwrap();
}

/// A real RPC (NewParserOptions = svc699/mid0, empty request) succeeds and returns
/// a non-empty response (the ParserOptions handle).
#[test]
fn invoke_new_parser_options_returns_response() {
    let mut module = Module::new().unwrap();
    let resp = module.invoke(699, 0, &[]).unwrap();
    assert!(!resp.is_empty(), "NewParserOptions response was empty");
}
