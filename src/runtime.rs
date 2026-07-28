//! GoogleSQL の prebuilt wasm を wasmtime 上で駆動するホストランタイム。
//!
//! ABI の詳細は `docs/SPIKE.md` を参照。要点:
//! - WASI(preview1)をリアクタとして提供し、`/` をマウント(タイムゾーン等の読取)
//! - `env` の C++ ランタイム import はすべてスタブで満たす
//! - `wasmify::callback_invoke` を提供(MVP では未使用のため 0 を返す)
//! - RPC は `w_<svc>_<mid>(ptr,len) -> (ptr<<32 | len)` の規約

use wasmtime::{
    Caller, Engine, Extern, ExternType, Instance, Linker, Memory, Module as WasmModule, Store,
    TypedFunc, Val, ValType,
};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

use crate::error::Error;

/// build.rs が用意した googlesql.wasm の絶対パス。
const WASM_PATH: &str = env!("GOOGLESQL_WASM_PATH");

/// Store に載せるホスト状態。
struct HostState {
    wasi: WasiP1Ctx,
}

/// GoogleSQL wasm の単一インスタンス。
///
/// wasmtime の `Store` は排他アクセス(`&mut`)を要するため、各メソッドは
/// `&mut self` を取り、単一インスタンスへの呼び出しを直列化する。
pub struct Module {
    store: Store<HostState>,
    memory: Memory,
    alloc_fn: TypedFunc<u32, u32>,
    free_fn: TypedFunc<u32, ()>,
    instance: Instance,
}

impl Module {
    /// 埋め込まれた wasm をロードし、初期化まで済ませたインスタンスを返す。
    pub fn new() -> Result<Self, Error> {
        let engine = Engine::default();
        let wasm = WasmModule::from_file(&engine, WASM_PATH)
            .map_err(|e| Error::Instantiate(format!("load {WASM_PATH}: {e}")))?;

        let mut linker: Linker<HostState> = Linker::new(&engine);
        wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |s: &mut HostState| &mut s.wasi)
            .map_err(|e| Error::Instantiate(format!("wasi linker: {e}")))?;
        register_callback_import(&mut linker)?;
        register_env_stubs(&mut linker, &wasm)?;

        let wasi = WasiCtxBuilder::new()
            .inherit_stderr()
            .preopened_dir("/", "/", DirPerms::READ, FilePerms::READ)
            .map_err(|e| Error::Instantiate(format!("preopen /: {e}")))?
            .build_p1();
        let mut store = Store::new(&engine, HostState { wasi });

