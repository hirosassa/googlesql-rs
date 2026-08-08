//! The engine-agnostic [`Module`]: the GoogleSQL bindings and the RPC
//! marshaling that drive a [`GuestInstance`].
//!
//! `Module` owns the parser/formatter/analyzer bindings and the shared plumbing
//! (request encoding, the persistent wasm request region, packed-response
//! decoding, and deferred handle frees) written once against the engine-agnostic
//! ABI. The wasm engine itself — wasmtime, plus WASI and the C++ `env` stubs —
//! lives in [`wasmtime_backend`](crate::wasmtime_backend); the RPC convention it
//! drives is described on [`backend`](crate::backend) (ABI specifics in
//! `docs/SPIKE.md`).

use std::sync::{Arc, Mutex, PoisonError};

use crate::backend::GuestInstance;
use crate::error::Error;
use crate::pb;

/// `LanguageOptions` service and method ids (see `spike/wazero.go`).
const SVC_LANGUAGE_OPTIONS: i32 = 678;
const MID_NEW_LANGUAGE_OPTIONS: i32 = 0;
const MID_ENABLE_MAXIMUM_LANGUAGE_FEATURES: i32 = 7;
const MID_SET_SUPPORTS_ALL_STATEMENT_KINDS: i32 = 20;
const MID_ADD_SUPPORTED_STATEMENT_KIND: i32 = 2;
const MID_DISABLE_LANGUAGE_FEATURE: i32 = 4;
const MID_DISABLE_ALL_LANGUAGE_FEATURES: i32 = 3;
const MID_ENABLE_LANGUAGE_FEATURE: i32 = 6;
const MID_SET_PRODUCT_MODE: i32 = 28;

/// `ProductMode::PRODUCT_INTERNAL`: the default type surface (e.g. `DOUBLE`,
/// `INT64`). GoogleSQL numbers its `ProductMode` enum from 1, so this is 1, not 0.
/// The public [`ProductMode`](crate::ProductMode) enum owns the wire mapping;
/// this is only the default the `Module` starts with.
const PRODUCT_MODE_INTERNAL: i32 = 1;

/// How the analyzer's optional [`LanguageFeature`](crate::LanguageFeature) set
/// is built for [`Module::language_options`].
///
/// The two variants are the two opposite defaults ZetaSQL offers: start from
/// every feature on and switch some off, or start from every feature off and
/// switch some on.
#[derive(Debug, Clone)]
enum LanguageFeatureMode {
    /// `EnableMaximumLanguageFeatures`, then `DisableLanguageFeature` for each
    /// listed wire value. The default, with an empty list, keeps every feature.
    Maximum(Vec<i32>),
    /// `DisableAllLanguageFeatures`, then `EnableLanguageFeature` for each listed
    /// wire value. An empty list leaves every optional feature off.
    Minimal(Vec<i32>),
}

