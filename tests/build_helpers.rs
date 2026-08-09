//! Unit tests for the pure `build.rs` helpers (prebuilt-cdylib asset naming and
//! pinned checksums). A build script is not itself a test target, so the pure
//! logic lives in `build_helpers.rs` and is `include!`d both there and here so it
//! can be exercised directly.
#![allow(
    clippy::unwrap_used,
    reason = "test code: a missing pin should fail the test loudly"
)]
#![allow(
    dead_code,
    reason = "build_helpers.rs is shared via include!; the release-only sentinel is not read here"
)]

include!("../build_helpers.rs");

/// The asset name carries the full target triple (so one Release holds every
/// platform) and the platform's shared-library extension ahead of `.zst`.
#[test]
fn asset_name_encodes_target_and_platform_extension() {
    assert_eq!(
        native_ffi_asset_name("aarch64-apple-darwin"),
        "libguest_ffi-aarch64-apple-darwin.dylib.zst"
    );
    assert_eq!(
        native_ffi_asset_name("x86_64-unknown-linux-gnu"),
        "libguest_ffi-x86_64-unknown-linux-gnu.so.zst"
    );
    assert_eq!(
        native_ffi_asset_name("x86_64-pc-windows-msvc"),
        "libguest_ffi-x86_64-pc-windows-msvc.dll.zst"
    );
}

/// The decompressed file name must be exactly what `-l dylib=guest_ffi` looks up
/// on each platform (`lib`-prefixed on unix, bare on windows).
#[test]
fn dylib_filename_matches_linker_lookup() {
    assert_eq!(
        native_ffi_dylib_filename("aarch64-apple-darwin"),
        "libguest_ffi.dylib"
    );
    assert_eq!(
        native_ffi_dylib_filename("x86_64-unknown-linux-gnu"),
        "libguest_ffi.so"
    );
    assert_eq!(
        native_ffi_dylib_filename("x86_64-pc-windows-msvc"),
        "guest_ffi.dll"
    );
}

/// Every shipped target has a pin entry; an unshipped target resolves to `None`
/// so `build.rs` can emit a "build from source" error instead of a bad download.
#[test]
fn sha256_pinned_for_shipped_targets_only() {
    for target in ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"] {
        assert!(
            native_ffi_sha256(target).is_some(),
            "shipped target {target} must have a pin entry"
        );
    }
    assert!(native_ffi_sha256("riscv64gc-unknown-linux-gnu").is_none());
}
