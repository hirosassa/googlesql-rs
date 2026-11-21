//! C-ABI cdylib wrapper around the wasm2rs-transpiled `guest` crate (PoC).
//!
//! Exposes the six [`GuestInstance`](../../src/backend.rs) operations — plus
//! construct/destroy — through a flat C ABI, so the compiled, opt-level-3 object
//! code can be distributed as a shared library and linked into `googlesql` **without**
//! `guest` being a Cargo dependency (and without the rustc-version coupling an
//! `rlib` would impose). `HostImports` and the generated dispatch table — which
//! also live in `googlesql`'s in-crate `native` backend — are duplicated here so
//! this archive is self-contained.
//!
//! ## Boundary safety
//!
//! Every entry point wraps its body in [`catch_unwind`]: a guest trap surfaces
//! as a `panic!` (e.g. the C++ `__cxa_throw` stub), and unwinding across an
//! `extern "C"` boundary is undefined behavior. Panics are converted to a
//! nonzero status instead — strictly safer than the in-crate `native` backend,
//! which lets such a trap unwind through Rust frames.
//!
//! Results are returned through caller-provided out-pointers; the return value
//! is always a status code (`0` = success). The caller owns all buffers, so no
//! allocation crosses the boundary: `guest_read` copies into a caller buffer
//! that must be `len` bytes.

use std::panic::{AssertUnwindSafe, catch_unwind};

// `guard` below relies on unwinding to turn a guest trap into an error status;
// under `panic=abort` it would silently become a no-op and a trap would kill the
// host process, so refuse to build that configuration.
#[cfg(panic = "abort")]
compile_error!("guest-ffi requires panic=unwind: the C-ABI boundary relies on catch_unwind");

// The generated (svc,mid)/name -> `Instance` method dispatch, identical to the
// table googlesql's native backend includes. Wrapped in a module so the blanket
// allow for the machine-generated code stays scoped to it.
#[allow(
    clippy::allow_attributes,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    reason = "machine-generated dispatch table (26k arms); not hand-maintained, and an `expect` \
              would go unfulfilled for whichever group lints the table happens not to trip"
)]
mod dispatch {
    include!("../dispatch.rs");
}

/// Host imports the transpiled module leaves unresolved: the C++ runtime `env`
/// stubs (wasm2rs wires every WASI function natively since v0.2.0).
///
/// A hand-kept mirror of `src/native_backend.rs`'s `HostImports`: it must match
/// that impl exactly (zero result values; a C++ throw traps), so the native-ffi
/// engine behaves identically to the native engine and the differential test can
/// demand byte-for-byte agreement. The two crates cannot share source (guest-ffi
/// is an excluded workspace), so any edit to one must be mirrored in the other.
///
/// `pub` only because it appears in the opaque `Inst` type of the `extern "C"`
/// signatures below; the C ABI hands it back as a `void*`-equivalent, so callers
/// never name it.
pub struct HostImports;

/// Traps the guest on a C++ `throw`. The `guest::Imports` methods return plain
/// values, so a panic is the only way to abort the call; `guard` catches it at
/// the C-ABI boundary and reports `ERR_PANIC`.
#[expect(
    clippy::panic,
    reason = "a C++ throw cannot be resumed; the panic is caught by `guard` at the C-ABI edge"
)]
fn trap_cxx_throw() -> ! {
    panic!("C++ exception thrown in wasm (env::__cxa_throw)");
}