/// A single instance of the GoogleSQL guest, the entry point to the parser,
/// formatter, and analyzer.
///
/// Each `Module` owns one `GuestInstance`, which requires exclusive access, so
/// every method takes `&mut self`,
/// serializing all calls through the one instance. A `Module` is [`Send`], so it
/// can be moved between threads and, since each instance owns an isolated wasm
/// linear memory, many instances (one per thread) run truly in parallel. The
/// engine forbids concurrent calls into a single instance, so a `Module` is
/// deliberately not `Sync`: parallelism comes from separate instances, not from
/// sharing one.
pub struct Module {
    /// The wasm engine that executes the GoogleSQL guest. Boxed as a trait
    /// object so the default wasmtime engine and a future ahead-of-time-compiled
    /// engine share the identical `Module` above them; the indirection is one
    /// vtable hop per RPC, negligible beside the guest call itself.
    backend: Box<dyn GuestInstance>,
    /// Deferred frees enqueued by dropped [`Handle`]s, drained by
    /// [`Module::flush_frees`]. Shared with each live [`Handle`] via `Arc` (not
    /// `Rc`) so the whole `Module` stays [`Send`]; the `Mutex` is only ever
    /// taken by the single owning thread, so it is uncontended.
    pending_frees: Arc<Mutex<Vec<PendingFree>>>,
    /// Cached `LanguageOptions` handle, built on first use by
    /// [`Module::language_options`] and reused for the `Module`'s lifetime.
    ///
    /// The handle is immutable configuration every parse and analyze call wires
    /// into its options, so building it once removes the two-RPC reconstruction
    /// each call otherwise paid. It is intentionally not registered for deferred
    /// free: the wasm-side allocation is reclaimed when the `Store` is dropped.
    cached_language_options: Option<u64>,
    /// `ResolvedNodeKind` wire values the analyzer is restricted to, or empty to
    /// accept every kind (the default). Set by
    /// [`Module::set_supported_statement_kinds`](crate::Module::set_supported_statement_kinds)
    /// and applied when [`Module::language_options`] (re)builds its handle.
    supported_statement_kinds: Vec<i32>,
    /// How the optional `LanguageFeature` set is built: the maximum set minus a
    /// disabled list (the default) or the minimal set plus an enabled list. Set
    /// by [`Module::disable_language_features`](crate::Module::disable_language_features)
    /// and [`Module::enable_only_language_features`](crate::Module::enable_only_language_features),
    /// and applied when [`Module::language_options`] (re)builds its handle.
    language_feature_mode: LanguageFeatureMode,
    /// The `ProductMode` wire value (INTERNAL by default) applied when
    /// [`Module::language_options`] (re)builds its handle, and used to render
    /// resolved type names. Set by
    /// [`Module::set_product_mode`](crate::Module::set_product_mode).
    product_mode: i32,
    /// The `DescriptorPool` handle in effect for the current analysis, or `None`
    /// when no proto descriptors are registered.
    ///
    /// Proto type resolution needs the pool built from a catalog's `proto_files`
    /// available deep in the type-building recursion (`build_column_type`), far
    /// from where the pool is created. Rather than thread it through every
    /// type-building signature, the analysis pipeline sets it for the duration of
    /// a single populate-and-analyze and clears it at each entry. The pool's own
    /// lifetime is owned by an RAII `Handle` kept alive across the analysis; this
    /// field only borrows its pointer, so it must never be read outside that
    /// window.
    pub(crate) descriptor_pool: Option<u64>,
    /// Reusable request-encoding buffer for hot-path RPCs.
    ///
    /// The AST and resolved-tree walks issue thousands of small RPCs, most of
    /// them a single handle argument. Encoding each request into this retained
    /// buffer (via [`Module::invoke_handle`] and friends) rather than a fresh
    /// `Vec` removes a per-call allocation; the buffer keeps its capacity across
    /// calls. Only ever touched by the single owning thread between calls.
    scratch: Vec<u8>,
    /// Persistent wasm-side (linear-memory) region every RPC encodes its request
    /// into, and its capacity in bytes. Complements [`Module::scratch`]: that
    /// removes the host `Vec` allocation, this removes the wasm-side
    /// `wasm_alloc`/`wasm_free` request pair from every call. Grown by
    /// [`Module::wasm_request_scratch`] only when a request exceeds the current
    /// capacity (monotonically, so it settles at the largest request seen and
    /// then reallocates no more), and reclaimed with the `Store`. `0`/`0` means
    /// not yet allocated.
    req_scratch_ptr: u32,
    req_scratch_cap: u32,
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
    /// Loads the prebuilt wasm on the default engine and returns a fully
    /// initialized instance.
    pub fn new() -> Result<Self, Error> {
        Self::from_backend(Box::new(crate::wasmtime_backend::WasmtimeInstance::new()?))
    }

