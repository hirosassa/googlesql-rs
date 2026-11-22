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
#[expect(
    unsafe_code,
    reason = "extern declarations for the guest-ffi C ABI; every call site carries a SAFETY comment"
)]
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

/// `guest-ffi`'s out-of-range status (`ERR_MEMORY` in `native/guest-ffi/lib.rs`).
const ERR_MEMORY: i32 = 1;

/// Allocates `len` zeroed bytes, returning `Error::Memory` instead of aborting
/// when the allocator refuses. `len` is guest-supplied (up to 4 GiB), so
/// `vec![0u8; len]` could abort; `try_reserve_exact` + `resize` would be
/// fallible but touches every page. `alloc_zeroed` is both fallible and lazy
/// (untouched pages stay uncommitted until the guest bounds check rejects them).
fn alloc_zeroed_buf(len: usize) -> Result<Vec<u8>, Error> {
    if len == 0 {
        return Ok(Vec::new());
    }
    let layout = std::alloc::Layout::array::<u8>(len).map_err(|e| Error::Memory(e.to_string()))?;
    // SAFETY: `layout` has non-zero size (`len > 0`).
    #[expect(
        unsafe_code,
        reason = "raw allocation for a read buffer; see the SAFETY comment"
    )]
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        return Err(Error::Memory(format!(
            "cannot allocate {len} byte read buffer"
        )));
    }
    // SAFETY: `ptr` was allocated by the global allocator with `Layout::array::<u8>(len)`
    // (align 1, size `len`) and is fully initialized (zeroed), so it is a valid
    // `Vec<u8>` of length and capacity `len`.
    #[expect(
        clippy::same_length_and_capacity,
        reason = "u8 buffer sized exactly by the allocation; no layout-changing conversion follows"
    )]
    #[expect(
        unsafe_code,
        reason = "raw allocation for a read buffer; see the SAFETY comment"
    )]
    let buf = unsafe { Vec::from_raw_parts(ptr, len, len) };
    Ok(buf)
}

/// The native-ffi engine: owns one staticlib instance for its lifetime.
pub struct NativeFfiInstance {
    handle: *mut GuestHandle,
}

// SAFETY: the handle owns a single guest instance that is only ever touched from
// the `&mut self` methods below, i.e. from one thread at a time. This matches
// the `GuestInstance: Send` (not `Sync`) contract the other backends uphold, and
// how `Module` moves an instance between threads without sharing it.
#[expect(
    unsafe_code,
    reason = "the handle is owned exclusively by this instance; see the SAFETY comment"
)]
unsafe impl Send for NativeFfiInstance {}

impl NativeFfiInstance {
    /// Constructs and initializes a native-ffi instance via the staticlib.
    pub(crate) fn new() -> Result<Self, Error> {
        let mut handle: *mut GuestHandle = std::ptr::null_mut();
        // SAFETY: `&raw mut handle` is a valid, writable out-pointer.
        #[expect(
            unsafe_code,
            reason = "FFI call into the guest-ffi cdylib; preconditions are stated in the SAFETY comment"
        )]
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
        #[expect(
            unsafe_code,
            reason = "FFI call into the guest-ffi cdylib; preconditions are stated in the SAFETY comment"
        )]
        unsafe {
            guest_free_instance(self.handle);
        }
    }
}

