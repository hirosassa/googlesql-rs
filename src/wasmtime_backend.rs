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
use std::path::Path;
#[cfg(any(windows, feature = "bundled-tzdb"))]
use std::path::PathBuf;
use std::sync::OnceLock;

use wasmtime::{
    Caller, Engine, Extern, ExternType, Instance, Linker, Memory, Module as WasmModule, Store,
    TypedFunc, Val, ValType,
};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::{FsPerms, WasiCtxBuilder};

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

/// Guest path at which the tz database is exposed inside the sandbox.
///
/// cctz (reached through absl's `FindTimeZoneByName` on the analyzer path)
/// resolves named zones under `TZDIR`; this is the path the wasm build expects.
const GUEST_TZDIR: &str = "/usr/share/zoneinfo";

/// Version marker file in a materialized tz database directory.
///
/// Written last, containing the tz database version: a directory whose marker
/// is absent or stale is a half-written leftover from a crashed process and is
/// rebuilt from scratch.
#[cfg(any(windows, feature = "bundled-tzdb"))]
const TZDB_MARKER: &str = ".complete";

/// Materialized bundled tz database, resolved at most once per process.
///
/// `Err` caches why materialization failed, so repeated instance construction
/// neither retries a doomed write nor loses the reason.
#[cfg(any(windows, feature = "bundled-tzdb"))]
static BUNDLED_TZDB: OnceLock<Result<PathBuf, String>> = OnceLock::new();

/// Returns the directory of the bundled tz database, materializing it on first
/// use.
///
/// Time zone policy for the analyzer (cctz resolves named zones under `TZDIR`):
/// Windows always bundles — its hosts have no `/usr/share/zoneinfo` at all, and
/// that absence made every analyze call trap (wasm `unreachable` in cctz) —
/// while unix defaults to the host database, whose local modifications are
/// honored, and opts into this hermetic path with the `bundled-tzdb` feature:
/// every host then analyzes the same tz data, whatever it ships locally.
///
/// Concretely, the [`tzdb`](https://docs.rs/tzdb) crate ships the IANA database
/// in the binary; this writes each zone out as a TZif file under
/// `<cache root>/<tz version>/` once per version (guarded by the
/// [`TZDB_MARKER`] file) and the directory is preopened at the guest path.
///
/// The error names the materialization action so the caller can decide how to
/// survive it; see `WasmtimeInstance::new` for the unix host-database fallback
/// and the windows hard failure.
#[cfg(any(windows, feature = "bundled-tzdb"))]
fn bundled_tzdb_dir() -> Result<&'static Path, &'static str> {
    BUNDLED_TZDB
        .get_or_init(|| write_bundled_tzdb(&tzdb_cache_root()))
        .as_ref()
        .map(PathBuf::as_path)
        .map_err(String::as_str)
}

/// Root under which materialized tz database versions are cached.
///
/// Windows `%TEMP%` is already per-user and access-controlled by the OS. Unix
/// `/tmp` is shared and world-writable, so a predictable path there could be
/// pre-created by another local user with crafted zones; the per-UID suffix
/// plus the ownership check in [`dir_is_private`] keep the cache ours alone.
#[cfg(any(windows, feature = "bundled-tzdb"))]
fn tzdb_cache_root() -> PathBuf {
    #[cfg(unix)]
    let user = {
        #[allow(
            unsafe_code,
            reason = "getuid is a pure libc query with no preconditions; std exposes no uid API"
        )]
        let uid = unsafe { libc::getuid() };
        format!("-{uid}")
    };
    // Windows needs no suffix: `%TEMP%` is per-user already.
    #[cfg(windows)]
    let user = "";
    std::env::temp_dir().join(format!("googlesql-tzdb{user}"))
}