    /// Builds a `Module` around an already-initialized engine backend.
    ///
    /// The single seam through which an engine is injected: `new` supplies the
    /// wasmtime backend today, and the forthcoming wasm2rs native engine will
    /// plug in here behind the same [`GuestInstance`] trait without touching any
    /// of the state below.
    fn from_backend(backend: Box<dyn GuestInstance>) -> Result<Self, Error> {
        Ok(Self {
            backend,
            pending_frees: Arc::new(Mutex::new(Vec::new())),
            cached_language_options: None,
            descriptor_pool: None,
            supported_statement_kinds: Vec::new(),
            language_feature_mode: LanguageFeatureMode::Maximum(Vec::new()),
            product_mode: PRODUCT_MODE_INTERNAL,
            scratch: Vec::new(),
            req_scratch_ptr: 0,
            req_scratch_cap: 0,
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

    /// Returns a cached `LanguageOptions` handle configured with the requested
    /// feature set and all statement kinds accepted (unless restricted), building
    /// it on first use.
    ///
    /// GoogleSQL gates optional syntax (e.g. the `QUALIFY` clause) behind
    /// language features that default `LanguageOptions` leaves off, and its
    /// analyzer resolves only query statements unless told otherwise. By default
    /// the parser and analyzer both wire in the maximum released feature set and
    /// every statement kind; callers can instead disable features on top of the
    /// maximum, enable only a chosen few on top of the minimal set, or restrict
    /// the statement kinds. The configuration is immutable between changes, so
    /// the handle is built once and cached for the `Module`'s lifetime rather
    /// than reconstructed per parse/analyze; each setter invalidates the cache.
    /// It is deliberately not registered for deferred free: the parser/analyzer
    /// option setters copy it rather than adopt it, and its wasm-side allocation
    /// is reclaimed when the `Store` is dropped with the instance.
    pub(crate) fn language_options(&mut self) -> Result<u64, Error> {
        if let Some(ptr) = self.cached_language_options {
            return Ok(ptr);
        }
        let ptr = self.new_handle(SVC_LANGUAGE_OPTIONS, MID_NEW_LANGUAGE_OPTIONS, &[])?;
        // Establish the optional feature set: either the maximum minus a disabled
        // list, or the minimal set plus an enabled list. Clone the small mode so
        // the `&mut self` invokes below do not borrow it during iteration.
        match self.language_feature_mode.clone() {
            LanguageFeatureMode::Maximum(disabled) => {
                let resp = self.invoke(
                    SVC_LANGUAGE_OPTIONS,
                    MID_ENABLE_MAXIMUM_LANGUAGE_FEATURES,
                    &pb::handle_arg(ptr),
                )?;
                crate::error::check_error(&resp)?;
                // Turn off the features the caller disabled so syntax gated
                // behind them fails to resolve.
                for feature in disabled {
                    let mut req = Vec::new();
                    pb::append_handle(&mut req, 1, ptr);
                    pb::append_int32(&mut req, 2, feature);
                    let resp =
                        self.invoke(SVC_LANGUAGE_OPTIONS, MID_DISABLE_LANGUAGE_FEATURE, &req)?;
                    crate::error::check_error(&resp)?;
                }
            }
            LanguageFeatureMode::Minimal(enabled) => {
                let resp = self.invoke(
                    SVC_LANGUAGE_OPTIONS,
                    MID_DISABLE_ALL_LANGUAGE_FEATURES,
                    &pb::handle_arg(ptr),
                )?;
                crate::error::check_error(&resp)?;
                // Turn on only the features the caller asked for; every other
                // optional feature stays off.
                for feature in enabled {
                    let mut req = Vec::new();
                    pb::append_handle(&mut req, 1, ptr);
                    pb::append_int32(&mut req, 2, feature);
                    let resp =
                        self.invoke(SVC_LANGUAGE_OPTIONS, MID_ENABLE_LANGUAGE_FEATURE, &req)?;
                    crate::error::check_error(&resp)?;
                }
            }
        }
        if self.supported_statement_kinds.is_empty() {
            // Accept every statement kind (DML, DDL, script control), not just
            // query. Without this the analyzer rejects e.g. INSERT/CREATE TABLE
            // with "Statement not supported"; the parser is unaffected since it
            // never restricts statement kinds. Equivalent to passing an empty set
            // to SetSupportedStatementKinds.
            let resp = self.invoke(
                SVC_LANGUAGE_OPTIONS,
                MID_SET_SUPPORTS_ALL_STATEMENT_KINDS,
                &pb::handle_arg(ptr),
            )?;
            crate::error::check_error(&resp)?;
        } else {
            // Adding kinds leaves the supported set non-empty, so only those
            // resolve and every other kind is rejected. Clone the small kind list
            // so the `&mut self` invoke does not borrow it during iteration.
            for kind in self.supported_statement_kinds.clone() {
                let mut req = Vec::new();
                pb::append_handle(&mut req, 1, ptr);
                pb::append_int32(&mut req, 2, kind);
                let resp =
                    self.invoke(SVC_LANGUAGE_OPTIONS, MID_ADD_SUPPORTED_STATEMENT_KIND, &req)?;
                crate::error::check_error(&resp)?;
            }
        }
        // Select the type surface (INTERNAL vs EXTERNAL/BigQuery). A fresh
        // LanguageOptions already defaults to INTERNAL, but set it explicitly so
        // the field is the single source of truth.
        let mut mode_req = Vec::new();
        pb::append_handle(&mut mode_req, 1, ptr);
        pb::append_int32(&mut mode_req, 2, self.product_mode);
        let resp = self.invoke(SVC_LANGUAGE_OPTIONS, MID_SET_PRODUCT_MODE, &mode_req)?;
        crate::error::check_error(&resp)?;

        self.cached_language_options = Some(ptr);
        Ok(ptr)
    }

    /// The current `ProductMode` wire value, used when rendering resolved type
    /// names so they match the configured type surface.
    pub(crate) const fn product_mode(&self) -> i32 {
        self.product_mode
    }

    /// Sets the `ProductMode` wire value and invalidates the cached
    /// [`LanguageOptions`](Self::language_options) handle so the next analysis
    /// rebuilds it with the new mode; the stale handle is reclaimed with the
    /// `Store`, matching the cache's existing no-free policy.
    pub(crate) const fn set_product_mode_raw(&mut self, mode: i32) {
        self.product_mode = mode;
        self.cached_language_options = None;
    }

    /// Restricts the analyzer to the given `ResolvedNodeKind` wire values, or
    /// restores "accept every kind" when `kinds` is empty. Invalidates the cached
    /// [`LanguageOptions`](Self::language_options) handle so the next analysis
    /// rebuilds it with the new restriction; the stale handle is reclaimed with
    /// the `Store`, matching the cache's existing no-free policy.
    pub(crate) fn set_supported_statement_kinds_raw(&mut self, kinds: Vec<i32>) {
        self.supported_statement_kinds = kinds;
        self.cached_language_options = None;
    }

    /// Builds the optional feature set as the maximum minus the given
    /// `LanguageFeature` wire values, or restores the full set when `features` is
    /// empty. Invalidates the cached [`LanguageOptions`](Self::language_options)
    /// handle so the next analysis rebuilds it, matching the no-free policy above.
    pub(crate) fn set_disabled_language_features_raw(&mut self, features: Vec<i32>) {
        self.language_feature_mode = LanguageFeatureMode::Maximum(features);
        self.cached_language_options = None;
    }

    /// Builds the optional feature set as the minimal set plus only the given
    /// `LanguageFeature` wire values; an empty list leaves every optional feature
    /// off. Invalidates the cached [`LanguageOptions`](Self::language_options)
    /// handle so the next analysis rebuilds it, matching the no-free policy above.
    pub(crate) fn set_enabled_language_features_raw(&mut self, features: Vec<i32>) {
        self.language_feature_mode = LanguageFeatureMode::Minimal(features);
        self.cached_language_options = None;
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
        self.backend.alloc(len)
    }

    /// Frees a pointer previously returned by `alloc`.
    pub fn free(&mut self, ptr: u32) -> Result<(), Error> {
        self.backend.free(ptr)
    }

    /// Writes a byte slice into wasm memory at `ptr`.
    pub fn write(&mut self, ptr: u32, data: &[u8]) -> Result<(), Error> {
        self.backend.write(ptr, data)
    }

    /// Reads `len` bytes from wasm memory starting at `ptr` and returns them as a `Vec<u8>`.
    pub fn read(&mut self, ptr: u32, len: u32) -> Result<Vec<u8>, Error> {
        self.backend.read(ptr, len)
    }

    /// Invokes a wasmify RPC following the `w_<svc>_<mid>` convention: encodes
    /// `req` into the reused request region, calls the export via the backend, and
    /// decodes the packed response, freeing the wasm-side response buffer.
    pub fn invoke(&mut self, svc: i32, mid: i32, req: &[u8]) -> Result<Vec<u8>, Error> {
        let (req_ptr, req_len) = self.prepare_request(req)?;
        let packed = self.backend.call_rpc(svc, mid, req_ptr, req_len)?;
        self.finish_response(packed)
    }

    /// Calls a named export (`wasmify_get_type_name`, etc.), the by-name
    /// counterpart to [`invoke`](Module::invoke).
    pub fn call_export(&mut self, name: &str, req: &[u8]) -> Result<Vec<u8>, Error> {
        let (req_ptr, req_len) = self.prepare_request(req)?;
        let packed = self.backend.call_named(name, req_ptr, req_len)?;
        self.finish_response(packed)
    }

    /// Encodes `req` into the persistent wasm request region for a call and
    /// returns the `(ptr, len)` pair to pass to the export, or `(0, 0)` for an
    /// empty request. The region is reused across calls (see
    /// [`wasm_request_scratch`](Module::wasm_request_scratch)); only its growth
    /// touches the wasm allocator.
    fn prepare_request(&mut self, req: &[u8]) -> Result<(u32, u32), Error> {
        if req.is_empty() {
            return Ok((0, 0));
        }
        let len = u32::try_from(req.len()).map_err(|e| Error::Memory(e.to_string()))?;
        let ptr = self.wasm_request_scratch(len)?;
        self.backend.write(ptr, req)?;
        Ok((ptr, len))
    }

    /// Decodes a packed `(ptr<<32 | len)` response: reads the response bytes and
    /// frees the wasm-side response buffer the export allocated. An empty response
    /// (`len == 0`) neither reads nor frees.
    fn finish_response(&mut self, packed: u64) -> Result<Vec<u8>, Error> {
        let resp_ptr = u32::try_from(packed >> 32).map_err(|e| Error::Memory(e.to_string()))?;
        let resp_len =
            u32::try_from(packed & 0xFFFF_FFFF).map_err(|e| Error::Memory(e.to_string()))?;
        if resp_len == 0 {
            return Ok(Vec::new());
        }
        let resp = self.backend.read(resp_ptr, resp_len)?;
        self.backend.free(resp_ptr)?;
        Ok(resp)
    }

    /// Encodes a request into the reused [`scratch`](Module::scratch) buffer via
    /// `build`, runs `call` with it, and returns the buffer to `self` afterwards
    /// (on success or failure) so its capacity is kept for the next call.
    ///
    /// This is the single home of the scratch-reuse dance shared by
    /// [`invoke_encoded`](Module::invoke_encoded) and
    /// [`call_export_encoded`](Module::call_export_encoded): the buffer is taken
    /// out so `call` can borrow `self` mutably, and `build` must only append to it
    /// (it is cleared first). Used by the tree-walk hot paths to avoid a fresh
    /// per-call request allocation.
    fn with_scratch(
        &mut self,
        build: impl FnOnce(&mut Vec<u8>),
        call: impl FnOnce(&mut Self, &[u8]) -> Result<Vec<u8>, Error>,
    ) -> Result<Vec<u8>, Error> {
        let mut buf = std::mem::take(&mut self.scratch);
        buf.clear();
        build(&mut buf);
        let resp = call(self, &buf);
        self.scratch = buf;
        resp
    }

    /// Invokes a wasmify RPC whose request is encoded into the reused scratch
    /// buffer by `build` (see [`with_scratch`](Module::with_scratch)).
    pub(crate) fn invoke_encoded(
        &mut self,
        svc: i32,
        mid: i32,
        build: impl FnOnce(&mut Vec<u8>),
    ) -> Result<Vec<u8>, Error> {
        self.with_scratch(build, |m, buf| m.invoke(svc, mid, buf))
    }

    /// Invokes a single-handle RPC (`field 1 = ptr`) over the reused scratch
    /// buffer — the dominant request shape in the AST and resolved-tree walks.
    pub(crate) fn invoke_handle(&mut self, svc: i32, mid: i32, ptr: u64) -> Result<Vec<u8>, Error> {
        self.invoke_encoded(svc, mid, |buf| pb::append_handle(buf, 1, ptr))
    }

    /// Calls a named export whose request is encoded into the reused scratch
    /// buffer, the [`invoke_encoded`](Module::invoke_encoded) counterpart for
    /// named exports such as `wasmify_get_type_name`.
    pub(crate) fn call_export_encoded(
        &mut self,
        name: &str,
        build: impl FnOnce(&mut Vec<u8>),
    ) -> Result<Vec<u8>, Error> {
        self.with_scratch(build, |m, buf| m.call_export(name, buf))
    }

    /// Returns the persistent wasm-side request region ([`Module::req_scratch_ptr`]),
    /// ensuring it holds at least `len` bytes.
    ///
    /// When the current region already covers `len` it is returned unchanged, so
    /// the common case pays no wasm call at all. Otherwise the old region (if any)
    /// is freed and a new one of exactly `len` bytes is allocated; since the
    /// region only ever grows, it converges on the largest request the instance
    /// encounters and then reallocates no further. `len` must be non-zero (the
    /// empty-request path never calls this).
    fn wasm_request_scratch(&mut self, len: u32) -> Result<u32, Error> {
        if self.req_scratch_ptr != 0 && self.req_scratch_cap >= len {
            return Ok(self.req_scratch_ptr);
        }
        // Grow: release the undersized region before allocating a larger one so
        // the wasm allocator can coalesce it, then commit the new region only
        // once both steps succeed.
        if self.req_scratch_ptr != 0 {
            self.free(self.req_scratch_ptr)?;
            self.req_scratch_ptr = 0;
            self.req_scratch_cap = 0;
        }
        let ptr = self.alloc(len)?;
        if ptr == 0 {
            return Err(Error::Wasm("wasm_alloc returned NULL".into()));
        }
        self.req_scratch_ptr = ptr;
        self.req_scratch_cap = len;
        Ok(ptr)
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

    /// The maximum-feature `LanguageOptions` handle is built once and cached:
    /// the cache starts empty, the first call populates it with a non-null
    /// handle, and a second call returns that same handle without rebuilding.
    #[test]
    fn language_options_is_cached() {
        let mut module = super::Module::new().expect("instantiate module");
        assert_eq!(
            module.cached_language_options, None,
            "cache must start empty before any call"
        );

        let first = module.language_options().expect("build language options");
        assert_ne!(first, 0, "language options handle must be non-null");
        assert_eq!(
            module.cached_language_options,
            Some(first),
            "the first call must populate the cache"
        );

        let second = module.language_options().expect("reuse language options");
        assert_eq!(
            first, second,
            "the second call must return the cached handle, not a fresh one"
        );
    }

    /// `invoke_handle` drives a real single-handle RPC through the reused
    /// scratch buffer and returns the buffer to the `Module` afterwards, keeping
    /// its capacity for the next call rather than leaving the empty buffer that
    /// `mem::take` installed. (The buffer is cleared before each build, so a
    /// retained capacity never corrupts a later, shorter request.)
    #[test]
    fn invoke_handle_reuses_scratch_buffer() {
        let mut module = super::Module::new().expect("instantiate module");
        assert_eq!(
            module.scratch.capacity(),
            0,
            "scratch must start unallocated"
        );

        let resp = module
            .invoke(SVC_PARSER_OPTIONS, MID_NEW_PARSER_OPTIONS, &[])
            .expect("NewParserOptions RPC");
        let handle = super::pb::read_handle_at_field(&resp, 1);
        assert_ne!(handle, 0, "NewParserOptions must return a non-null handle");

        // Free it back through the single-handle helper under test.
        module
            .invoke_handle(SVC_PARSER_OPTIONS, MID_FREE_PARSER_OPTIONS, handle)
            .expect("FreeParserOptions via invoke_handle");
        assert!(
            module.scratch.capacity() > 0,
            "the scratch buffer must be returned to the Module with capacity to reuse"
        );
    }

    /// The persistent wasm-side request region grows to fit the largest request
    /// and never shrinks: a first size allocates it, a larger size reallocates
    /// (new pointer, larger capacity), and a smaller size afterwards reuses the
    /// same region rather than allocating again.
    #[test]
    fn wasm_request_scratch_grows_monotonically() {
        let mut module = super::Module::new().expect("instantiate module");
        assert_eq!(
            module.req_scratch_ptr, 0,
            "the wasm request region must start unallocated"
        );

        let small = module
            .wasm_request_scratch(16)
            .expect("allocate small request region");
        assert_ne!(small, 0, "a non-empty request must yield a non-null region");
        assert!(
            module.req_scratch_cap >= 16,
            "capacity must cover the request"
        );

        let large = module
            .wasm_request_scratch(4096)
            .expect("grow request region");
        assert_ne!(
            large, small,
            "a larger request than capacity must reallocate to a new region"
        );
        assert!(
            module.req_scratch_cap >= 4096,
            "capacity must grow to cover the larger request"
        );

        let reused = module
            .wasm_request_scratch(8)
            .expect("reuse request region");
        assert_eq!(
            reused, large,
            "a request within capacity must reuse the region without reallocating"
        );
    }

    /// Request-bearing RPCs reuse the persistent wasm-side request region: the
    /// first such call allocates it, and a later same-size request keeps the very
    /// same pointer, proving the per-call `wasm_alloc`/`wasm_free` request pair is
    /// gone.
    #[test]
    fn request_bearing_rpc_reuses_wasm_region() {
        let mut module = super::Module::new().expect("instantiate module");
        assert_eq!(
            module.req_scratch_ptr, 0,
            "the wasm request region must start unallocated"
        );

        // An empty-request constructor does not touch the request region.
        let resp = module
            .invoke(SVC_PARSER_OPTIONS, MID_NEW_PARSER_OPTIONS, &[])
            .expect("NewParserOptions RPC");
        let first = super::pb::read_handle_at_field(&resp, 1);
        assert_ne!(first, 0, "NewParserOptions must return a non-null handle");
        assert_eq!(
            module.req_scratch_ptr, 0,
            "an empty request must not allocate the request region"
        );

        // Freeing it carries a handle argument: the first request-bearing RPC.
        module
            .invoke_handle(SVC_PARSER_OPTIONS, MID_FREE_PARSER_OPTIONS, first)
            .expect("FreeParserOptions via invoke_handle");
        let region = module.req_scratch_ptr;
        assert_ne!(region, 0, "a request-bearing RPC must allocate the region");

        // A second same-size request-bearing RPC must reuse the identical region.
        let resp = module
            .invoke(SVC_PARSER_OPTIONS, MID_NEW_PARSER_OPTIONS, &[])
            .expect("NewParserOptions RPC");
        let second = super::pb::read_handle_at_field(&resp, 1);
        module
            .invoke_handle(SVC_PARSER_OPTIONS, MID_FREE_PARSER_OPTIONS, second)
            .expect("FreeParserOptions via invoke_handle");
        assert_eq!(
            module.req_scratch_ptr, region,
            "a same-size request must reuse the region, not reallocate it"
        );
    }
}
