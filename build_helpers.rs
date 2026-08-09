// Pure helpers shared by `build.rs` and its tests for locating the prebuilt
// `guest-ffi` cdylib: per-target asset names and pinned checksums. Kept free of
// I/O so the naming and lookup logic is unit-testable — a build script is not a
// test target, so both `build.rs` and `tests/build_helpers.rs` `include!` this
// file rather than importing it. (Plain `//` comments, not `//!`: an `include!`d
// file is spliced mid-module, where inner doc comments are not allowed.)

/// SHA256 of the *decompressed* cdylib for each shipped target — the artifact
/// that actually gets linked, so a corrupt or wrong-target download is caught
/// before it reaches the linker.
///
/// The release workflow (`.github/workflows/native-ffi-release.yml`) rebuilds
/// every target from the pinned inputs and fills these in when it publishes the
/// `native-ffi-v*` assets; until a release exists the entries are the
/// [`NATIVE_FFI_SHA_PENDING`] sentinel and only the `GUEST_FFI_LIB` path (a
/// locally built cdylib) works. A target absent from this table has no prebuilt
/// and must be built from source.
const NATIVE_FFI_SHA256: &[(&str, &str)] = &[
    ("aarch64-apple-darwin", NATIVE_FFI_SHA_PENDING),
    ("x86_64-unknown-linux-gnu", NATIVE_FFI_SHA_PENDING),
];

/// Placeholder pin for a target whose prebuilt cdylib has not been published
/// yet. `build.rs` treats it as "no usable download", so a build without
/// `GUEST_FFI_LIB` fails with a clear message instead of fetching a bad asset.
const NATIVE_FFI_SHA_PENDING: &str = "PENDING_RELEASE";

/// The shared-library file extension for `target`'s OS — Mach-O `dylib`, PE
/// `dll`, or ELF `so` — derived from the triple so no target-cfg env is needed.
fn native_ffi_dylib_ext(target: &str) -> &'static str {
    if target.contains("-apple-") {
        "dylib"
    } else if target.contains("-windows-") {
        "dll"
    } else {
        "so"
    }
}

/// The local (decompressed) cdylib file name for `target`, matching what the
/// linker looks up for `-l dylib=guest_ffi` (`lib`-prefixed on unix, bare `.dll`
/// on windows).
fn native_ffi_dylib_filename(target: &str) -> String {
    if target.contains("-windows-") {
        "guest_ffi.dll".to_string()
    } else {
        format!("libguest_ffi.{}", native_ffi_dylib_ext(target))
    }
}

/// The Release asset file name for `target`: the platform cdylib, zstd-compressed.
/// Carries the full triple so a single Release can hold every target side by side.
fn native_ffi_asset_name(target: &str) -> String {
    format!("libguest_ffi-{target}.{}.zst", native_ffi_dylib_ext(target))
}

/// The pinned decompressed-cdylib SHA256 for `target`, or `None` if no prebuilt
/// is shipped for it. Returns the [`NATIVE_FFI_SHA_PENDING`] sentinel for a
/// shipped-but-not-yet-released target; callers must treat that as "not usable".
fn native_ffi_sha256(target: &str) -> Option<&'static str> {
    NATIVE_FFI_SHA256
        .iter()
        .find(|(t, _)| *t == target)
        .map(|(_, sha)| *sha)
}
