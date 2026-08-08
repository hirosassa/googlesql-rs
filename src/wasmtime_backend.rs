//! The default [`GuestInstance`] engine: the GoogleSQL prebuilt wasm running on
//! wasmtime.
//!
//! The [`GuestInstance`] RPC convention it serves is described on
//! [`crate::backend`]; ABI details are documented in `docs/SPIKE.md`. The
//! engine-specific wiring here:
//! - WASI (preview1) is provided as a reactor; `/` is pre-opened (for timezone reads, etc.)
//! - All C++ runtime imports from `env` are satisfied by stubs
//! - `wasmify::callback_invoke` is provided (returns 0 in the MVP — no callbacks registered)
//!
//! This module holds every wasmtime-specific type (`Store`, `Instance`,
//! `Memory`, `Linker`, `TypedFunc`, `Val`, …); the rest of the crate depends
//! only on the engine-agnostic [`GuestInstance`] trait.

use std::collections::HashMap;
use std::sync::OnceLock;

use wasmtime::{
    Caller, Engine, Extern, ExternType, Instance, Linker, Memory, Module as WasmModule, Store,
    TypedFunc, Val, ValType,
};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

use crate::backend::GuestInstance;
use crate::error::Error;

/// Absolute path to googlesql.wasm prepared by build.rs.
const WASM_PATH: &str = env!("GOOGLESQL_WASM_PATH");

/// Absolute path to the precompiled `googlesql.cwasm` prepared by build.rs.
const CWASM_PATH: &str = env!("GOOGLESQL_CWASM_PATH");

/// Process-wide cache of the compiled wasm module and its `Engine`.
///
/// Loading the ~13MB module (deserializing the precompiled `.cwasm`, or
/// JIT-compiling on fallback) dominates the cost of [`WasmtimeInstance::new`].
/// wasmtime's `Engine` and `Module` are `Send + Sync` and designed to be shared
/// across many instances and threads, so we load once here and only
/// instantiate per instance. The immutable compiled code is shared; each
/// instance still gets its own `Store`, `Instance`, and linear memory, so
/// instances stay fully isolated.
static SHARED_WASM: OnceLock<Result<(Engine, WasmModule), String>> = OnceLock::new();

/// Returns the shared `Engine` and compiled module, loading on first use.
///
/// build.rs precompiles the wasm into a `.cwasm`, so the common path just
/// deserializes that native artifact — no JIT. If deserialization fails (e.g. a
/// stale artifact from a different wasmtime version or CPU), we fall back to
/// JIT-compiling the wasm so correctness never depends on the artifact matching.
///
/// Uses `get_or_init` so the load runs exactly once even when many threads
/// construct an instance concurrently; the losers block rather than each
/// repeating the work. A failure is cached (the inputs are fixed, so it would
/// fail identically every time).
fn shared_wasm() -> Result<&'static (Engine, WasmModule), Error> {
    SHARED_WASM
        .get_or_init(|| {
            let engine = Engine::default();
            let module = load_module(&engine)?;
            Ok((engine, module))
        })
        .as_ref()
        .map_err(|e| Error::Instantiate(e.clone()))
}

/// Deserializes the precompiled `.cwasm`, falling back to JIT-compiling the wasm.
fn load_module(engine: &Engine) -> Result<WasmModule, String> {
    // SAFETY: CWASM_PATH is produced by our own build.rs from the SHA-pinned
    // wasm using the identical `Engine::default()` configuration, so the bytes
    // are a trusted, format-compatible artifact. A mismatch is caught by
    // `deserialize_file` returning `Err` (not undefined behavior), which we
    // handle by falling back to JIT below.
    #[expect(
        unsafe_code,
        reason = "deserializing a trusted build-time artifact produced by our own build.rs"
    )]
    let deserialized = unsafe { WasmModule::deserialize_file(engine, CWASM_PATH) };
    deserialized.or_else(|_| {
        WasmModule::from_file(engine, WASM_PATH)
            .map_err(|e| format!("load {WASM_PATH} (after .cwasm deserialize failed): {e}"))
    })
}

/// Host state carried in the wasmtime `Store`.
struct HostState {
    wasi: WasiP1Ctx,
}

