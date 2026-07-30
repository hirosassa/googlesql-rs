//! `Module` is `Send`, so a wasm instance can move between threads and many
//! instances can run truly in parallel (one per thread). wasmtime forbids
//! concurrent calls into a single instance, so parallelism comes from separate
//! instances, not from sharing one.
#![allow(clippy::unwrap_used, reason = "test code")]

use googlesql::Module;

/// A statically-checked witness that `Module` implements `Send`; if the field
/// types regress to a non-`Send` shape (e.g. `Rc`), this stops compiling.
const fn assert_send<T: Send>() {}
const _: () = assert_send::<Module>();

/// A `Module` built on one thread can be moved to another and used there.
#[test]
fn module_moves_across_threads() {
    let module = Module::new().unwrap();

    let canonical = std::thread::spawn(move || {
        let mut module = module;
        module
            .parse_statement("SELECT 1")
            .unwrap()
            .canonical_sql()
            .to_string()
    })
    .join()
    .unwrap();

    assert!(canonical.to_uppercase().contains("SELECT"));
}

/// Independent instances parse concurrently on separate threads, each isolated
/// in its own wasm linear memory.
#[test]
fn independent_modules_run_in_parallel() {
    let threads: Vec<_> = (0..4)
        .map(|i| {
            std::thread::spawn(move || {
                let mut module = Module::new().unwrap();
                let canonical = module
                    .parse_statement(&format!("SELECT {i}"))
                    .unwrap()
                    .canonical_sql()
                    .to_string();
                assert!(canonical.contains(&i.to_string()));
            })
        })
        .collect();

    for thread in threads {
        thread.join().unwrap();
    }
}
