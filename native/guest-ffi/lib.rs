//! C-ABI cdylib wrapper around the `wasm2rs`-transpiled `guest` crate (proof of concept).
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

/// The opaque instance handle passed back and forth across the C ABI: the guest
/// instance plus the poison flag. Callers only ever see it as a `void*`.
///
/// `pub` only because it appears in the `extern "C"` signatures below.
pub struct Inst {
    guest: guest::Instance<HostImports>,
    /// Set when a guest trap was caught. A trap leaves the guest half-executed
    /// (shadow stack pointer, malloc state and wasm globals inconsistent), so a
    /// poisoned instance rejects every call except [`guest_free_instance`].
    poisoned: bool,
}

// `src/native_ffi_backend.rs` implements `Send` for its handle to this type by
// hand (`unsafe impl Send for NativeFfiInstance`), which is only sound while the
// instance is itself `Send`. Break the build here if a guest/import change makes
// it `!Send`, rather than letting the unsafe impl silently become wrong.
const _: fn() = || {
    const fn assert_send<T: Send>() {}
    assert_send::<Inst>();
};

/// Version of this C ABI, returned by [`guest_abi_version`]. Bump on any change
/// to a signature or to the meaning of a status code. Libraries that predate the
/// handshake do not export the symbol and are implicitly version 0.
const ABI_VERSION: u32 = 1;

/// Success.
const OK: i32 = 0;
/// A bad pointer, out-of-bounds range, or otherwise invalid argument.
const ERR_MEMORY: i32 = 1;
/// No export matched the requested `(svc, mid)` or name (a dispatch miss).
const ERR_NO_EXPORT: i32 = 2;
/// The guest allocator (`wasm_alloc`) returned NULL for a non-empty request.
const ERR_ALLOC: i32 = 3;
/// The instance trapped earlier; only [`guest_free_instance`] is allowed.
const ERR_POISONED: i32 = 4;
/// A guest trap (panic) unwound into the boundary and was caught. The instance
/// is poisoned from this point on.
const ERR_PANIC: i32 = -1;

/// Reborrows a caller-provided pointer as a mutable reference, or `None` if null.
///
/// # Safety
/// A non-null `ptr` must be valid for reads and writes of a `T` and not aliased
/// for the duration of the returned borrow.
#[expect(unsafe_code, reason = "raw-pointer reborrow at the C-ABI boundary")]
const unsafe fn borrow_mut<'a, T>(ptr: *mut T) -> Option<&'a mut T> {
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

/// The poison flag [`run_on_instance`] reads and sets; a trait so the state
/// machine can be exercised without constructing a real guest `Inst`.
trait Poisonable {
    fn is_poisoned(&self) -> bool;
    fn poison(&mut self);
}

impl Poisonable for Inst {
    fn is_poisoned(&self) -> bool {
        self.poisoned
    }
    fn poison(&mut self) {
        self.poisoned = true;
    }
}

/// The poison state machine behind [`with_instance`]: a missing instance is
/// `ERR_MEMORY`, a poisoned one `ERR_POISONED` (without running `body`), and a
/// `body` that traps poisons the instance so every later call is rejected.
fn run_on_instance<T: Poisonable>(inst: Option<&mut T>, body: impl FnOnce(&mut T) -> i32) -> i32 {
    let Some(inst) = inst else {
        return ERR_MEMORY;
    };
    if inst.is_poisoned() {
        return ERR_POISONED;
    }
    let status = guard(|| body(inst));
    if status == ERR_PANIC {
        inst.poison();
    }
    status
}

/// [`guard`] for entry points that operate on a live instance: rejects a null or
/// poisoned handle, and poisons the instance when `body` traps.
///
/// # Safety
/// A non-null `inst` must be a live pointer from [`guest_new`], not aliased for
/// the duration of the call.
#[expect(
    unsafe_code,
    reason = "raw-pointer reborrow of the instance handle at the C-ABI boundary"
)]
unsafe fn with_instance(inst: *mut Inst, body: impl FnOnce(&mut Inst) -> i32) -> i32 {
    // SAFETY: the caller guarantees `inst` is live and unaliased if non-null.
    run_on_instance(unsafe { borrow_mut(inst) }, body)
}

/// Returns the C ABI version ([`ABI_VERSION`]) this library implements, so a
/// host can refuse a library whose signatures or status codes differ from the
/// declarations it was compiled against.
#[expect(unsafe_code, reason = "C-ABI export: exported symbol")]
#[unsafe(no_mangle)]
pub const extern "C" fn guest_abi_version() -> u32 {
    ABI_VERSION
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
        let mut guest = guest::Instance::new(HostImports).with_preopen_root("/");
        guest.func60(); // WASI reactor / C++ global constructors.
        // `wasm_init`'s returned status is deliberately not treated as failure:
        // the reference wazero glue (spike/wazero.go) and the wasmtime backend
        // both ignore it and only fail when the call traps, and no ABI note gives
        // a nonzero value a failure meaning. A trap here is caught by `guard`.
        let _wasm_init_status: i32 = guest.func15923();
        *out = Box::into_raw(Box::new(Inst {
            guest,
            poisoned: false,
        }));
        OK
    })
}

