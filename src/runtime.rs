//! Host runtime that drives the GoogleSQL prebuilt wasm on top of wasmtime.
//!
//! ABI details are documented in `docs/SPIKE.md`. Key points:
//! - WASI (preview1) is provided as a reactor; `/` is pre-opened (for timezone reads, etc.)
//! - All C++ runtime imports from `env` are satisfied by stubs
//! - `wasmify::callback_invoke` is provided (returns 0 in the MVP — no callbacks registered)
//! - RPC convention: `w_<svc>_<mid>(ptr,len) -> (ptr<<32 | len)`

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use wasmtime::{
    Caller, Engine, Extern, ExternType, Instance, Linker, Memory, Module as WasmModule, Store,
    TypedFunc, Val, ValType,
};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

use crate::error::Error;
use crate::pb;

/// Absolute path to googlesql.wasm prepared by build.rs.
const WASM_PATH: &str = env!("GOOGLESQL_WASM_PATH");

/// `LanguageOptions` service and method ids (see `spike/wazero.go`).
const SVC_LANGUAGE_OPTIONS: i32 = 678;
const MID_NEW_LANGUAGE_OPTIONS: i32 = 0;
const MID_ENABLE_MAXIMUM_LANGUAGE_FEATURES: i32 = 7;
const MID_FREE_LANGUAGE_OPTIONS: i32 = 29;

/// Process-wide cache of the compiled wasm module and its `Engine`.
///
/// JIT-compiling the ~13MB module dominates the cost of [`Module::new`]. wasmtime's
/// `Engine` and `Module` are `Send + Sync` and designed to be shared across many
/// instances and threads, so we compile once here and only instantiate per `Module`.
/// The immutable compiled code is shared; each `Module` still gets its own `Store`,
/// `Instance`, and linear memory, so instances stay fully isolated.
static SHARED_WASM: OnceLock<Result<(Engine, WasmModule), String>> = OnceLock::new();

/// Returns the shared `Engine` and compiled module, compiling on first use.
///
/// Uses `get_or_init` so the expensive compilation runs exactly once even when
/// many threads construct a `Module` concurrently; the losers block rather than
/// each recompiling. A compilation failure is cached (the embedded wasm is fixed,
/// so it would fail identically every time).
fn shared_wasm() -> Result<&'static (Engine, WasmModule), Error> {
    SHARED_WASM
        .get_or_init(|| {
            let engine = Engine::default();
            let wasm = WasmModule::from_file(&engine, WASM_PATH)
                .map_err(|e| format!("load {WASM_PATH}: {e}"))?;
            Ok((engine, wasm))
        })
        .as_ref()
        .map_err(|e| Error::Instantiate(e.clone()))
}

/// Host state carried in the wasmtime `Store`.
struct HostState {
    wasi: WasiP1Ctx,
}

/// A single instance of the GoogleSQL wasm module.
///
/// wasmtime's `Store` requires exclusive access (`&mut`), so every method takes
/// `&mut self`, serializing all calls through one instance. A `Module` is
/// [`Send`], so it can be moved between threads and, since each instance owns an
/// isolated wasm linear memory, many instances (one per thread) run truly in
/// parallel. wasmtime forbids concurrent calls into a single instance, so a
/// `Module` is deliberately not `Sync`: parallelism comes from separate
/// instances, not from sharing one.
pub struct Module {
    store: Store<HostState>,
    memory: Memory,
    alloc_fn: TypedFunc<u32, u32>,
    free_fn: TypedFunc<u32, ()>,
    instance: Instance,
    /// Deferred frees enqueued by dropped [`Handle`]s, drained by
    /// [`Module::flush_frees`]. Shared with each live [`Handle`] via `Arc` (not
    /// `Rc`) so the whole `Module` stays [`Send`]; the `Mutex` is only ever
    /// taken by the single owning thread, so it is uncontended.
    pending_frees: Arc<Mutex<Vec<PendingFree>>>,
    /// Cache of `w_<svc>_<mid>` RPC exports, keyed by `(svc, mid)`.
    ///
    /// Resolving an export is a by-name lookup plus a type check; the tree-walking
    /// APIs make thousands of RPC calls, so caching the resolved [`TypedFunc`]
    /// removes that per-call cost (and the per-call name formatting) after the
    /// first use. `TypedFunc` is a cheap `Copy` handle independent of the store.
    invoke_cache: HashMap<(i32, i32), TypedFunc<(u32, u32), u64>>,
    /// Cache of named exports (e.g. `wasmify_get_type_name`), keyed by name, for
    /// the same reason as [`Module::invoke_cache`].
    export_cache: HashMap<String, TypedFunc<(u32, u32), u64>>,
}

