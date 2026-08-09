//! The native-ffi [`GuestInstance`] engine: the wasm2rs `guest` reached through
//! a C-ABI cdylib (`native/guest-ffi`) instead of the `guest` path-dependency.
//!
//! Enabled by the `native-ffi` feature (PoC). Where the `native` backend links
//! the `guest` crate directly and drives its `Instance` methods in-process, this
//! backend links a prebuilt `guest-ffi` shared library and reaches the same
//! engine through a flat C ABI. Every request marshaling and the packed-response
//! decode stay in `Module`; this file is only the six raw operations, each a thin
//! FFI call.
//!
//! The point of the C ABI is portability: an `rlib` is tied to an exact rustc
//! version, but a cdylib with a C ABI is not, so the optimized (opt-level-3)
//! object code can be built once and distributed as a Release asset. A cdylib
//! (rather than a staticlib) also keeps its bundled `std` internal, so a consumer
//! built with a different rustc links cleanly instead of colliding on the one
//! fixed-name std symbol (`rust_eh_personality`) a staticlib would leak. The
//! library also converts guest traps into error statuses at its boundary, so no
//! panic unwinds across the `extern "C"` edge.
#![allow(
    unsafe_code,
    reason = "this backend is an FFI shim over the C-ABI guest-ffi cdylib; all unsafe is confined here"
)]

use crate::backend::GuestInstance;
use crate::error::Error;

/// Opaque handle to a guest-ffi instance — an `Instance` owned and boxed by the
/// shared library. Never dereferenced on this side; only passed across the ABI.
#[repr(C)]
struct GuestHandle {
    _private: [u8; 0],
}

// The C ABI exported by the `guest-ffi` cdylib (see `native/guest-ffi/lib.rs`).
// Each returns an `i32` status (`0` = success) and writes results through
// out-pointers, so no allocation crosses the boundary.
unsafe extern "C" {
    fn guest_new(out: *mut *mut GuestHandle) -> i32;
    fn guest_free_instance(inst: *mut GuestHandle);
    fn guest_alloc(inst: *mut GuestHandle, len: u32, out: *mut u32) -> i32;
    fn guest_free(inst: *mut GuestHandle, ptr: u32) -> i32;
    fn guest_write(inst: *mut GuestHandle, ptr: u32, data: *const u8, data_len: usize) -> i32;
    fn guest_read(inst: *mut GuestHandle, ptr: u32, len: u32, out_buf: *mut u8) -> i32;
    fn guest_call_rpc(
        inst: *mut GuestHandle,
        svc: i32,
        mid: i32,
        ptr: u32,
        len: u32,
        out: *mut u64,
    ) -> i32;
    fn guest_call_named(
        inst: *mut GuestHandle,
        name_ptr: *const u8,
        name_len: usize,
        ptr: u32,
        len: u32,
        out: *mut u64,
    ) -> i32;
}

/// Turns a staticlib status code into a `Result`, naming the failed operation.
fn check(status: i32, op: &str) -> Result<(), Error> {
    if status == 0 {
        Ok(())
    } else {
        Err(Error::Wasm(format!(
            "guest-ffi {op} failed (status {status})"
        )))
    }
}

/// The native-ffi engine: owns one staticlib instance for its lifetime.
pub struct NativeFfiInstance {
    handle: *mut GuestHandle,
}

// SAFETY: the handle owns a single guest instance that is only ever touched from
// the `&mut self` methods below, i.e. from one thread at a time. This matches
// the `GuestInstance: Send` (not `Sync`) contract the other backends uphold, and
// how `Module` moves an instance between threads without sharing it.
unsafe impl Send for NativeFfiInstance {}

impl NativeFfiInstance {
    /// Constructs and initializes a native-ffi instance via the staticlib.
    pub(crate) fn new() -> Result<Self, Error> {
        let mut handle: *mut GuestHandle = std::ptr::null_mut();
        // SAFETY: `&raw mut handle` is a valid, writable out-pointer.
        let status = unsafe { guest_new(&raw mut handle) };
        check(status, "new")?;
        if handle.is_null() {
            return Err(Error::Wasm(
                "guest-ffi new returned a null instance".to_string(),
            ));
        }
        Ok(Self { handle })
    }
}

impl Drop for NativeFfiInstance {
    fn drop(&mut self) {
        // SAFETY: `handle` came from `guest_new` and is released exactly once here.
        unsafe { guest_free_instance(self.handle) };
    }
}

impl GuestInstance for NativeFfiInstance {
    fn alloc(&mut self, len: u32) -> Result<u32, Error> {
        let mut out: u32 = 0;
        // SAFETY: `handle` is live; `out` is a valid writable pointer.
        let status = unsafe { guest_alloc(self.handle, len, &raw mut out) };
        check(status, "alloc")?;
        Ok(out)
    }

    fn free(&mut self, ptr: u32) -> Result<(), Error> {
        // SAFETY: `handle` is live for the duration of the call.
        let status = unsafe { guest_free(self.handle, ptr) };
        check(status, "free")
    }

    fn write(&mut self, ptr: u32, data: &[u8]) -> Result<(), Error> {
        // SAFETY: `handle` is live; `data` points to `data.len()` readable bytes.
        let status = unsafe { guest_write(self.handle, ptr, data.as_ptr(), data.len()) };
        check(status, "write")
    }

    fn read(&mut self, ptr: u32, len: u32) -> Result<Vec<u8>, Error> {
        let len_usize = usize::try_from(len).map_err(|e| Error::Memory(e.to_string()))?;
        let mut buf = vec![0u8; len_usize];
        // SAFETY: `handle` is live; `buf` has exactly `len` writable bytes.
        let status = unsafe { guest_read(self.handle, ptr, len, buf.as_mut_ptr()) };
        check(status, "read")?;
        Ok(buf)
    }

    fn call_rpc(&mut self, svc: i32, mid: i32, req_ptr: u32, req_len: u32) -> Result<u64, Error> {
        let mut out: u64 = 0;
        // SAFETY: `handle` is live; `out` is a valid writable pointer.
        let status =
            unsafe { guest_call_rpc(self.handle, svc, mid, req_ptr, req_len, &raw mut out) };
        check(status, "call_rpc")?;
        Ok(out)
    }

    fn call_named(&mut self, name: &str, req_ptr: u32, req_len: u32) -> Result<u64, Error> {
        let mut out: u64 = 0;
        // SAFETY: `handle` is live; `name` points to `name.len()` readable bytes
        // and `out` is a valid writable pointer.
        let status = unsafe {
            guest_call_named(
                self.handle,
                name.as_ptr(),
                name.len(),
                req_ptr,
                req_len,
                &raw mut out,
            )
        };
        check(status, "call_named")?;
        Ok(out)
    }
}