/// Writes every bundled zone out as a TZif file under `<root>/<tz version>/`
/// and returns that directory.
///
/// The tree is built under a `.tmp-<pid>` sibling and renamed into place, so a
/// concurrent reader sees either no directory at all or a fully written one —
/// never a zone file truncated mid-write by a rebuilding process (plain
/// `fs::write` would truncate in place). The marker is written last inside the
/// staged tree, and a writer that loses the rename race to a concurrent one
/// keeps the winner's tree, re-verified via [`tzdb_dir_complete`].
///
/// A previous run's untrusted or incomplete `<version>` directory (a crash
/// from before this staging existed, or a temp cleaner that deleted zones but
/// kept the marker) is torn down first — but only after re-checking
/// completeness, so a tree a concurrent process renamed into place while we
/// staged ours survives instead (that race is lost the same way as a failed
/// rename: the winner's tree is kept). The alternative, rewriting in place,
/// truncates files under readers on every rebuild.
///
/// Every directory is created without group/world write permission (see
/// [`create_tz_dirs`]) and every file with explicit `0644` (see
/// [`write_tzdb_file`]), so even a permissive umask cannot leave the cache
/// modifiable by others.
#[cfg(any(windows, feature = "bundled-tzdb"))]
fn write_bundled_tzdb(root: &Path) -> Result<PathBuf, String> {
    let dir = root.join(tzdb::VERSION);
    if tzdb_dir_complete(&dir) {
        return Ok(dir);
    }
    // The cache root must be ours alone before anything is read from or
    // written into it: unix `/tmp` is world-writable and another user could
    // have pre-created this predictable path (windows `%TEMP%` is per-user,
    // where [`dir_is_private`] always passes).
    create_tz_dirs(root).map_err(|error| {
        format!(
            "materialize the bundled tz database: create {}: {error}",
            root.display()
        )
    })?;
    if !dir_is_private(root) {
        return Err(format!(
            "materialize the bundled tz database: cache root {} is owned by another user \
             or group/world-writable; not loading a possibly planted tz database",
            root.display()
        ));
    }
    // Staged tree of this process; a leftover from an earlier crashed run of
    // this pid (or a recycled pid) would make the write below incomplete.
    let staged = root.join(format!("{}.tmp-{}", tzdb::VERSION, std::process::id()));
    remove_dir_best_effort(&staged);
    create_tz_dirs(&staged).map_err(|error| {
        format!(
            "materialize the bundled tz database: create {}: {error}",
            staged.display()
        )
    })?;
    for &name in tzdb::TZ_NAMES {
        let zone = staged.join(name);
        create_tz_dirs(zone.parent().unwrap_or(&staged)).map_err(|error| {
            format!(
                "materialize the bundled tz database: create {}: {error}",
                zone.display()
            )
        })?;
        let bytes = tzdb::raw_tz_by_name(name).ok_or_else(|| {
            format!("materialize the bundled tz database: zone {name} missing from bundled data")
        })?;
        write_tzdb_file(&zone, bytes).map_err(|error| {
            format!(
                "materialize the bundled tz database: write {}: {error}",
                zone.display()
            )
        })?;
    }
    write_tzdb_file(&staged.join(TZDB_MARKER), tzdb::VERSION.as_bytes()).map_err(|error| {
        format!(
            "materialize the bundled tz database: write {}/{TZDB_MARKER}: {error}",
            staged.display()
        )
    })?;
    // Make room for the rename over an untrusted or incomplete leftover —
    // unless a concurrent process renamed a complete tree into `dir` while we
    // staged ours: tearing that down would fail readers that just trusted it,
    // and the race is lost exactly like a failed rename below — keep the
    // winner's tree and drop the one staged here.
    if tzdb_dir_complete(&dir) {
        remove_dir_best_effort(&staged);
        return Ok(dir);
    }
    remove_dir_best_effort(&dir);
    match std::fs::rename(&staged, &dir) {
        Ok(()) => Ok(dir),
        // Lost a race with a concurrent writer whose tree is complete: it is
        // equivalent to ours, so accept it and drop the one staged here.
        Err(_) if tzdb_dir_complete(&dir) => {
            remove_dir_best_effort(&staged);
            Ok(dir)
        }
        Err(error) => Err(format!(
            "materialize the bundled tz database: rename {} -> {}: {error}",
            staged.display(),
            dir.display()
        )),
    }
}

/// Whether `dir` holds a complete materialization of the bundled tz database
/// for the running tzdb version — and, on unix, is ours alone.
///
/// The marker proves a writer finished (it is written last inside the staged
/// tree); the representative `UTC` zone additionally guards against a temp
/// cleaner deleting zone files while leaving the marker, which would otherwise
/// bring the analyzer trap right back.
#[cfg(any(windows, feature = "bundled-tzdb"))]
fn tzdb_dir_complete(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(TZDB_MARKER)).is_ok_and(|done| done == tzdb::VERSION)
        && dir.join("UTC").is_file()
        && dir_is_private(dir)
}

