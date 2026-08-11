//! Prepares the GoogleSQL prebuilt WebAssembly module at build time.
//!
//! Resolution priority:
//! 1. Local file pointed to by the `GOOGLESQL_WASM` environment variable (offline / development)
//! 2. A verified cached copy in `OUT_DIR`
//! 3. Download from GitHub Releases via `curl`
//!
//! In all cases the SHA256 is checked against the pinned value before the file is used.

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// Version and integrity metadata for the bundled googlesql.wasm (goccy/googlesql-wasm).
const WASM_SHA256: &str = "5f14b3a74a9bb4e333b03e8420b11b633a1b77379053f02e44235abed08ae407";
const WASM_URL: &str =
    "https://github.com/goccy/googlesql-wasm/releases/download/v0.3.4/googlesql.wasm";

/// The Release tag whose prebuilt `guest-ffi` cdylib assets this build expects.
/// Bumped whenever the cdylib is regenerated (new googlesql.wasm or a change to
/// the `native/guest-ffi` wrapper), in lockstep with the pins in
/// `build_helpers.rs`.
const NATIVE_FFI_TAG: &str = "native-ffi-v0.1.0";

/// Base URL the per-target `guest-ffi` cdylib assets are downloaded from; the
/// full URL is `{base}/{tag}/{asset}`. Overridable via `GUEST_FFI_URL_BASE`
/// (a mirror, or a `file://` directory for offline builds and tests).
const NATIVE_FFI_RELEASE_BASE: &str = "https://github.com/hirosassa/googlesql-rs/releases/download";

// Pure asset-naming and checksum-lookup helpers, shared with the unit tests in
// `tests/build_helpers.rs` (a build script is not a test target, so the logic is
// factored out and `include!`d rather than imported).
include!("build_helpers.rs");

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=GOOGLESQL_WASM");
    println!("cargo::rerun-if-env-changed=DOCS_RS");

    // Under the `native-ffi` feature, link the prebuilt C-ABI shared library
    // (`libguest_ffi.{dylib,so}` / `guest_ffi.dll`, built separately from
    // `native/guest-ffi`). `GUEST_FFI_LIB` points at the directory holding it —
    // the local stand-in for what a Release download would provide, mirroring the
    // wasm's fetch-then-use model above. A cdylib (not a staticlib) keeps its
    // bundled `std` internal, so a consumer built with a different rustc links
    // cleanly instead of hitting a duplicate `rust_eh_personality`.
    if env::var_os("CARGO_FEATURE_NATIVE_FFI").is_some() {
        link_guest_ffi()?;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let dest = out_dir.join("googlesql.wasm");
    let cwasm_dest = out_dir.join("googlesql.cwasm");

    // docs.rs builds run offline and only generate documentation — they never
    // execute the crate, and the wasm is loaded lazily at runtime (not embedded
    // at compile time). Skip fetching it there so the build doesn't fail on the
    // network; only the env vars below need to be set for compilation.
    if env::var_os("DOCS_RS").is_none() {
        let bytes = resolve_wasm_bytes(&dest)?;
        verify_sha256(&bytes)?;
        fs::write(&dest, &bytes)?;
        precompile_cwasm(&bytes, &cwasm_dest)?;
    }

    // Expose the absolute paths to the wasm and its precompiled artifact for use
    // at runtime.
    println!("cargo::rustc-env=GOOGLESQL_WASM_PATH={}", dest.display());
    println!(
        "cargo::rustc-env=GOOGLESQL_CWASM_PATH={}",
        cwasm_dest.display()
    );
    Ok(())
}

/// Resolves the prebuilt `guest-ffi` cdylib for the `native-ffi` backend and
/// emits its link directives. Resolution mirrors the wasm's priority above:
///
/// 1. `GUEST_FFI_LIB` — a directory holding a locally built cdylib (development,
///    offline, or a target with no published prebuilt). Used as-is.
/// 2. A verified cached copy in `OUT_DIR` from an earlier build.
/// 3. Download the target's asset from the `native-ffi-v*` Release, decompress
///    it, and verify its SHA256 before caching it in `OUT_DIR`.
///
/// In every case the resolved directory is baked as an `-rpath` (so *this
/// crate's own* native-ffi tests/examples find the cdylib) and also handed to
/// consumers via `DEP_GUEST_FFI_LIBDIR` (this crate's `links = "guest_ffi"`).
/// A build script's `rustc-link-arg` reaches only its own crate's targets,
/// never a downstream binary's, so a consumer resolves the library from the
/// metadata with its own `-rpath` (see docs/NATIVE.md).
fn link_guest_ffi() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-env-changed=GUEST_FFI_LIB");
    println!("cargo::rerun-if-env-changed=GUEST_FFI_URL_BASE");
    let target = env::var("TARGET")?;
    let filename = native_ffi_dylib_filename(&target);

    // 1. Local override: link straight from the directory it names.
    if let Ok(dir) = env::var("GUEST_FFI_LIB") {
        let lib = Path::new(&dir).join(&filename);
        if !lib.is_file() {
            return Err(format!("GUEST_FFI_LIB={dir} does not contain {filename}").into());
        }
        emit_guest_ffi_link(&dir, &target);
        return Ok(());
    }

    // 2/3. No override: the target must have a published prebuilt to download.
    let expected_sha = native_ffi_sha256(&target)
        .filter(|sha| *sha != NATIVE_FFI_SHA_PENDING)
        .ok_or_else(|| {
            format!(
                "no prebuilt guest-ffi cdylib is available for target {target}; build it from \
                 native/guest-ffi (`cargo build --release`) and point GUEST_FFI_LIB at the output"
            )
        })?;
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let lib_dest = out_dir.join(&filename);
    ensure_guest_ffi_dylib(&lib_dest, &target, expected_sha)?;
    emit_guest_ffi_link(&out_dir.to_string_lossy(), &target);
    Ok(())
}