        let instance = linker
            .instantiate(&mut store, &wasm)
            .map_err(|e| Error::Instantiate(format!("instantiate: {e}")))?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| Error::Instantiate("wasm export `memory` not found".into()))?;

        // WASI リアクタ初期化(C++ グローバルコンストラクタ実行)。
        if let Some(init) = instance.get_func(&mut store, "_initialize") {
            let init = init
                .typed::<(), ()>(&store)
                .map_err(|e| Error::Instantiate(format!("_initialize type: {e}")))?;
            init.call(&mut store, ())
                .map_err(|e| Error::Instantiate(format!("_initialize call: {e}")))?;
        }
        if let Some(winit) = instance.get_func(&mut store, "wasm_init") {
            let winit = winit
                .typed::<(), u32>(&store)
                .map_err(|e| Error::Instantiate(format!("wasm_init type: {e}")))?;
            winit
                .call(&mut store, ())
                .map_err(|e| Error::Instantiate(format!("wasm_init call: {e}")))?;
        }

        let alloc_fn = instance
            .get_typed_func::<u32, u32>(&mut store, "wasm_alloc")
            .map_err(|e| Error::Instantiate(format!("wasm_alloc: {e}")))?;
        let free_fn = instance
            .get_typed_func::<u32, ()>(&mut store, "wasm_free")
            .map_err(|e| Error::Instantiate(format!("wasm_free: {e}")))?;

        Ok(Self {
            store,
            memory,
            alloc_fn,
            free_fn,
            instance,
        })
    }

    /// wasm 線形メモリを `len` バイト確保し、その先頭ポインタを返す。
    pub fn alloc(&mut self, len: u32) -> Result<u32, Error> {
        self.alloc_fn
            .call(&mut self.store, len)
            .map_err(|e| Error::Wasm(format!("wasm_alloc({len}): {e}")))
    }

    /// `alloc` で確保したポインタを解放する。
    pub fn free(&mut self, ptr: u32) -> Result<(), Error> {
        self.free_fn
            .call(&mut self.store, ptr)
            .map_err(|e| Error::Wasm(format!("wasm_free({ptr}): {e}")))
    }

    /// `ptr` からバイト列を書き込む。
    pub fn write(&mut self, ptr: u32, data: &[u8]) -> Result<(), Error> {
        let offset = usize::try_from(ptr).map_err(|e| Error::Memory(e.to_string()))?;
        self.memory
            .write(&mut self.store, offset, data)
            .map_err(|e| Error::Memory(e.to_string()))
    }

    /// `ptr` から `len` バイトを読み出してコピーを返す。
    pub fn read(&mut self, ptr: u32, len: u32) -> Result<Vec<u8>, Error> {
        let offset = usize::try_from(ptr).map_err(|e| Error::Memory(e.to_string()))?;
        let len = usize::try_from(len).map_err(|e| Error::Memory(e.to_string()))?;
        let mut buf = vec![0u8; len];
        self.memory
            .read(&self.store, offset, &mut buf)
            .map_err(|e| Error::Memory(e.to_string()))?;
        Ok(buf)
    }

    /// wasmify RPC を呼び出す。
    ///
    /// export `w_<svc>_<mid>(ptr,len) -> (ptr<<32 | len)` の規約に従い、
    /// `req` を wasm メモリに書いて渡し、応答バイト列をコピーして返す。
    /// リクエスト/レスポンスの各バッファは呼び出し後に解放する。
    pub fn invoke(&mut self, svc: i32, mid: i32, req: &[u8]) -> Result<Vec<u8>, Error> {
        let name = format!("w_{svc}_{mid}");
        self.call_export(&name, req)
    }

    /// 名前付き export(`w_<svc>_<mid>` または `wasmify_get_type_name` 等)を
    /// `(ptr,len) -> (ptr<<32 | len)` 規約で呼び出す。
    pub fn call_export(&mut self, name: &str, req: &[u8]) -> Result<Vec<u8>, Error> {
        let func = self
            .instance
            .get_typed_func::<(u32, u32), u64>(&mut self.store, name)
            .map_err(|e| Error::Wasm(format!("export `{name}`: {e}")))?;

        let (req_ptr, req_len) = if req.is_empty() {
            (0, 0)
        } else {
            let len = u32::try_from(req.len()).map_err(|e| Error::Memory(e.to_string()))?;
            let ptr = self.alloc(len)?;
            if ptr == 0 {
                return Err(Error::Wasm("wasm_alloc returned NULL".into()));
            }
            self.write(ptr, req)?;
            (ptr, len)
        };

        let packed = func
            .call(&mut self.store, (req_ptr, req_len))
            .map_err(|e| Error::Wasm(format!("`{name}` call: {e}")))?;

        if req_ptr != 0 {
            self.free(req_ptr)?;
        }

        let resp_ptr = u32::try_from(packed >> 32).map_err(|e| Error::Memory(e.to_string()))?;
        let resp_len =
            u32::try_from(packed & 0xFFFF_FFFF).map_err(|e| Error::Memory(e.to_string()))?;
        if resp_len == 0 {
            return Ok(Vec::new());
        }
        let resp = self.read(resp_ptr, resp_len)?;
        self.free(resp_ptr)?;
        Ok(resp)
    }
}