/// Whether `dir` is owned by the current user and not writable by anyone else.
///
/// Only meaningful where the temp directory is shared: unix `/tmp`. Windows
/// `%TEMP%` is per-user and access-controlled by the OS, so everything
/// reachable there is trusted outright.
// Compiled only under `bundled-tzdb` (matching its callers): plain unix
// wasmtime builds never materialize a cache and do not pull `libc`.
#[cfg(all(unix, feature = "bundled-tzdb"))]
fn dir_is_private(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(dir).is_ok_and(|meta| {
        #[allow(
            unsafe_code,
            reason = "getuid is a pure libc query with no preconditions; std exposes no uid API"
        )]
        let uid = unsafe { libc::getuid() };
        uid == meta.uid() && meta.mode() & 0o022 == 0
    })
}

/// Windows twin of [`dir_is_private`]: `%TEMP%` is per-user and ACL-controlled,
/// so there is nothing to verify.
#[cfg(windows)]
const fn dir_is_private(_dir: &Path) -> bool {
    true
}

/// Removes `dir` recursively, best-effort.
///
/// Every call site discards the outcome deliberately: a stale staged tree
/// only costs disk space, and a failed teardown of an untrusted leftover
/// surfaces at the next real filesystem operation instead.
#[cfg(any(windows, feature = "bundled-tzdb"))]
fn remove_dir_best_effort(dir: &Path) {
    #[allow(
        clippy::let_underscore_must_use,
        reason = "best-effort cleanup; a real failure surfaces at the next operation"
    )]
    let _ = std::fs::remove_dir_all(dir);
}

/// Creates `dir` and any missing parents without group/world write permission.
///
/// Unix directory creation applies the umask to `0777`, so a permissive umask
/// (e.g. 000 in some containers) would leave the cache group/world-writable —
/// both failing our own trust check on the next run and letting others rewrite
/// zones. `mode(0o755)` is still umask-masked, but since `0o755` carries no
/// group/world-write bits, the umask can only tighten it, never loosen it.
/// Windows needs nothing beyond `create_dir_all`: `%TEMP%` is per-user and
/// ACL-controlled.
// Compiled only under `bundled-tzdb`, matching its callers (see `dir_is_private`).
#[cfg(all(unix, feature = "bundled-tzdb"))]
fn create_tz_dirs(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .mode(0o755)
        .recursive(true)
        .create(dir)
}

/// Windows twin of [`create_tz_dirs`].
#[cfg(windows)]
fn create_tz_dirs(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Writes one file of the tz database (a zone, or the marker) with explicit
/// `0644` permissions.
///
/// Unix `fs::write` creates files as `0666 & !umask`, so a permissive umask
/// (e.g. 000 in some containers) would leave the zones rewritable by other
/// users even inside `0755` directories. `create_new` never truncates an
/// existing file, and like [`create_tz_dirs`] the mode is umask-masked but
/// `0o644` carries no group/world-write bits, so the umask can only tighten
/// it. Compiled only under `bundled-tzdb`, matching its callers.
#[cfg(all(unix, feature = "bundled-tzdb"))]
fn write_tzdb_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(path)?;
    file.write_all(bytes)
}

/// Windows twin of [`write_tzdb_file`]: `%TEMP%` is per-user and
/// ACL-controlled, so plain `fs::write` needs no pinned mode.
#[cfg(windows)]
fn write_tzdb_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