/// Emits the search-path and link directives that make the linker resolve
/// `guest_ffi` from `dir`, an `-rpath` to it, and publishes `dir` to consumers
/// as `DEP_GUEST_FFI_LIBDIR` (via this crate's `links = "guest_ffi"`).
///
/// The `-rpath` serves only *this crate's own* targets — its `native-ffi`
/// integration tests and examples, which link the cdylib and must find it at
/// run time. A build script's `rustc-link-arg` never crosses into a downstream
/// binary's link, so a consumer cannot rely on it: it reads
/// `DEP_GUEST_FFI_LIBDIR` in its own build script and emits its own `-rpath`
/// (see docs/NATIVE.md). `-rpath` is a GNU/Mach-O concept; a Windows DLL is
/// found by its own search rules, so it is skipped there.
fn emit_guest_ffi_link(dir: &str, target: &str) {
    println!("cargo::rustc-link-search=native={dir}");
    println!("cargo::rustc-link-lib=dylib=guest_ffi");
    if !target.contains("-windows-") {
        println!("cargo::rustc-link-arg=-Wl,-rpath,{dir}");
    }
    println!("cargo::metadata=libdir={dir}");
}

/// Ensures `dest` holds the decompressed cdylib whose SHA256 is `expected_sha`,
/// reusing a matching cached copy or downloading and verifying the Release asset.
fn ensure_guest_ffi_dylib(
    dest: &Path,
    target: &str,
    expected_sha: &str,
) -> Result<(), Box<dyn Error>> {
    // Reuse a cached copy whose checksum still matches the pin.
    if let Ok(cached) = fs::read(dest)
        && sha256_hex(&cached) == expected_sha
    {
        return Ok(());
    }

    let base =
        env::var("GUEST_FFI_URL_BASE").unwrap_or_else(|_| NATIVE_FFI_RELEASE_BASE.to_string());
    let asset = native_ffi_asset_name(target);
    let url = format!("{base}/{NATIVE_FFI_TAG}/{asset}");
    let out_dir = dest
        .parent()
        .ok_or("guest-ffi dylib dest has no parent dir")?;
    let compressed = out_dir.join(&asset);
    download_with_curl(&url, &compressed)?;

    let packed =
        fs::read(&compressed).map_err(|e| format!("cannot read {}: {e}", compressed.display()))?;
    // The compressed asset has served its purpose; drop it (best-effort) so it
    // doesn't linger in OUT_DIR beside the decompressed cdylib the cache keys off.
    fs::remove_file(&compressed).ok();
    let bytes = zstd::decode_all(packed.as_slice())
        .map_err(|e| format!("cannot zstd-decode {asset}: {e}"))?;
    verify_sha256_against(&format!("guest-ffi {target} cdylib"), &bytes, expected_sha)?;
    fs::write(dest, &bytes).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    Ok(())
}

/// Precompiles the wasm into a serialized `.cwasm` so the runtime deserializes
/// native code instead of JIT-compiling on first use.
///
/// The engine here must match the runtime's `Engine::default()` exactly (same
/// wasmtime version and default features, guaranteed by Cargo) — otherwise the
/// runtime's `deserialize` rejects the artifact and falls back to JIT. A failure
/// here is fatal: the artifact is required, and the runtime test asserts it
/// deserializes.
fn precompile_cwasm(wasm: &[u8], dest: &Path) -> Result<(), Box<dyn Error>> {
    let engine = wasmtime::Engine::default();
    let cwasm = engine
        .precompile_module(wasm)
        .map_err(|e| format!("precompile googlesql.wasm: {e}"))?;
    fs::write(dest, &cwasm).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    Ok(())
}

/// Resolves the wasm bytes according to the priority order (verification is left to the caller).
fn resolve_wasm_bytes(dest: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    if let Ok(local) = env::var("GOOGLESQL_WASM") {
        let bytes =
            fs::read(&local).map_err(|e| format!("cannot read GOOGLESQL_WASM={local}: {e}"))?;
        return Ok(bytes);
    }

    if let Ok(cached) = fs::read(dest)
        && sha256_hex(&cached) == WASM_SHA256
    {
        return Ok(cached);
    }

    download_with_curl(WASM_URL, dest)?;
    let bytes = fs::read(dest).map_err(|e| format!("cannot read downloaded wasm: {e}"))?;
    Ok(bytes)
}

/// Downloads `url` to `dest` using `curl` (shared by the wasm and cdylib paths).
fn download_with_curl(url: &str, dest: &Path) -> Result<(), Box<dyn Error>> {
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("cannot launch curl: {e}"))?;
    if !status.success() {
        return Err(format!("curl failed to download {url}").into());
    }
    Ok(())
}

/// Verifies that the SHA256 of the bundled googlesql.wasm matches its pin.
fn verify_sha256(bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    verify_sha256_against("googlesql.wasm", bytes, WASM_SHA256)
}

/// Verifies that the SHA256 of `bytes` equals `expected`, naming `what` in the
/// mismatch error. Shared by the wasm and prebuilt-cdylib integrity checks.
fn verify_sha256_against(what: &str, bytes: &[u8], expected: &str) -> Result<(), Box<dyn Error>> {
    let actual = sha256_hex(bytes);
    if actual != expected {
        return Err(format!("{what} SHA256 mismatch (expected {expected}, got {actual})").into());
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
