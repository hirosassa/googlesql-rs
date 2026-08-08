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
const WASM_VERSION: &str = "v0.3.4";
const WASM_SHA256: &str = "5f14b3a74a9bb4e333b03e8420b11b633a1b77379053f02e44235abed08ae407";
const WASM_URL: &str =
    "https://github.com/goccy/googlesql-wasm/releases/download/v0.3.4/googlesql.wasm";

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=GOOGLESQL_WASM");
    println!("cargo::rerun-if-env-changed=DOCS_RS");

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

/// Downloads `url` to `dest` using `curl`.
fn download_with_curl(url: &str, dest: &Path) -> Result<(), Box<dyn Error>> {
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("cannot launch curl ({WASM_VERSION}): {e}"))?;
    if !status.success() {
        return Err(format!("curl failed to download {url}").into());
    }
    Ok(())
}

/// Verifies that the SHA256 of `bytes` matches the pinned value.
fn verify_sha256(bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    let actual = sha256_hex(bytes);
    if actual != WASM_SHA256 {
        return Err(format!(
            "googlesql.wasm SHA256 mismatch (expected {WASM_SHA256}, got {actual})"
        )
        .into());
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
