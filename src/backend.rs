//! The engine-agnostic wasm ABI that [`Module`](crate::Module) drives.
//!
//! GoogleSQL's parser, formatter, and analyzer are reached through a small,
//! fixed set of wasm operations: allocate and free linear memory, read and write
//! it, and call an export following the
//! `w_<svc>_<mid>(ptr,len) -> (ptr<<32 | len)` RPC convention (or a named export
//! such as `wasmify_get_type_name` under the same `(ptr,len) -> packed` shape).
//!
//! [`GuestInstance`] is exactly that surface. Everything above it — the request
//! encoding, the persistent request region, decoding the packed response, the
//! deferred-free machinery, and the parser/analyzer bindings — is written once
//! against this trait and shared across engines. The default engine is
//! [`wasmtime_backend::WasmtimeInstance`](crate::wasmtime_backend::WasmtimeInstance);
//! a future ahead-of-time-compiled (wasm2rs) engine implements the same trait so
//! it drops in behind the identical `Module` API.
//!
//! The trait is `Send` because a [`Module`](crate::Module) is moved between
//! threads for parallelism (one instance per thread); it is deliberately not
//! `Sync`, since each engine forbids concurrent calls into a single instance.

use crate::error::Error;

/// A single running instance of the GoogleSQL guest, abstracted over the engine
/// that executes it.
///
/// Implementors own the guest's linear memory and exports. Resolving an export
/// (and caching that resolution) is an engine-specific concern and lives in the
/// implementation, not here: a JIT engine caches a resolved function handle,
/// whereas an ahead-of-time-compiled engine resolves exports at compile time and
/// needs no cache.
pub(crate) trait GuestInstance: Send {
    /// Allocates `len` bytes in the guest's linear memory and returns the pointer.
    fn alloc(&mut self, len: u32) -> Result<u32, Error>;

    /// Frees a pointer previously returned by [`alloc`](Self::alloc) (or by a
    /// guest export that allocates its response buffer).
    fn free(&mut self, ptr: u32) -> Result<(), Error>;

    /// Writes `data` into the guest's linear memory at `ptr`.
    fn write(&mut self, ptr: u32, data: &[u8]) -> Result<(), Error>;

    /// Reads `len` bytes from the guest's linear memory starting at `ptr`.
    fn read(&mut self, ptr: u32, len: u32) -> Result<Vec<u8>, Error>;

    /// Calls the `w_<svc>_<mid>(ptr,len) -> packed` RPC export and returns the
    /// raw packed `(ptr<<32 | len)` response value. `req_ptr`/`req_len` are `0`
    /// for an empty request.
    fn call_rpc(&mut self, svc: i32, mid: i32, req_ptr: u32, req_len: u32) -> Result<u64, Error>;

    /// Calls a named `(ptr,len) -> packed` export (e.g. `wasmify_get_type_name`)
    /// and returns the raw packed response value, the by-name counterpart to
    /// [`call_rpc`](Self::call_rpc).
    fn call_named(&mut self, name: &str, req_ptr: u32, req_len: u32) -> Result<u64, Error>;
}