impl guest::Imports for HostImports {
    fn import0(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import1(&mut self, _a0: i32) -> i32 {
        0
    }
    // env::__cxa_throw — a C++ exception. It cannot be resumed; trap here and
    // let the FFI boundary's catch_unwind turn it into an error status.
    fn import2(&mut self, _a0: i32, _a1: i32, _a2: i32) {
        trap_cxx_throw();
    }
    fn import3(&mut self, _a0: i32, _a1: i32, _a2: i32, _a3: i32, _a4: i32) {}
    fn import4(&mut self, _a0: i32, _a1: i32, _a2: i32, _a3: i32) -> i64 {
        0 // wasmify::callback_invoke — no host callbacks are registered.
    }
    fn import5(&mut self, _a0: i32, _a1: i32, _a2: i32) -> i32 {
        0
    }
    fn import6(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import7(&mut self, _a0: i32, _a1: i32, _a2: i32) -> i32 {
        0
    }
    fn import8(&mut self) -> i32 {
        0
    }
    fn import9(&mut self) -> i32 {
        0
    }
    fn import10(&mut self) -> i32 {
        0
    }
    fn import11(&mut self) -> i32 {
        0
    }
    fn import12(&mut self, _a0: i32, _a1: i32, _a2: i32) -> i32 {
        0
    }
    fn import13(&mut self) -> i32 {
        0
    }
    fn import14(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import15(&mut self, _a0: i32, _a1: i32) {}
    fn import16(&mut self, _a0: i32, _a1: i32, _a2: i32) -> i32 {
        0
    }
    fn import17(&mut self, _a0: i32, _a1: i32, _a2: i32) {}
    fn import18(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import19(
        &mut self,
        _a0: i32,
        _a1: i32,
        _a2: i32,
        _a3: i32,
        _a4: i32,
        _a5: i32,
        _a6: i32,
        _a7: i32,
        _a8: i32,
    ) -> i32 {
        0
    }
    fn import20(&mut self, _a0: i32, _a1: i32) {}
    fn import21(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import22(&mut self, _a0: i32) {}
    fn import23(&mut self, _a0: i32, _a1: i32, _a2: i32, _a3: i32, _a4: i32, _a5: i32) -> i32 {
        0
    }
    fn import24(&mut self, _a0: i32) {}
    fn import25(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import26(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import27(&mut self, _a0: i32, _a1: i32) {}
    fn import28(&mut self, _a0: i32, _a1: i32, _a2: i32, _a3: i32, _a4: i32) -> i32 {
        0
    }
    fn import29(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import30(&mut self, _a0: i32, _a1: i32) -> i32 {
        0
    }
    fn import31(&mut self, _a0: i32, _a1: i32) -> i32 {
        0
    }
    fn import32(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import33(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import34(&mut self, _a0: i32, _a1: i32) -> i32 {
        0
    }
    fn import35(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import36(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import37(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import38(&mut self, _a0: i32, _a1: i32, _a2: i32, _a3: i32) {}
    fn import39(&mut self, _a0: i32, _a1: i32, _a2: i32, _a3: i32) -> i32 {
        0
    }
    fn import40(&mut self, _a0: i32, _a1: i32, _a2: i32, _a3: i32, _a4: i32) -> i32 {
        0
    }
    fn import41(&mut self, _a0: i32, _a1: i32, _a2: i32) -> i32 {
        0
    }
    fn import42(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import43(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import44(&mut self, _a0: i32, _a1: i32, _a2: i32) -> i32 {
        0
    }
}

/// The opaque instance handle passed back and forth across the C ABI.
type Inst = guest::Instance<HostImports>;

/// Success.
const OK: i32 = 0;
/// A bad pointer, out-of-bounds range, or otherwise invalid argument.
const ERR_MEMORY: i32 = 1;
/// No export matched the requested `(svc, mid)` or name (a dispatch miss).
const ERR_NO_EXPORT: i32 = 2;
/// A guest trap (panic) unwound into the boundary and was caught.
const ERR_PANIC: i32 = -1;

/// Reborrows a caller-provided pointer as a mutable reference, or `None` if null.
///
/// # Safety
/// A non-null `ptr` must be valid for reads and writes of a `T` and not aliased
/// for the duration of the returned borrow.
#[expect(unsafe_code, reason = "raw-pointer reborrow at the C-ABI boundary")]
unsafe fn borrow_mut<'a, T>(ptr: *mut T) -> Option<&'a mut T> {
    // SAFETY: the caller upholds validity and aliasing; `as_mut` handles null.
    unsafe { ptr.as_mut() }
}

/// Views `(ptr, len)` as a byte slice. `(NULL, 0)` is an empty slice; a null
/// pointer with a nonzero length, or a length above `isize::MAX`, is `None`.
///
/// # Safety
/// A non-null `ptr` must point to `len` readable bytes that stay valid and
/// unmutated for the returned lifetime.
#[expect(
    unsafe_code,
    reason = "raw-parts slice construction at the C-ABI boundary"
)]
unsafe fn bytes_from<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    if len == 0 {
        return Some(&[]);
    }
    if ptr.is_null() || isize::try_from(len).is_err() {
        return None;
    }
    // SAFETY: non-null, `len <= isize::MAX`, and the caller guarantees the range.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// Mutable twin of [`bytes_from`].
///
/// # Safety
/// A non-null `ptr` must point to `len` writable bytes that stay valid and
/// unaliased for the returned lifetime.
#[expect(
    unsafe_code,
    reason = "raw-parts slice construction at the C-ABI boundary"
)]
unsafe fn bytes_from_mut<'a>(ptr: *mut u8, len: usize) -> Option<&'a mut [u8]> {
    if len == 0 {
        return Some(&mut []);
    }
    if ptr.is_null() || isize::try_from(len).is_err() {
        return None;
    }
    // SAFETY: non-null, `len <= isize::MAX`, and the caller guarantees the range.
    Some(unsafe { std::slice::from_raw_parts_mut(ptr, len) })
}

/// Runs `body`, catching any unwind so a guest trap never crosses the C ABI.
///
/// This is why every entry point writes its `*out` result inside `body`: an
/// out-pointer is written only on the success (`OK`) path, and is left untouched
/// on any error status (including a caught panic, where `body` never runs to the
/// write). Callers must therefore treat `*out` as valid only when the returned
/// status is `OK`, and pre-initialize it before the call (as the Rust shim does).
fn guard(body: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or(ERR_PANIC)
}

/// Constructs and initializes a native instance, writing the owning pointer to
/// `*out`. The caller must release it with [`guest_free_instance`].
///
/// # Safety
/// `out` must be a valid, writable pointer to a `*mut Inst`.
#[expect(
    unsafe_code,
    reason = "C-ABI export: exported symbol, raw pointers dereferenced behind null checks"
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn guest_new(out: *mut *mut Inst) -> i32 {
    guard(|| {
        // SAFETY: the caller guarantees `out` is valid and writable if non-null.
        let Some(out) = (unsafe { borrow_mut(out) }) else {
            return ERR_MEMORY;
        };
        // Root the single WASI preopen at `/`, matching the wasmtime backend, so
        // the analyzer's absolute-path timezone reads resolve.
        let mut instance = guest::Instance::new(HostImports).with_preopen_root("/");
        instance.func60(); // WASI reactor / C++ global constructors.
        let _status: i32 = instance.func15923(); // wasm_init (status ignored, as in native).
        *out = Box::into_raw(Box::new(instance));
        OK
    })
}

/// Releases an instance created by [`guest_new`]. A null pointer is ignored.
///
/// # Safety
/// `inst` must be a pointer returned by [`guest_new`] and not used afterward.
#[expect(
    unsafe_code,
    reason = "C-ABI export: exported symbol, raw pointers dereferenced behind null checks"
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn guest_free_instance(inst: *mut Inst) {
    if inst.is_null() {
        return;
    }
    let _ = guard(|| {
        // SAFETY: `inst` came from `guest_new`'s `Box::into_raw` and is not aliased.
        drop(unsafe { Box::from_raw(inst) });
        OK
    });
}

/// Allocates `len` bytes in guest memory, writing the offset to `*out`.
///
/// # Safety
/// `inst` must be a live instance and `out` a writable `*mut u32`.
#[expect(
    unsafe_code,
    reason = "C-ABI export: exported symbol, raw pointers dereferenced behind null checks"
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn guest_alloc(inst: *mut Inst, len: u32, out: *mut u32) -> i32 {
    guard(|| {
        // SAFETY: the caller guarantees `inst` is live and `out` writable if non-null.
        let (Some(inst), Some(out)) = (unsafe { borrow_mut(inst) }, unsafe { borrow_mut(out) })
        else {
            return ERR_MEMORY;
        };
        *out = inst.func61(len.cast_signed()).cast_unsigned();
        OK
    })
}

/// Frees a guest allocation previously returned by [`guest_alloc`].
///
/// # Safety
/// `inst` must be a live instance.
#[expect(
    unsafe_code,
    reason = "C-ABI export: exported symbol, raw pointers dereferenced behind null checks"
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn guest_free(inst: *mut Inst, ptr: u32) -> i32 {
    guard(|| {
        // SAFETY: the caller guarantees `inst` is live if non-null.
        let Some(inst) = (unsafe { borrow_mut(inst) }) else {
            return ERR_MEMORY;
        };
        inst.func62(ptr.cast_signed());
        OK
    })
}

/// Copies `data_len` bytes from `data` into guest memory at offset `ptr`.
///
/// # Safety
/// `inst` must be a live instance and `data` must point to `data_len` readable
/// bytes.
#[expect(
    unsafe_code,
    reason = "C-ABI export: exported symbol, raw pointers dereferenced behind null checks"
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn guest_write(
    inst: *mut Inst,
    ptr: u32,
    data: *const u8,
    data_len: usize,
) -> i32 {
    guard(|| {
        // SAFETY: the caller guarantees `inst` is live and `data`/`data_len`
        // describe a readable range.
        let (Some(inst), Some(data)) = (unsafe { borrow_mut(inst) }, unsafe {
            bytes_from(data, data_len)
        }) else {
            return ERR_MEMORY;
        };
        let Ok(offset) = usize::try_from(ptr) else {
            return ERR_MEMORY;
        };
        let Some(end) = offset.checked_add(data.len()) else {
            return ERR_MEMORY;
        };
        let mem = inst.memory();
        let Some(dst) = mem.get_mut(offset..end) else {
            return ERR_MEMORY;
        };
        dst.copy_from_slice(data);
        OK
    })
}

/// Copies `len` bytes from guest memory at offset `ptr` into `out_buf`, which
/// the caller must have sized to at least `len` bytes.
///
/// # Safety
/// `inst` must be a live instance and `out_buf` must point to `len` writable
/// bytes.
#[expect(
    unsafe_code,
    reason = "C-ABI export: exported symbol, raw pointers dereferenced behind null checks"
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn guest_read(inst: *mut Inst, ptr: u32, len: u32, out_buf: *mut u8) -> i32 {
    guard(|| {
        let Ok(offset) = usize::try_from(ptr) else {
            return ERR_MEMORY;
        };
        let Ok(len) = usize::try_from(len) else {
            return ERR_MEMORY;
        };
        // SAFETY: the caller guarantees `inst` is live and `out_buf` covers `len`
        // writable bytes.
        let (Some(inst), Some(out)) = (unsafe { borrow_mut(inst) }, unsafe {
            bytes_from_mut(out_buf, len)
        }) else {
            return ERR_MEMORY;
        };
        let Some(end) = offset.checked_add(len) else {
            return ERR_MEMORY;
        };
        let mem = inst.memory();
        let Some(slice) = mem.get(offset..end) else {
            return ERR_MEMORY;
        };
        out.copy_from_slice(slice);
        OK
    })
}

/// Invokes the RPC export identified by `(svc, mid)` with the request at
/// `(ptr, len)`, writing the packed `(resp_ptr, resp_len)` result to `*out`.
///
/// # Safety
/// `inst` must be a live instance and `out` a writable `*mut u64`.
#[expect(
    unsafe_code,
    reason = "C-ABI export: exported symbol, raw pointers dereferenced behind null checks"
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn guest_call_rpc(
    inst: *mut Inst,
    svc: i32,
    mid: i32,
    ptr: u32,
    len: u32,
    out: *mut u64,
) -> i32 {
    guard(|| {
        // SAFETY: the caller guarantees `inst` is live and `out` writable if non-null.
        let (Some(inst), Some(out)) = (unsafe { borrow_mut(inst) }, unsafe { borrow_mut(out) })
        else {
            return ERR_MEMORY;
        };
        let Some(func) = dispatch::packed_by_rpc::<HostImports>(svc, mid) else {
            return ERR_NO_EXPORT;
        };
        *out = func(inst, ptr.cast_signed(), len.cast_signed()).cast_unsigned();
        OK
    })
}

/// Invokes the named export with the request at `(ptr, len)`, writing the packed
/// `(resp_ptr, resp_len)` result to `*out`. `name` is UTF-8 of length `name_len`.
///
/// # Safety
/// `inst` must be a live instance, `name_ptr` must point to `name_len` readable
/// bytes, and `out` must be a writable `*mut u64`.
#[expect(
    unsafe_code,
    reason = "C-ABI export: exported symbol, raw pointers dereferenced behind null checks"
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn guest_call_named(
    inst: *mut Inst,
    name_ptr: *const u8,
    name_len: usize,
    ptr: u32,
    len: u32,
    out: *mut u64,
) -> i32 {
    guard(|| {
        // SAFETY: the caller guarantees `inst` is live, `out` writable, and
        // `name_ptr`/`name_len` a readable range.
        let (Some(inst), Some(out), Some(name_bytes)) = (
            unsafe { borrow_mut(inst) },
            unsafe { borrow_mut(out) },
            unsafe { bytes_from(name_ptr, name_len) },
        ) else {
            return ERR_MEMORY;
        };
        let Ok(name) = std::str::from_utf8(name_bytes) else {
            return ERR_MEMORY;
        };
        let Some(func) = dispatch::packed_by_name::<HostImports>(name) else {
            return ERR_NO_EXPORT;
        };
        *out = func(inst, ptr.cast_signed(), len.cast_signed()).cast_unsigned();
        OK
    })
}
