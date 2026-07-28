//! Host runtime that drives the GoogleSQL prebuilt wasm on top of wasmtime.
//!
//! ABI details are documented in `docs/SPIKE.md`. Key points:
//! - WASI (preview1) is provided as a reactor; `/` is pre-opened (for timezone reads, etc.)
//! - All C++ runtime imports from `env` are satisfied by stubs
//! - `wasmify::callback_invoke` is provided (returns 0 in the MVP — no callbacks registered)
//! - RPC convention: `w_<svc>_<mid>(ptr,len) -> (ptr<<32 | len)`

use wasmtime::{
    Caller, Engine, Extern, ExternType, Instance, Linker, Memory, Module as WasmModule, Store,
    TypedFunc, Val, ValType,
};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

use crate::error::Error;

/// Absolute path to googlesql.wasm prepared by build.rs.
const WASM_PATH: &str = env!("GOOGLESQL_WASM_PATH");

/// Host state carried in the wasmtime `Store`.
struct HostState {
    wasi: WasiP1Ctx,
}

/// A single instance of the GoogleSQL wasm module.
///
/// wasmtime's `Store` requires exclusive access (`&mut`), so every method takes
/// `&mut self`, serializing all calls through a single instance.
pub struct Module {
    store: Store<HostState>,
    memory: Memory,
    alloc_fn: TypedFunc<u32, u32>,
    free_fn: TypedFunc<u32, ()>,
    instance: Instance,
}

impl Module {
    /// Loads the embedded wasm and returns a fully initialized instance.
    pub fn new() -> Result<Self, Error> {
        let engine = Engine::default();
        let wasm = WasmModule::from_file(&engine, WASM_PATH)
            .map_err(|e| Error::Instantiate(format!("load {WASM_PATH}: {e}")))?;

        let mut linker: Linker<HostState> = Linker::new(&engine);
        wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |s: &mut HostState| &mut s.wasi)
            .map_err(|e| Error::Instantiate(format!("wasi linker: {e}")))?;
        register_callback_import(&mut linker)?;
        register_env_stubs(&mut linker, &wasm)?;

        let mut builder = WasiCtxBuilder::new();
        builder.inherit_stderr();
        // Resolve the host tz database (may be a chain of symlinks on macOS) and
        // preopen the real directory directly at the guest path cctz expects, so
        // absl's FindTimeZoneByName works without in-sandbox symlink traversal.
        if let Ok(real) = std::fs::canonicalize("/usr/share/zoneinfo") {
            builder.env("TZDIR", "/usr/share/zoneinfo");
            let _ = builder.preopened_dir(
                &real,
                "/usr/share/zoneinfo",
                DirPerms::READ,
                FilePerms::READ,
            );
        }
        let wasi = builder
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

        // Initialize the WASI reactor (runs C++ global constructors).
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

    /// Allocates `len` bytes in wasm linear memory and returns the pointer.
    pub fn alloc(&mut self, len: u32) -> Result<u32, Error> {
        self.alloc_fn
            .call(&mut self.store, len)
            .map_err(|e| Error::Wasm(format!("wasm_alloc({len}): {e}")))
    }

    /// Frees a pointer previously returned by `alloc`.
    pub fn free(&mut self, ptr: u32) -> Result<(), Error> {
        self.free_fn
            .call(&mut self.store, ptr)
            .map_err(|e| Error::Wasm(format!("wasm_free({ptr}): {e}")))
    }

    /// Writes a byte slice into wasm memory at `ptr`.
    pub fn write(&mut self, ptr: u32, data: &[u8]) -> Result<(), Error> {
        let offset = usize::try_from(ptr).map_err(|e| Error::Memory(e.to_string()))?;
        self.memory
            .write(&mut self.store, offset, data)
            .map_err(|e| Error::Memory(e.to_string()))
    }

    /// Reads `len` bytes from wasm memory starting at `ptr` and returns them as a `Vec<u8>`.
    pub fn read(&mut self, ptr: u32, len: u32) -> Result<Vec<u8>, Error> {
        let offset = usize::try_from(ptr).map_err(|e| Error::Memory(e.to_string()))?;
        let len = usize::try_from(len).map_err(|e| Error::Memory(e.to_string()))?;
        let mut buf = vec![0u8; len];
        self.memory
            .read(&self.store, offset, &mut buf)
            .map_err(|e| Error::Memory(e.to_string()))?;
        Ok(buf)
    }

    /// Invokes a wasmify RPC.
    ///
    /// Follows the `w_<svc>_<mid>(ptr,len) -> (ptr<<32 | len)` convention:
    /// writes `req` into wasm memory, calls the export, and returns the response bytes.
    /// Both the request and response buffers are freed after the call.
    pub fn invoke(&mut self, svc: i32, mid: i32, req: &[u8]) -> Result<Vec<u8>, Error> {
        let name = format!("w_{svc}_{mid}");
        self.call_export(&name, req)
    }

    /// Calls a named export (`w_<svc>_<mid>` or `wasmify_get_type_name`, etc.)
    /// using the `(ptr,len) -> (ptr<<32 | len)` convention.
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

/// Registers the `wasmify::callback_invoke` import.
///
/// In the MVP, no Catalog or other callbacks are used, so this always returns 0
/// (meaning "no handler registered"), following the same convention as wazero.
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

/// Satisfies all C++ runtime `env` imports required by the wasm module with stubs.
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

/// Body of each `env` stub (ported from wazero's envStub convention).
fn env_stub_call(
    name: &str,
    result_types: &[ValType],
    caller: &mut Caller<'_, HostState>,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<()> {
    // C++ throws cannot be unwound, so trap.
    if name.contains("__cxa_throw") || name.ends_with("_throw") {
        return Err(wasmtime::Error::msg(format!(
            "C++ exception thrown in wasm (env::{name})"
        )));
    }
    // Semaphore waits succeed immediately in a single-threaded context.
    if name.contains("SemWait") || name.contains("sem_wait") {
        if let Some(slot) = results.get_mut(0) {
            *slot = Val::I32(1);
        }
        return Ok(());
    }
    // Satisfy C++ exception object allocation via wasm_alloc.
    if name.contains("allocate_exception") {
        let size = params.first().and_then(Val::i32).unwrap_or(64);
        let size = if size <= 0 { 64 } else { size };
        let ptr = alloc_via_caller(caller, size).unwrap_or(0);
        if let Some(slot) = results.get_mut(0) {
            *slot = Val::I32(ptr);
        }
        return Ok(());
    }
    // All other stubs return the zero value for each result type.
    for (i, ty) in result_types.iter().enumerate() {
        if let Some(slot) = results.get_mut(i) {
            *slot = zero_val(ty);
        }
    }
    Ok(())
}

/// Calls `wasm_alloc` through a `Caller` and returns the allocated pointer (i32).
fn alloc_via_caller(caller: &mut Caller<'_, HostState>, size: i32) -> Option<i32> {
    let Some(Extern::Func(alloc)) = caller.get_export("wasm_alloc") else {
        return None;
    };
    let typed = alloc.typed::<u32, u32>(&caller).ok()?;
    let size = u32::try_from(size).ok()?;
    let ptr = typed.call(&mut *caller, size).ok()?;
    i32::try_from(ptr).ok()
}

/// Returns the zero `Val` for a numeric `ValType`.
const fn zero_val(ty: &ValType) -> Val {
    match ty {
        ValType::I32 => Val::I32(0),
        ValType::I64 => Val::I64(0),
        ValType::F32 => Val::F32(0),
        ValType::F64 => Val::F64(0),
        _ => Val::I32(0),
    }
}