/// `wasmify::callback_invoke` を登録する。
///
/// MVP では Catalog 等のコールバックを使わないため、常に「ハンドラ未登録」を
/// 意味する 0 を返す(wazero と同じ規約)。
fn register_callback_import(linker: &mut Linker<HostState>) -> Result<(), Error> {
    linker
        .func_wrap(
            "wasmify",
            "callback_invoke",
            |_caller: Caller<'_, HostState>,
             _callback_id: i32,
             _method_id: i32,
             _req_ptr: i32,
             _req_len: i32|
             -> i64 { 0 },
        )
        .map_err(|e| Error::Instantiate(format!("callback_invoke: {e}")))?;
    Ok(())
}

/// wasm が要求する `env` の C++ ランタイム import をすべてスタブで満たす。
fn register_env_stubs(linker: &mut Linker<HostState>, wasm: &WasmModule) -> Result<(), Error> {
    for import in wasm.imports() {
        if import.module() != "env" {
            continue;
        }
        let ExternType::Func(func_type) = import.ty() else {
            continue;
        };
        let name = import.name().to_string();
        let result_types: Vec<ValType> = func_type.results().collect();
        linker
            .func_new(
                "env",
                import.name(),
                func_type,
                move |mut caller, params, results| {
                    env_stub_call(&name, &result_types, &mut caller, params, results)
                },
            )
            .map_err(|e| Error::Instantiate(format!("env stub `{}`: {e}", import.name())))?;
    }
    Ok(())
}

/// `env` スタブの呼び出し本体(wazero の envStub 規約を移植)。
fn env_stub_call(
    name: &str,
    result_types: &[ValType],
    caller: &mut Caller<'_, HostState>,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<()> {
    // C++ throw は巻き戻せないので trap させる。
    if name.contains("__cxa_throw") || name.ends_with("_throw") {
        return Err(wasmtime::Error::msg(format!(
            "C++ exception thrown in wasm (env::{name})"
        )));
    }
    // セマフォ待ちはシングルスレッドなので成功(1)を返す。
    if name.contains("SemWait") || name.contains("sem_wait") {
        if let Some(slot) = results.get_mut(0) {
            *slot = Val::I32(1);
        }
        return Ok(());
    }
    // C++ 例外オブジェクトの確保は wasm_alloc で満たす。
    if name.contains("allocate_exception") {
        let size = params.first().and_then(Val::i32).unwrap_or(64);
        let size = if size <= 0 { 64 } else { size };
        let ptr = alloc_via_caller(caller, size).unwrap_or(0);
        if let Some(slot) = results.get_mut(0) {
            *slot = Val::I32(ptr);
        }
        return Ok(());
    }
    // それ以外は各戻り値型のゼロ値を返すだけ。
    for (i, ty) in result_types.iter().enumerate() {
        if let Some(slot) = results.get_mut(i) {
            *slot = zero_val(ty);
        }
    }
    Ok(())
}

/// caller 経由で `wasm_alloc` を呼び、確保したポインタ(i32)を返す。
fn alloc_via_caller(caller: &mut Caller<'_, HostState>, size: i32) -> Option<i32> {
    let Some(Extern::Func(alloc)) = caller.get_export("wasm_alloc") else {
        return None;
    };
    let typed = alloc.typed::<u32, u32>(&caller).ok()?;
    let size = u32::try_from(size).ok()?;
    let ptr = typed.call(&mut *caller, size).ok()?;
    i32::try_from(ptr).ok()
}

/// 数値型 `ValType` に対応するゼロ値の `Val` を返す。
fn zero_val(ty: &ValType) -> Val {
    match ty {
        ValType::I32 => Val::I32(0),
        ValType::I64 => Val::I64(0),
        ValType::F32 => Val::F32(0),
        ValType::F64 => Val::F64(0),
        _ => Val::I32(0),
    }
}