/// Releases an instance created by [`guest_new`]. A null pointer is ignored. Also
/// the only call permitted on a poisoned instance.
///
/// Returns nothing (the C signature is unchanged for compatibility); a teardown
/// that traps is reported on stderr instead of being silently dropped, since the
/// caller is typically a destructor with no way to act on a status.
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
    let status = guard(|| {
        // SAFETY: `inst` came from `guest_new`'s `Box::into_raw` and is not aliased.
        drop(unsafe { Box::from_raw(inst) });
        OK
    });
    if status != OK {
        report_free_failure(status);
    }
}

/// Reports a failed teardown on stderr. `eprintln!` panics when stderr is closed
/// or broken, and a panic escaping an `extern "C"` function aborts the host, so
/// the write runs under [`guard`]. If that write itself fails there is no channel
/// left to report on and the function returns nothing, so its status is dropped
/// deliberately.
fn report_free_failure(status: i32) {
    guard(|| {
        eprintln!("guest-ffi: guest_free_instance failed (status {status}); instance leaked");
        OK
    });
}

/// Allocates `len` bytes in guest memory, writing the offset to `*out`. A NULL
/// result from the guest allocator for a non-empty request (its out-of-memory
/// signal) is `ERR_ALLOC` and leaves `*out` untouched.
///
/// # Safety
/// `inst` must be a live instance and `out` a writable `*mut u32`.
#[expect(
    unsafe_code,
    reason = "C-ABI export: exported symbol, raw pointers dereferenced behind null checks"
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn guest_alloc(inst: *mut Inst, len: u32, out: *mut u32) -> i32 {
    // SAFETY: the caller guarantees `inst` is live and `out` writable if non-null.
    unsafe {
        with_instance(inst, |inst| {
            let Some(out) = borrow_mut(out) else {
                return ERR_MEMORY;
            };
            let ptr = inst.guest.func61(len.cast_signed()).cast_unsigned();
            if ptr == 0 && len != 0 {
                return ERR_ALLOC;
            }
            *out = ptr;
            OK
        })
    }
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
    // SAFETY: the caller guarantees `inst` is live if non-null.
    unsafe {
        with_instance(inst, |inst| {
            inst.guest.func62(ptr.cast_signed());
            OK
        })
    }
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
    // SAFETY: the caller guarantees `inst` is live and `data`/`data_len`
    // describe a readable range.
    unsafe {
        with_instance(inst, |inst| {
            let Some(data) = bytes_from(data, data_len) else {
                return ERR_MEMORY;
            };
            let Ok(offset) = usize::try_from(ptr) else {
                return ERR_MEMORY;
            };
            let Some(end) = offset.checked_add(data.len()) else {
                return ERR_MEMORY;
            };
            let mem = inst.guest.memory();
            let Some(dst) = mem.get_mut(offset..end) else {
                return ERR_MEMORY;
            };
            dst.copy_from_slice(data);
            OK
        })
    }
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
    // SAFETY: the caller guarantees `inst` is live and `out_buf` covers `len`
    // writable bytes.
    unsafe {
        with_instance(inst, |inst| {
            let Ok(offset) = usize::try_from(ptr) else {
                return ERR_MEMORY;
            };
            let Ok(len) = usize::try_from(len) else {
                return ERR_MEMORY;
            };
            let Some(out) = bytes_from_mut(out_buf, len) else {
                return ERR_MEMORY;
            };
            let Some(end) = offset.checked_add(len) else {
                return ERR_MEMORY;
            };
            let mem = inst.guest.memory();
            let Some(slice) = mem.get(offset..end) else {
                return ERR_MEMORY;
            };
            out.copy_from_slice(slice);
            OK
        })
    }
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
    // SAFETY: the caller guarantees `inst` is live and `out` writable if non-null.
    unsafe {
        with_instance(inst, |inst| {
            let Some(out) = borrow_mut(out) else {
                return ERR_MEMORY;
            };
            let Some(func) = dispatch::packed_by_rpc::<HostImports>(svc, mid) else {
                return ERR_NO_EXPORT;
            };
            *out = func(&mut inst.guest, ptr.cast_signed(), len.cast_signed()).cast_unsigned();
            OK
        })
    }
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
    // SAFETY: the caller guarantees `inst` is live, `out` writable, and
    // `name_ptr`/`name_len` a readable range.
    unsafe {
        with_instance(inst, |inst| {
            let (Some(out), Some(name_bytes)) = (borrow_mut(out), bytes_from(name_ptr, name_len))
            else {
                return ERR_MEMORY;
            };
            let Ok(name) = std::str::from_utf8(name_bytes) else {
                return ERR_MEMORY;
            };
            let Some(func) = dispatch::packed_by_name::<HostImports>(name) else {
                return ERR_NO_EXPORT;
            };
            *out = func(&mut inst.guest, ptr.cast_signed(), len.cast_signed()).cast_unsigned();
            OK
        })
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test code")]
mod tests {
    use super::{
        ERR_MEMORY, ERR_PANIC, ERR_POISONED, OK, Poisonable, borrow_mut, bytes_from,
        bytes_from_mut, run_on_instance,
    };

    #[derive(Default)]
    struct Fake {
        poisoned: bool,
        calls: u32,
    }

    impl Poisonable for Fake {
        fn is_poisoned(&self) -> bool {
            self.poisoned
        }
        fn poison(&mut self) {
            self.poisoned = true;
        }
    }

    #[test]
    fn bytes_from_empty_accepts_null() {
        // SAFETY: a zero length never dereferences the pointer.
        #[expect(unsafe_code, reason = "exercising the unsafe helper")]
        let s = unsafe { bytes_from(std::ptr::null(), 0) };
        assert_eq!(s, Some(&[][..]));
    }

    #[test]
    fn bytes_from_views_the_range() {
        let data = [1u8, 2, 3, 4];
        // SAFETY: `data` is 4 readable bytes outliving the borrow.
        #[expect(unsafe_code, reason = "exercising the unsafe helper")]
        let s = unsafe { bytes_from(data.as_ptr(), data.len()) };
        assert_eq!(s, Some(&data[..]));
    }

    #[test]
    fn bytes_from_rejects_null_with_length_and_oversize() {
        // SAFETY: both calls are rejected before any dereference.
        #[expect(unsafe_code, reason = "exercising the unsafe helper")]
        let (null, huge) = unsafe {
            (
                bytes_from(std::ptr::null(), 1),
                bytes_from(std::ptr::NonNull::<u8>::dangling().as_ptr(), usize::MAX),
            )
        };
        assert_eq!(null, None);
        assert_eq!(huge, None);
    }

    #[test]
    fn bytes_from_mut_writes_through() {
        let mut data = [0u8; 3];
        // SAFETY: `data` is 3 writable bytes, unaliased for the borrow.
        #[expect(unsafe_code, reason = "exercising the unsafe helper")]
        let s = unsafe { bytes_from_mut(data.as_mut_ptr(), 3) }.expect("valid range");
        s.copy_from_slice(&[7, 8, 9]);
        assert_eq!(data, [7, 8, 9]);
    }

    #[test]
    fn bytes_from_mut_empty_and_invalid() {
        // SAFETY: zero length and rejected inputs never dereference the pointer.
        #[expect(unsafe_code, reason = "exercising the unsafe helper")]
        let (empty, null, huge) = unsafe {
            (
                bytes_from_mut(std::ptr::null_mut(), 0).map(|s| s.len()),
                bytes_from_mut(std::ptr::null_mut(), 1).map(|s| s.len()),
                bytes_from_mut(std::ptr::NonNull::<u8>::dangling().as_ptr(), usize::MAX)
                    .map(|s| s.len()),
            )
        };
        assert_eq!(empty, Some(0));
        assert_eq!(null, None);
        assert_eq!(huge, None);
    }

    #[test]
    fn borrow_mut_null_is_none_and_non_null_aliases_target() {
        // SAFETY: null is handled without dereference.
        #[expect(unsafe_code, reason = "exercising the unsafe helper")]
        let none = unsafe { borrow_mut::<u32>(std::ptr::null_mut()) };
        assert!(none.is_none());

        let mut v = 1u32;
        // SAFETY: `&raw mut v` is valid and not otherwise used during the borrow.
        #[expect(unsafe_code, reason = "exercising the unsafe helper")]
        let r = unsafe { borrow_mut(&raw mut v) }.expect("non-null");
        *r = 5;
        assert_eq!(v, 5);
    }

    #[test]
    fn run_on_instance_null_is_memory_error() {
        let status = run_on_instance::<Fake>(None, |_| OK);
        assert_eq!(status, ERR_MEMORY);
    }

    #[test]
    fn run_on_instance_passes_status_through_without_poisoning() {
        let mut f = Fake::default();
        let status = run_on_instance(Some(&mut f), |f| {
            f.calls += 1;
            ERR_MEMORY
        });
        assert_eq!(status, ERR_MEMORY);
        assert!(!f.poisoned);
        assert_eq!(run_on_instance(Some(&mut f), |_| OK), OK);
        assert_eq!(f.calls, 1);
    }

    #[test]
    fn run_on_instance_poisoned_rejects_without_running_body() {
        let mut f = Fake {
            poisoned: true,
            calls: 0,
        };
        let status = run_on_instance(Some(&mut f), |f| {
            f.calls += 1;
            OK
        });
        assert_eq!(status, ERR_POISONED);
        assert_eq!(f.calls, 0);
    }

    #[test]
    #[expect(clippy::panic, reason = "simulates a guest trap")]
    fn run_on_instance_panic_poisons_for_later_calls() {
        let mut f = Fake::default();
        let status = run_on_instance(Some(&mut f), |_| panic!("trap"));
        assert_eq!(status, ERR_PANIC);
        assert!(f.poisoned);
        assert_eq!(run_on_instance(Some(&mut f), |_| OK), ERR_POISONED);
    }
}