/// A GoogleSQL guest instance backed by wasmtime.
///
/// wasmtime's `Store` requires exclusive access (`&mut`), so every method takes
/// `&mut self`, serializing all calls through one instance. The type is [`Send`]
/// (its `Store` is), so a [`Module`](crate::Module) built on it can move between
/// threads; each instance owns an isolated wasm linear memory, so many instances
/// run truly in parallel. wasmtime forbids concurrent calls into a single
/// instance, which is why it is not `Sync`.
pub struct WasmtimeInstance {
    store: Store<HostState>,
    memory: Memory,
    alloc_fn: TypedFunc<u32, u32>,
    free_fn: TypedFunc<u32, ()>,
    instance: Instance,
    /// Cache of `w_<svc>_<mid>` RPC exports, keyed by `(svc, mid)`.
    ///
    /// Resolving an export is a by-name lookup plus a type check; the tree-walking
    /// APIs make thousands of RPC calls, so caching the resolved [`TypedFunc`]
    /// removes that per-call cost (and the per-call name formatting) after the
    /// first use. `TypedFunc` is a cheap `Copy` handle independent of the store.
    invoke_cache: HashMap<(i32, i32), TypedFunc<(u32, u32), u64>>,
    /// Cache of named exports (e.g. `wasmify_get_type_name`), keyed by name, for
    /// the same reason as [`WasmtimeInstance::invoke_cache`].
    export_cache: HashMap<String, TypedFunc<(u32, u32), u64>>,
}

impl WasmtimeInstance {
    /// Loads the shared compiled wasm and returns a fully initialized instance.
    pub(crate) fn new() -> Result<Self, Error> {
        let (engine, wasm) = shared_wasm()?;

        let mut linker: Linker<HostState> = Linker::new(engine);
        wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |s: &mut HostState| &mut s.wasi)
            .map_err(|e| Error::Instantiate(format!("wasi linker: {e}")))?;
        register_callback_import(&mut linker)?;
        register_env_stubs(&mut linker, wasm)?;

        let mut builder = WasiCtxBuilder::new();
        builder.inherit_stderr();
        // Resolve the host tz database (may be a chain of symlinks on macOS) and
        // preopen the real directory directly at the guest path cctz expects, so
        // absl's FindTimeZoneByName works without in-sandbox symlink traversal.
        if let Ok(real) = std::fs::canonicalize("/usr/share/zoneinfo") {
            builder.env("TZDIR", "/usr/share/zoneinfo");
            // Best-effort: the required "/" preopen below also covers zoneinfo,
            // so failing to preopen it directly is tolerable and discarded.
            #[allow(
                clippy::let_underscore_must_use,
                reason = "optional tz preopen; the required \"/\" preopen is the guarantee"
            )]
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
        let mut store = Store::new(engine, HostState { wasi });

        let instance = linker
            .instantiate(&mut store, wasm)
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
            invoke_cache: HashMap::new(),
            export_cache: HashMap::new(),
        })
    }

    /// Resolves an export by name into a typed `(ptr,len) -> packed` function.
    fn typed_export(&mut self, name: &str) -> Result<TypedFunc<(u32, u32), u64>, Error> {
        self.instance
            .get_typed_func::<(u32, u32), u64>(&mut self.store, name)
            .map_err(|e| Error::Wasm(format!("export `{name}`: {e}")))
    }
}

impl GuestInstance for WasmtimeInstance {
    fn alloc(&mut self, len: u32) -> Result<u32, Error> {
        self.alloc_fn
            .call(&mut self.store, len)
            .map_err(|e| Error::Wasm(format!("wasm_alloc({len}): {e}")))
    }

    fn free(&mut self, ptr: u32) -> Result<(), Error> {
        self.free_fn
            .call(&mut self.store, ptr)
            .map_err(|e| Error::Wasm(format!("wasm_free({ptr}): {e}")))
    }

    fn write(&mut self, ptr: u32, data: &[u8]) -> Result<(), Error> {
        let offset = usize::try_from(ptr).map_err(|e| Error::Memory(e.to_string()))?;
        self.memory
            .write(&mut self.store, offset, data)
            .map_err(|e| Error::Memory(e.to_string()))
    }

