//! Makes the shared library refer to *itself* by a relocatable name so a
//! consumer records a portable reference instead of this build machine's
//! absolute output path.
//!
//! By default rustc/the linker leaves a cdylib's Mach-O `LC_ID_DYLIB`
//! (install_name) at the build output path. A binary linked against it then
//! hard-codes that path — on CI, `/Users/runner/work/.../libguest_ffi.dylib` —
//! and dies with `dyld: Library not loaded` anywhere else. Setting the
//! install_name to `@rpath/libguest_ffi.dylib` makes the consumer reference the
//! library through its own rpaths instead (the consumer adds those; see
//! `docs/NATIVE.md`). On ELF the analogous `DT_SONAME` keeps the recorded
//! `DT_NEEDED` entry a bare leaf name, likewise resolvable via the consumer's
//! rpath / `LD_LIBRARY_PATH` rather than an absolute path.
//!
//! This build script belongs to the cdylib crate, so its `rustc-link-arg`
//! applies to the cdylib's own link. (A `rustc-link-arg` cannot cross into a
//! *consumer's* binary link, which is why the rpath is the consumer's job and
//! only the install_name is fixed here.)

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "macos" | "ios" | "tvos" | "watchos" | "visionos" => {
            println!("cargo::rustc-link-arg=-Wl,-install_name,@rpath/libguest_ffi.dylib");
        }
        // A Windows DLL is located by its own search rules (next to the exe / on
        // PATH), not an embedded name, so there is nothing to fix.
        "windows" => {}
        // ELF (Linux, *BSD, …): pin the SONAME to the leaf name.
        _ => {
            println!("cargo::rustc-link-arg=-Wl,-soname,libguest_ffi.so");
        }
    }
}