impl GuestInstance for NativeFfiInstance {
    fn alloc(&mut self, len: u32) -> Result<u32, Error> {
        let mut out: u32 = 0;
        // SAFETY: `handle` is live; `out` is a valid writable pointer.
        #[expect(
            unsafe_code,
            reason = "FFI call into the guest-ffi cdylib; preconditions are stated in the SAFETY comment"
        )]
        let status = unsafe { guest_alloc(self.handle, len, &raw mut out) };
        check(status, "alloc")?;
        Ok(out)
    }

    fn free(&mut self, ptr: u32) -> Result<(), Error> {
        // SAFETY: `handle` is live for the duration of the call.
        #[expect(
            unsafe_code,
            reason = "FFI call into the guest-ffi cdylib; preconditions are stated in the SAFETY comment"
        )]
        let status = unsafe { guest_free(self.handle, ptr) };
        check(status, "free")
    }

    fn write(&mut self, ptr: u32, data: &[u8]) -> Result<(), Error> {
        // SAFETY: `handle` is live; `data` points to `data.len()` readable bytes.
        #[expect(
            unsafe_code,
            reason = "FFI call into the guest-ffi cdylib; preconditions are stated in the SAFETY comment"
        )]
        let status = unsafe { guest_write(self.handle, ptr, data.as_ptr(), data.len()) };
        check(status, "write")
    }

    fn read(&mut self, ptr: u32, len: u32) -> Result<Vec<u8>, Error> {
        let len_usize = usize::try_from(len).map_err(|e| Error::Memory(e.to_string()))?;
        let mut buf = alloc_zeroed_buf(len_usize)?;
        // SAFETY: `handle` is live; `buf` has exactly `len` writable bytes.
        #[expect(
            unsafe_code,
            reason = "FFI call into the guest-ffi cdylib; preconditions are stated in the SAFETY comment"
        )]
        let status = unsafe { guest_read(self.handle, ptr, len, buf.as_mut_ptr()) };
        if status == ERR_MEMORY {
            return Err(Error::Memory(format!(
                "guest-ffi read of {len} bytes at {ptr} is out of bounds"
            )));
        }
        check(status, "read")?;
        Ok(buf)
    }

    fn call_rpc(&mut self, svc: i32, mid: i32, req_ptr: u32, req_len: u32) -> Result<u64, Error> {
        let mut out: u64 = 0;
        // SAFETY: `handle` is live; `out` is a valid writable pointer.
        #[expect(
            unsafe_code,
            reason = "FFI call into the guest-ffi cdylib; preconditions are stated in the SAFETY comment"
        )]
        let status =
            unsafe { guest_call_rpc(self.handle, svc, mid, req_ptr, req_len, &raw mut out) };
        check(status, "call_rpc")?;
        Ok(out)
    }

    fn call_named(&mut self, name: &str, req_ptr: u32, req_len: u32) -> Result<u64, Error> {
        let mut out: u64 = 0;
        // SAFETY: `handle` is live; `name` points to `name.len()` readable bytes
        // and `out` is a valid writable pointer.
        #[expect(
            unsafe_code,
            reason = "FFI call into the guest-ffi cdylib; preconditions are stated in the SAFETY comment"
        )]
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

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test code")]
mod tests {
    use super::NativeFfiInstance;
    use crate::backend::GuestInstance;
    use crate::error::Error;

    /// A real instance plus a `(ptr, len)` block it allocated, so the bounds the
    /// guest enforces are the ones the test probes.
    fn instance_with_block(len: u32) -> (NativeFfiInstance, u32) {
        let mut inst = NativeFfiInstance::new().expect("instantiate native-ffi instance");
        let ptr = inst.alloc(len).expect("alloc block");
        (inst, ptr)
    }

    /// A read far past linear memory (guest-supplied `len` up to 4 GiB) must come
    /// back as `Error::Memory`, never abort the process on allocation failure.
    #[test]
    fn read_out_of_bounds_is_a_memory_error() {
        let (mut inst, ptr) = instance_with_block(16);

        let err = inst.read(ptr, u32::MAX).expect_err("len far beyond memory");
        assert!(matches!(err, Error::Memory(_)), "unexpected error: {err}");
        let err = inst.read(u32::MAX, 1).expect_err("ptr beyond memory");
        assert!(matches!(err, Error::Memory(_)), "unexpected error: {err}");
    }

    /// A read of exactly the allocated block round-trips what was written.
    #[test]
    fn read_exact_block_round_trips() {
        let (mut inst, ptr) = instance_with_block(4);
        inst.write(ptr, &[1, 2, 3, 4]).expect("write block");

        assert_eq!(inst.read(ptr, 4).expect("read block"), [1, 2, 3, 4]);
        assert_eq!(inst.read(ptr, 0).expect("empty read"), Vec::<u8>::new());
    }
}