/// Identifies the export a [`Module::call_typed`] call targets, used only to
/// build the call-failure message. Formatting is deferred to the error path so
/// the cached-lookup hot path never allocates the export name.
enum ExportId<'a> {
    /// A `w_<svc>_<mid>` RPC export.
    Rpc { svc: i32, mid: i32 },
    /// A named export such as `wasmify_get_type_name`.
    Named(&'a str),
}

impl std::fmt::Display for ExportId<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Rpc { svc, mid } => write!(f, "w_{svc}_{mid}"),
            Self::Named(name) => f.write_str(name),
        }
    }
}

/// A wasm-side handle free deferred until [`Module::flush_frees`]: the
/// `w_<svc>_<mid>` free RPC to invoke with `ptr`.
#[derive(Clone, Copy)]
struct PendingFree {
    svc: i32,
    mid: i32,
    ptr: u64,
}

/// An RAII guard over a host-owned wasm-side C++ handle.
///
/// On drop it enqueues its free RPC into the owning [`Module`]'s queue rather
/// than freeing eagerly: releasing a handle needs `&mut Store`, which a `Drop`
/// impl cannot obtain. [`Module::flush_frees`] performs the deferred frees later.
/// Deferring lets handles (including nested ones) be created and dropped without
/// threading `&mut Module` through the guard.
pub struct Handle {
    ptr: u64,
    free_svc: i32,
    free_mid: i32,
    queue: Arc<Mutex<Vec<PendingFree>>>,
}

impl Handle {
    /// The wasm-side handle pointer, valid until [`Module::flush_frees`] runs.
    pub(crate) const fn ptr(&self) -> u64 {
        self.ptr
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // The lock is only ever taken by the single owning thread (here and in
        // `flush_frees`), so it is uncontended; recover from poisoning rather
        // than panic in `drop`, so a free is never silently dropped.
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(PendingFree {
                svc: self.free_svc,
                mid: self.free_mid,
                ptr: self.ptr,
            });
    }
}