/// Preopens `host_dir` read-only at [`GUEST_TZDIR`], best-effort.
///
/// The required `/` preopen in [`WasmtimeInstance::new`] already covers the tz
/// directory, so a failure here is tolerable and discarded; the direct preopen
/// only hands the guest an unambiguous handle at the path cctz expects.
fn preopen_tzdir(builder: &mut WasiCtxBuilder, host_dir: &Path) {
    #[allow(
        clippy::let_underscore_must_use,
        reason = "optional tz preopen; the required \"/\" preopen is the guarantee"
    )]
    let _ = builder.preopened_dir(host_dir, GUEST_TZDIR, FsPerms::ReadOnly);
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
        // Time zone database provisioning for the analyzer (cctz resolves named
        // zones under `TZDIR`), split by platform:
        // - Windows always gets the bundled database (see `bundled_tzdb_dir`):
        //   the host has no /usr/share/zoneinfo at all, and its absence made
        //   every analyze call trap (cctz aborts with wasm `unreachable` when
        //   the database is unreachable). There is no host database to fall
        //   back to, so a failed materialization fails construction with the
        //   real reason instead of letting the analyzer trap opaquely later.
        // - unix defaults to the host's own database (the arm below), honoring
        //   locally-modified zoneinfo; the `bundled-tzdb` feature opts into the
        //   hermetic bundled database, keeping a host-database fallback for
        //   when its cache directory cannot be written.
        #[cfg(any(windows, feature = "bundled-tzdb"))]
        {
            builder.env("TZDIR", GUEST_TZDIR);
            match bundled_tzdb_dir() {
                Ok(dir) => preopen_tzdir(&mut builder, dir),
                #[cfg(unix)]
                Err(error) => {
                    // Hermetic tz is an opt-in on unix: the host's own database
                    // (when present) still beats failing instantiation.
                    eprintln!("googlesql: {error}; falling back to host {GUEST_TZDIR}");
                    if let Ok(real) = std::fs::canonicalize(GUEST_TZDIR) {
                        preopen_tzdir(&mut builder, &real);
                    }
                }
                #[cfg(windows)]
                Err(error) => {
                    return Err(Error::Instantiate(format!(
                        "tz database provisioning: {error}"
                    )));
                }
            }
        }
        #[cfg(all(unix, not(feature = "bundled-tzdb")))]
        {
            // Resolve the host tz database (may be a chain of symlinks on
            // macOS) and preopen the real directory directly at the guest path
            // cctz expects, so absl's FindTimeZoneByName works without
            // in-sandbox symlink traversal.
            if let Ok(real) = std::fs::canonicalize(GUEST_TZDIR) {
                builder.env("TZDIR", GUEST_TZDIR);
                preopen_tzdir(&mut builder, &real);
            }
        }
        let wasi = builder
            .preopened_dir("/", "/", FsPerms::ReadOnly)
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
    /// a distinct `(svc, mid)` adds its own entry. `NewParserOptions` (svc700/mid0)
    /// and `NewLanguageOptions` (svc679/mid0) both take an empty request, so they
    /// exercise the cache without any request marshaling.
    #[test]
    fn call_rpc_caches_typed_export_per_svc_mid() {
        const SVC_PARSER_OPTIONS: i32 = 700;
        const MID_NEW_PARSER_OPTIONS: i32 = 0;
        const SVC_LANGUAGE_OPTIONS: i32 = 679;
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

#[cfg(all(test, any(windows, feature = "bundled-tzdb")))]
#[allow(clippy::expect_used, reason = "test code")]
mod tzdb_tests {
    use super::{TZDB_MARKER, remove_dir_best_effort, tzdb_dir_complete, write_bundled_tzdb};

    /// A private cache root per test (parallel tests in one binary share a
    /// pid, so the test name carries the isolation), cleared before use.
    fn test_root(test: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir()
            .join("googlesql-tzdb-tests")
            .join(format!("{test}-{}", std::process::id()));
        remove_dir_best_effort(&root);
        root
    }

    /// Nested zone names (`America/Argentina/...`) materialize as real TZif
    /// files under subdirectories of the version directory, not flattened or
    /// skipped.
    #[test]
    fn nested_zone_names_materialize() {
        assert!(
            tzdb::TZ_NAMES.contains(&"America/Argentina/Buenos_Aires"),
            "the chosen probe must exist in the bundled database"
        );
        let root = test_root("nested");
        let dir = write_bundled_tzdb(&root).expect("first materialization");
        let bytes = std::fs::read(dir.join("America/Argentina/Buenos_Aires"))
            .expect("nested zone file must exist");
        assert!(
            bytes.starts_with(b"TZif"),
            "nested zone must be a TZif file"
        );
        remove_dir_best_effort(&root);
    }

    /// A complete cache is reused as-is: the second run must not rewrite it.
    /// Pinned by setting the UTC zone's mtime to a sentinel and checking it
    /// survived, plus the absence of any staged `.tmp-` sibling afterwards.
    #[test]
    fn rerun_is_idempotent() {
        let root = test_root("idempotent");
        let dir = write_bundled_tzdb(&root).expect("first materialization");
        let sentinel = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(dir.join("UTC"))
            .and_then(|file| file.set_modified(sentinel))
            .expect("pin the UTC zone's mtime");

        let dir_again = write_bundled_tzdb(&root).expect("second materialization");

        assert_eq!(
            dir, dir_again,
            "the same version directory must be returned"
        );
        let mtime = std::fs::metadata(dir.join("UTC"))
            .and_then(|meta| meta.modified())
            .expect("UTC zone metadata");
        assert_eq!(
            mtime, sentinel,
            "a complete cache must be reused, not rewritten"
        );
        let staged: Vec<_> = std::fs::read_dir(&root)
            .expect("read cache root")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(
            staged.is_empty(),
            "no staged tree may be left behind: {staged:?}"
        );
        remove_dir_best_effort(&root);
    }

    /// A directory whose marker is gone (a crash mid-materialization from
    /// before staging existed) is rebuilt from scratch, marker and all.
    #[test]
    fn missing_marker_rebuilds() {
        let root = test_root("no-marker");
        let dir = write_bundled_tzdb(&root).expect("first materialization");
        std::fs::remove_file(dir.join(TZDB_MARKER)).expect("remove the marker");

        let rebuilt = write_bundled_tzdb(&root).expect("rebuild");

        assert_eq!(dir, rebuilt);
        assert!(
            tzdb_dir_complete(&rebuilt),
            "the rebuilt dir must be complete"
        );
        remove_dir_best_effort(&root);
    }

    /// The marker alone is not trusted: a temp cleaner that deleted zone files
    /// but left the marker must not silently bring the analyzer trap back —
    /// the representative `UTC` probe rejects the directory and triggers a
    /// full rebuild.
    #[test]
    fn missing_zone_despite_marker_rebuilds() {
        let root = test_root("missing-zone");
        let dir = write_bundled_tzdb(&root).expect("first materialization");
        std::fs::remove_file(dir.join("UTC")).expect("remove the UTC zone");

        assert!(
            !tzdb_dir_complete(&dir),
            "a directory without UTC must not count as complete"
        );
        let rebuilt = write_bundled_tzdb(&root).expect("rebuild");

        assert!(
            rebuilt.join("UTC").is_file(),
            "the rebuilt directory must contain every zone again"
        );
        remove_dir_best_effort(&root);
    }

    /// Zone files are written with explicit `0644`, so a permissive umask
    /// cannot leave them modifiable by other users of a shared temp directory.
    #[cfg(unix)]
    #[test]
    fn zone_files_are_not_group_writable() {
        use std::os::unix::fs::PermissionsExt;
        let root = test_root("zone-file-modes");
        let dir = write_bundled_tzdb(&root).expect("first materialization");
        let mode = std::fs::metadata(dir.join("UTC"))
            .expect("UTC zone metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o022,
            0,
            "zone files must not be group/world-writable"
        );
        remove_dir_best_effort(&root);
    }

    /// A group/world-writable version directory at this predictable cache path
    /// is not trusted on unix (another local user could have planted it);
    /// materialization replaces it with an owner-restricted one.
    #[cfg(unix)]
    #[test]
    fn group_writable_dir_is_rebuilt() {
        use std::os::unix::fs::PermissionsExt;
        let root = test_root("group-writable");
        let dir = write_bundled_tzdb(&root).expect("first materialization");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o0777))
            .expect("make the version directory world-writable");

        assert!(
            !tzdb_dir_complete(&dir),
            "a group/world-writable directory must not count as complete"
        );
        let rebuilt = write_bundled_tzdb(&root).expect("rebuild");

        let mode = std::fs::metadata(&rebuilt)
            .expect("rebuilt metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o022,
            0,
            "the rebuilt directory must not be group/world-writable"
        );
        remove_dir_best_effort(&root);
    }
}
