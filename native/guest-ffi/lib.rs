//! C-ABI staticlib wrapper around the wasm2rs-transpiled `guest` crate (PoC).
//!
//! Exposes the six [`GuestInstance`](../../src/backend.rs) operations — plus
//! construct/destroy — through a flat C ABI, so the compiled, opt-level-3 object
//! code can be distributed as a `.a` and linked into `googlesql` **without**
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

// The generated (svc,mid)/name -> `Instance` method dispatch, identical to the
// table googlesql's native backend includes. Wrapped in a module so the blanket
// allow for the machine-generated code stays scoped to it.
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    reason = "machine-generated dispatch table (26k arms); not hand-maintained"
)]
mod dispatch {
    include!("../dispatch.rs");
}

/// Host imports the transpiled module leaves unresolved: the C++ runtime `env`
/// stubs plus two WASI functions wasm2rs did not wire natively.
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

impl guest::Imports for HostImports {
    fn import2(&mut self, _a0: i32, _a1: i32, _a2: i32) {
        // env::__cxa_throw — a C++ exception. It cannot be resumed; trap here and
        // let the FFI boundary's catch_unwind turn it into an error status.
        panic!("C++ exception thrown in wasm (env::__cxa_throw)");
    }
    fn import4(&mut self, _a0: i32, _a1: i32, _a2: i32, _a3: i32) -> i64 {
        0 // wasmify::callback_invoke — no host callbacks are registered.
    }
    fn import0(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import1(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import3(&mut self, _a0: i32, _a1: i32, _a2: i32, _a3: i32, _a4: i32) {}
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
    fn import16(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import17(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import18(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import19(&mut self, _a0: i32, _a1: i32, _a2: i32, _a3: i32, _a4: i32) -> i32 {
        0
    }
    fn import20(&mut self, _a0: i32) {}
    fn import21(&mut self, _a0: i32, _a1: i32, _a2: i32, _a3: i32, _a4: i32, _a5: i32) -> i32 {
        0
    }
    fn import22(&mut self, _a0: i32) {}
    fn import23(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import24(&mut self, _a0: i32) -> i32 {
        0
    }
    fn import25(&mut self, _a0: i32, _a1: i32, _a2: i32) -> i32 {
        0
    }
    fn import31(&mut self, _a0: i32, _a1: i32) -> i32 {
        0
    }
    fn import38(&mut self, _a0: i32, _a1: i32, _a2: i32, _a3: i32) -> i32 {
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
#[unsafe(no_mangle)]
pub extern "C" fn guest_new(out: *mut *mut Inst) -> i32 {
    guard(|| {
        // Root the single WASI preopen at `/`, matching the wasmtime backend, so
        // the analyzer's absolute-path timezone reads resolve.
        let mut instance = guest::Instance::new(HostImports).with_preopen_root("/");
        instance.func41(); // WASI reactor / C++ global constructors.
        let _status: i32 = instance.func15885(); // wasm_init (status ignored, as in native).
        let boxed = Box::into_raw(Box::new(instance));
        // SAFETY: `out` is a caller-provided writable out-pointer.
        unsafe {
            *out = boxed;
        }
        OK
    })
}

/// Releases an instance created by [`guest_new`]. A null pointer is ignored.
///
/// # Safety
/// `inst` must be a pointer returned by [`guest_new`] and not used afterward.
#[unsafe(no_mangle)]
pub extern "C" fn guest_free_instance(inst: *mut Inst) {
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
#[unsafe(no_mangle)]
pub extern "C" fn guest_alloc(inst: *mut Inst, len: u32, out: *mut u32) -> i32 {
    guard(|| {
        // SAFETY: `inst` is a live instance for the duration of the call.
        let inst = unsafe { &mut *inst };
        let ptr = inst.func42(len.cast_signed()).cast_unsigned();
        // SAFETY: `out` is a caller-provided writable out-pointer.
        unsafe {
            *out = ptr;
        }
        OK
    })
}

/// Frees a guest allocation previously returned by [`guest_alloc`].
///
/// # Safety
/// `inst` must be a live instance.
#[unsafe(no_mangle)]
pub extern "C" fn guest_free(inst: *mut Inst, ptr: u32) -> i32 {
    guard(|| {
        // SAFETY: `inst` is a live instance for the duration of the call.
        let inst = unsafe { &mut *inst };
        inst.func43(ptr.cast_signed());
        OK
    })
}

/// Copies `data_len` bytes from `data` into guest memory at offset `ptr`.
///
/// # Safety
/// `inst` must be a live instance and `data` must point to `data_len` readable
/// bytes.
#[unsafe(no_mangle)]
pub extern "C" fn guest_write(inst: *mut Inst, ptr: u32, data: *const u8, data_len: usize) -> i32 {
    guard(|| {
        // SAFETY: `inst` is live; `data`/`data_len` describe a readable range.
        let inst = unsafe { &mut *inst };
        let data = unsafe { std::slice::from_raw_parts(data, data_len) };
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
#[unsafe(no_mangle)]
pub extern "C" fn guest_read(inst: *mut Inst, ptr: u32, len: u32, out_buf: *mut u8) -> i32 {
    guard(|| {
        // SAFETY: `inst` is live for the duration of the call.
        let inst = unsafe { &mut *inst };
        let Ok(offset) = usize::try_from(ptr) else {
            return ERR_MEMORY;
        };
        let Ok(len) = usize::try_from(len) else {
            return ERR_MEMORY;
        };
        let Some(end) = offset.checked_add(len) else {
            return ERR_MEMORY;
        };
        let mem = inst.memory();
        let Some(slice) = mem.get(offset..end) else {
            return ERR_MEMORY;
        };
        // SAFETY: caller guarantees `out_buf` covers `len` writable bytes.
        let out = unsafe { std::slice::from_raw_parts_mut(out_buf, len) };
        out.copy_from_slice(slice);
        OK
    })
}

/// Invokes the RPC export identified by `(svc, mid)` with the request at
/// `(ptr, len)`, writing the packed `(resp_ptr, resp_len)` result to `*out`.
///
/// # Safety
/// `inst` must be a live instance and `out` a writable `*mut u64`.
#[unsafe(no_mangle)]
pub extern "C" fn guest_call_rpc(
    inst: *mut Inst,
    svc: i32,
    mid: i32,
    ptr: u32,
    len: u32,
    out: *mut u64,
) -> i32 {
    guard(|| {
        // SAFETY: `inst` is live for the duration of the call.
        let inst = unsafe { &mut *inst };
        let Some(func) = dispatch::packed_by_rpc::<HostImports>(svc, mid) else {
            return ERR_NO_EXPORT;
        };
        let packed = func(inst, ptr.cast_signed(), len.cast_signed()).cast_unsigned();
        // SAFETY: `out` is a caller-provided writable out-pointer.
        unsafe {
            *out = packed;
        }
        OK
    })
}

/// Invokes the named export with the request at `(ptr, len)`, writing the packed
/// `(resp_ptr, resp_len)` result to `*out`. `name` is UTF-8 of length `name_len`.
///
/// # Safety
/// `inst` must be a live instance, `name_ptr` must point to `name_len` readable
/// bytes, and `out` must be a writable `*mut u64`.
#[unsafe(no_mangle)]
pub extern "C" fn guest_call_named(
    inst: *mut Inst,
    name_ptr: *const u8,
    name_len: usize,
    ptr: u32,
    len: u32,
    out: *mut u64,
) -> i32 {
    guard(|| {
        // SAFETY: `inst` is live; `name_ptr`/`name_len` describe a readable range.
        let inst = unsafe { &mut *inst };
        let name_bytes = unsafe { std::slice::from_raw_parts(name_ptr, name_len) };
        let Ok(name) = std::str::from_utf8(name_bytes) else {
            return ERR_MEMORY;
        };
        let Some(func) = dispatch::packed_by_name::<HostImports>(name) else {
            return ERR_NO_EXPORT;
        };
        let packed = func(inst, ptr.cast_signed(), len.cast_signed()).cast_unsigned();
        // SAFETY: `out` is a caller-provided writable out-pointer.
        unsafe {
            *out = packed;
        }
        OK
    })
}