impl Module {
    /// Loads the embedded wasm and returns a fully initialized instance.
    pub fn new() -> Result<Self, Error> {
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
            pending_frees: Arc::new(Mutex::new(Vec::new())),
            invoke_cache: HashMap::new(),
            export_cache: HashMap::new(),
        })
    }

    /// Runs `f`, then frees every handle it acquired via a single flush, whether
    /// `f` succeeded or failed.
    ///
    /// Handles dropped inside `f` enqueue their frees as its frames unwind; the
    /// flush here releases them all, preserving the drop (child-before-parent)
    /// order. This is the one place handle cleanup happens, so any handle must be
    /// acquired within a `with_frees` scope or it leaks. On error the work error
    /// takes priority over a flush error.
    pub(crate) fn with_frees<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let result = f(self);
        let freed = self.flush_frees();
        let value = result?;
        freed?;
        Ok(value)
    }

    /// Invokes a constructor RPC and returns its non-null handle from response
    /// field 1, without registering a free.
    ///
    /// Use for handles whose ownership transfers into the wasm side (e.g. a
    /// `SimpleColumn` adopted by a `SimpleTable`), which the host must not free.
    /// Returns [`Error::GoogleSql`] if the response carries a GoogleSQL error, or
    /// [`Error::Protocol`] if the constructor yields a null handle.
    pub(crate) fn new_handle(&mut self, svc: i32, mid: i32, req: &[u8]) -> Result<u64, Error> {
        let resp = self.invoke(svc, mid, req)?;
        if let Some(message) = pb::extract_error(&resp) {
            return Err(Error::GoogleSql(message.into()));
        }
        let ptr = pb::read_handle_at_field(&resp, 1);
        if ptr == 0 {
            return Err(Error::Protocol(format!(
                "constructor w_{svc}_{mid} returned null"
            )));
        }
        Ok(ptr)
    }

    /// Invokes a constructor RPC and returns an RAII [`Handle`] that, once
    /// dropped, defers freeing the resulting wasm-side handle via
    /// `w_<free_svc>_<free_mid>` (run by the enclosing [`with_frees`](Module::with_frees)).
    ///
    /// Returns [`Error::GoogleSql`] if the response carries a GoogleSQL error, or
    /// [`Error::Protocol`] if the constructor yields a null handle.
    pub(crate) fn acquire_handle(
        &mut self,
        new_svc: i32,
        new_mid: i32,
        req: &[u8],
        free_svc: i32,
        free_mid: i32,
    ) -> Result<Handle, Error> {
        let ptr = self.new_handle(new_svc, new_mid, req)?;
        Ok(self.register_free(free_svc, free_mid, ptr))
    }

    /// Wraps an already-obtained wasm-side handle `ptr` in an RAII [`Handle`]
    /// that defers freeing it via `w_<free_svc>_<free_mid>`.
    ///
    /// Use this for handles returned by non-constructor RPCs (e.g. the
    /// `ParserOutput`/`AnalyzerOutput` a `Parse`/`Analyze` call yields), where
    /// [`acquire_handle`](Module::acquire_handle) does not apply.
    pub(crate) fn register_free(&self, free_svc: i32, free_mid: i32, ptr: u64) -> Handle {
        Handle {
            ptr,
            free_svc,
            free_mid,
            queue: Arc::clone(&self.pending_frees),
        }
    }

    /// Acquires a `LanguageOptions` handle with the maximum released language
    /// feature set enabled.
    ///
    /// GoogleSQL gates optional syntax (e.g. the `QUALIFY` clause) behind
    /// language features that default `LanguageOptions` leaves off. The parser
    /// and analyzer both wire the returned handle into their options so those
    /// features are accepted. The handle is an RAII [`Handle`] freed by the
    /// enclosing [`with_frees`](Module::with_frees).
    pub(crate) fn acquire_max_language_options(&mut self) -> Result<Handle, Error> {
        let language = self.acquire_handle(
            SVC_LANGUAGE_OPTIONS,
            MID_NEW_LANGUAGE_OPTIONS,
            &[],
            SVC_LANGUAGE_OPTIONS,
            MID_FREE_LANGUAGE_OPTIONS,
        )?;
        let resp = self.invoke(
            SVC_LANGUAGE_OPTIONS,
            MID_ENABLE_MAXIMUM_LANGUAGE_FEATURES,
            &pb::handle_arg(language.ptr()),
        )?;
        crate::error::check_error(&resp)?;
        Ok(language)
    }

    /// Runs every free enqueued by a dropped [`Handle`], returning the first
    /// error after attempting all of them so a single failure cannot strand the
    /// remaining handles.
    fn flush_frees(&mut self) -> Result<(), Error> {
        // Drain into a local Vec first: `invoke` needs `&mut self`, so the
        // `pending_frees` lock cannot be held across the loop.
        let pending: Vec<PendingFree> = self
            .pending_frees
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
            .collect();
        let mut first_error = None;
        for free in pending {
            let freed = self.invoke(free.svc, free.mid, &pb::handle_arg(free.ptr));
            if let (Err(e), None) = (freed, &first_error) {
                first_error = Some(e);
            }
        }
        first_error.map_or(Ok(()), Err)
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
        let func = match self.invoke_cache.get(&(svc, mid)) {
            Some(func) => func.clone(),
            None => {
                let name = format!("w_{svc}_{mid}");
                let func = self.typed_export(&name)?;
                self.invoke_cache.insert((svc, mid), func.clone());
                func
            }
        };
        self.call_typed(func, ExportId::Rpc { svc, mid }, req)
    }

    /// Calls a named export (`w_<svc>_<mid>` or `wasmify_get_type_name`, etc.)
    /// using the `(ptr,len) -> (ptr<<32 | len)` convention.
    pub fn call_export(&mut self, name: &str, req: &[u8]) -> Result<Vec<u8>, Error> {
        let func = match self.export_cache.get(name) {
            Some(func) => func.clone(),
            None => {
                let func = self.typed_export(name)?;
                self.export_cache.insert(name.to_string(), func.clone());
                func
            }
        };
        self.call_typed(func, ExportId::Named(name), req)
    }

    /// Resolves an export by name into a typed `(ptr,len) -> packed` function.
    fn typed_export(&mut self, name: &str) -> Result<TypedFunc<(u32, u32), u64>, Error> {
        self.instance
            .get_typed_func::<(u32, u32), u64>(&mut self.store, name)
            .map_err(|e| Error::Wasm(format!("export `{name}`: {e}")))
    }

    /// Runs one RPC over an already-resolved export: writes `req` into wasm
    /// memory, calls the export, and returns the response bytes. Both the request
    /// and response buffers are freed after the call. `id` names the export only
    /// for the call-failure message, so it is formatted lazily off the hot path.
    fn call_typed(
        &mut self,
        func: TypedFunc<(u32, u32), u64>,
        id: ExportId<'_>,
        req: &[u8],
    ) -> Result<Vec<u8>, Error> {
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
            .map_err(|e| Error::Wasm(format!("`{id}` call: {e}")))?;

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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test code")]
mod tests {
    // A `ParserOptions` handle (NewParserOptions = svc699/mid0, freed by mid12)
    // is a convenient real handle to exercise the deferred-free machinery.
    const SVC_PARSER_OPTIONS: i32 = 699;
    const MID_NEW_PARSER_OPTIONS: i32 = 0;
    const MID_FREE_PARSER_OPTIONS: i32 = 12;

    /// A `Handle` does not free its wasm-side handle eagerly: dropping it only
    /// enqueues the free, which `flush_frees` later performs.
    #[test]
    fn handle_defers_free_until_flush() {
        let mut module = super::Module::new().expect("instantiate module");

        {
            let handle = module
                .acquire_handle(
                    SVC_PARSER_OPTIONS,
                    MID_NEW_PARSER_OPTIONS,
                    &[],
                    SVC_PARSER_OPTIONS,
                    MID_FREE_PARSER_OPTIONS,
                )
                .expect("acquire ParserOptions handle");
            assert_ne!(handle.ptr(), 0, "handle pointer must be non-null");
            assert_eq!(
                module
                    .pending_frees
                    .lock()
                    .unwrap_or_else(super::PoisonError::into_inner)
                    .len(),
                0,
                "nothing may be queued while the handle is still alive"
            );
        }

        assert_eq!(
            module
                .pending_frees
                .lock()
                .unwrap_or_else(super::PoisonError::into_inner)
                .len(),
            1,
            "dropping the handle must enqueue exactly one deferred free"
        );

        module.flush_frees().expect("flush frees");
        assert_eq!(
            module
                .pending_frees
                .lock()
                .unwrap_or_else(super::PoisonError::into_inner)
                .len(),
            0,
            "flush must drain the queue and run every free"
        );
    }

    /// Repeated RPCs to the same `(svc, mid)` resolve the export exactly once:
    /// the first `invoke` populates the cache and later calls reuse the cached
    /// `TypedFunc` instead of re-running `get_typed_func`.
    #[test]
    fn invoke_caches_typed_export_per_svc_mid() {
        let mut module = super::Module::new().expect("instantiate module");

        assert_eq!(
            module.invoke_cache.len(),
            0,
            "cache must start empty before any RPC"
        );

        let mut handle = 0;
        for _ in 0..3 {
            let resp = module
                .invoke(SVC_PARSER_OPTIONS, MID_NEW_PARSER_OPTIONS, &[])
                .expect("NewParserOptions RPC");
            handle = super::pb::read_handle_at_field(&resp, 1);
        }

        assert_eq!(
            module.invoke_cache.len(),
            1,
            "three calls to one RPC must leave a single cached export"
        );

        // A distinct RPC adds exactly one more entry rather than replacing it;
        // free the last handle acquired above so the instance is left clean.
        module
            .invoke(
                SVC_PARSER_OPTIONS,
                MID_FREE_PARSER_OPTIONS,
                &super::pb::handle_arg(handle),
            )
            .expect("FreeParserOptions RPC");
        assert_eq!(
            module.invoke_cache.len(),
            2,
            "a different (svc, mid) must add its own cache entry"
        );
    }
}