    fn read(&mut self, ptr: u32, len: u32) -> Result<Vec<u8>, Error> {
        let offset = usize::try_from(ptr).map_err(|e| Error::Memory(e.to_string()))?;
        let len = usize::try_from(len).map_err(|e| Error::Memory(e.to_string()))?;
        let mut buf = vec![0u8; len];
        self.memory
            .read(&self.store, offset, &mut buf)
            .map_err(|e| Error::Memory(e.to_string()))?;
        Ok(buf)
    }

    fn call_rpc(&mut self, svc: i32, mid: i32, req_ptr: u32, req_len: u32) -> Result<u64, Error> {
        let func = match self.invoke_cache.get(&(svc, mid)) {
            Some(func) => func.clone(),
            None => {
                let name = format!("w_{svc}_{mid}");
                let func = self.typed_export(&name)?;
                self.invoke_cache.insert((svc, mid), func.clone());
                func
            }
        };
        // Format the export name only on the error path, so the cached hot path
        // never allocates it.
        func.call(&mut self.store, (req_ptr, req_len))
            .map_err(|e| Error::Wasm(format!("`w_{svc}_{mid}` call: {e}")))
    }

    fn call_named(&mut self, name: &str, req_ptr: u32, req_len: u32) -> Result<u64, Error> {
        let func = match self.export_cache.get(name) {
            Some(func) => func.clone(),
            None => {
                let func = self.typed_export(name)?;
                self.export_cache.insert(name.to_string(), func.clone());
                func
            }
        };
        func.call(&mut self.store, (req_ptr, req_len))
            .map_err(|e| Error::Wasm(format!("`{name}` call: {e}")))
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test code")]
mod tests {
    use super::{CWASM_PATH, WasmtimeInstance};
    use crate::backend::GuestInstance;
    use wasmtime::{Engine, Module as WasmModule};

    /// build.rs precompiles the wasm to a `.cwasm` with `Engine::default()`, and
    /// the runtime deserializes it with the same default engine. This asserts the
    /// artifact exists and deserializes cleanly — i.e. the fast (no-JIT) path is
    /// actually available, not silently falling back to recompilation.
    #[test]
    #[expect(
        unsafe_code,
        reason = "deserializing a trusted build-time artifact; mirrors the runtime path"
    )]
    fn cwasm_artifact_deserializes_with_runtime_engine() {
        let engine = Engine::default();
        // SAFETY: CWASM_PATH is produced by our own build.rs from the SHA-pinned
        // wasm using the identical `Engine::default()` configuration.
        let module = unsafe { WasmModule::deserialize_file(&engine, CWASM_PATH) };
        assert!(
            module.is_ok(),
            "precompiled .cwasm at {CWASM_PATH} must deserialize with the runtime engine: {:?}",
            module.err()
        );
    }

    /// Repeated RPCs to the same `(svc, mid)` resolve the export exactly once, and
    /// a distinct `(svc, mid)` adds its own entry. `NewParserOptions` (svc699/mid0)
    /// and `NewLanguageOptions` (svc678/mid0) both take an empty request, so they
    /// exercise the cache without any request marshaling.
    #[test]
    fn call_rpc_caches_typed_export_per_svc_mid() {
        const SVC_PARSER_OPTIONS: i32 = 699;
        const MID_NEW_PARSER_OPTIONS: i32 = 0;
        const SVC_LANGUAGE_OPTIONS: i32 = 678;
        const MID_NEW_LANGUAGE_OPTIONS: i32 = 0;

        let mut inst = WasmtimeInstance::new().expect("instantiate instance");
        assert_eq!(
            inst.invoke_cache.len(),
            0,
            "cache must start empty before any RPC"
        );

        for _ in 0..3 {
            inst.call_rpc(SVC_PARSER_OPTIONS, MID_NEW_PARSER_OPTIONS, 0, 0)
                .expect("NewParserOptions RPC");
        }
        assert_eq!(
            inst.invoke_cache.len(),
            1,
            "three calls to one RPC must leave a single cached export"
        );

        inst.call_rpc(SVC_LANGUAGE_OPTIONS, MID_NEW_LANGUAGE_OPTIONS, 0, 0)
            .expect("NewLanguageOptions RPC");
        assert_eq!(
            inst.invoke_cache.len(),
            2,
            "a different (svc, mid) must add its own cache entry"
        );
    }
}
