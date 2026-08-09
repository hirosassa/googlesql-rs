//! Criterion benchmarks for the public query pipeline.
//!
//! These measure end-to-end latency of each high-level API against the shared
//! wasm module. The dominant cost is the number of wasm round-trips a call
//! makes (each is an alloc → write → invoke → read → free crossing), so the
//! `analyzer` group — especially `resolved_tree`, which walks every resolved
//! node — is the one to watch when the ABI or the traversal changes.
//!
//! Every group runs against each available backend, labelled `wasmtime` and
//! (under `--features native`) `native`, so criterion reports them side by side:
//! `cargo bench --features native` compares the wasmtime engine against the
//! wasm2rs-transpiled native engine on identical work. Without the feature only
//! the `wasmtime` arm builds.
//!
//! Run with `cargo bench`; these are excluded from `cargo test` (the
//! `[[bench]]` target uses `harness = false`) and from CI's test run.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "bench code"
)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use googlesql::{ColumnDef, ColumnType, Module, TableDef};

/// A benchmarked engine: a display label paired with a `Module` constructor.
type Backend = (&'static str, fn() -> Module);

/// The engines to benchmark: always wasmtime; the native (wasm2rs) engine too
/// when the `native` feature is enabled. Each entry pairs a label with a
/// constructor, so every group runs the same work through both backends.
fn backends() -> Vec<Backend> {
    #[allow(
        unused_mut,
        reason = "mutated only under the `native` feature; immutable otherwise"
    )]
    let mut engines: Vec<Backend> = vec![("wasmtime", || Module::new().unwrap())];
    #[cfg(feature = "native")]
    engines.push(("native", || Module::new_native().unwrap()));
    engines
}

/// A two-table catalog the analyzer benchmarks resolve against.
fn catalog() -> Vec<TableDef> {
    vec![
        TableDef {
            name: "users".to_string(),
            columns: vec![
                ColumnDef {
                    name: "id".to_string(),
                    ty: ColumnType::Int64,
                },
                ColumnDef {
                    name: "name".to_string(),
                    ty: ColumnType::String,
                },
                ColumnDef {
                    name: "org_id".to_string(),
                    ty: ColumnType::Int64,
                },
            ],
        },
        TableDef {
            name: "orgs".to_string(),
            columns: vec![
                ColumnDef {
                    name: "id".to_string(),
                    ty: ColumnType::Int64,
                },
                ColumnDef {
                    name: "name".to_string(),
                    ty: ColumnType::String,
                },
            ],
        },
    ]
}

/// A non-trivial query (join + filter + order + limit) that produces a deep
/// resolved tree, so the analyzer benchmarks exercise many wasm round-trips.
const JOIN_SQL: &str = "SELECT u.id, u.name, o.name AS org \
     FROM users AS u JOIN orgs AS o ON u.org_id = o.id \
     WHERE u.id > 10 ORDER BY u.name LIMIT 100";

/// Baseline: instantiating a `Module`. The compiled wasm is shared across
/// instances, but each call still spins up a fresh instance and WASI context.
fn bench_module_new(c: &mut Criterion) {
    let mut group = c.benchmark_group("module_new");
    for (name, make) in backends() {
        group.bench_function(name, |b| {
            b.iter(|| black_box(make()));
        });
    }
    group.finish();
}

/// Parser and formatter: single-statement round-trips with no catalog.
fn bench_syntax(c: &mut Criterion) {
    let mut group = c.benchmark_group("syntax");
    for (name, make) in backends() {
        let mut module = make();
        group.bench_function(BenchmarkId::new("parse_simple", name), |b| {
            b.iter(|| module.parse_statement(black_box("SELECT 1")).unwrap());
        });
        group.bench_function(BenchmarkId::new("parse_join", name), |b| {
            b.iter(|| module.parse_statement(black_box(JOIN_SQL)).unwrap());
        });
        group.bench_function(BenchmarkId::new("format", name), |b| {
            b.iter(|| module.format_sql(black_box(JOIN_SQL)).unwrap());
        });
    }
    group.finish();
}

/// Analyzer: the round-trip-heavy paths, resolved against `catalog`.
/// `resolved_tree` walks every node and is the deepest of these.
fn bench_analyzer(c: &mut Criterion) {
    let cat = catalog();
    let mut group = c.benchmark_group("analyzer");
    for (name, make) in backends() {
        let mut module = make();
        group.bench_function(BenchmarkId::new("analyze_trivial", name), |b| {
            b.iter(|| {
                module
                    .analyze_output_columns(black_box("SELECT 1"), &[])
                    .unwrap()
            });
        });
        group.bench_function(BenchmarkId::new("output_columns", name), |b| {
            b.iter(|| {
                module
                    .analyze_output_columns(black_box(JOIN_SQL), &cat)
                    .unwrap()
            });
        });
        group.bench_function(BenchmarkId::new("referenced_tables", name), |b| {
            b.iter(|| module.referenced_tables(black_box(JOIN_SQL), &cat).unwrap());
        });
        group.bench_function(BenchmarkId::new("resolved_tree", name), |b| {
            b.iter(|| module.resolved_tree(black_box(JOIN_SQL), &cat).unwrap());
        });
    }
    group.finish();
}

/// Parallel scaling: does throughput grow with thread count when each thread
/// drives its own `Module`?
///
/// This is the question a `ModulePool` hinges on. Each thread reuses one
/// pre-created instance (creation is outside the timed loop, mirroring a pool
/// that pays `Module::new` up front) and parses a fixed batch. `thread::scope`
/// hands each thread a disjoint `&mut Module` from the vec, which is sound only
/// because `Module: Send`. Throughput is set to the total parses per iteration,
/// so criterion reports elements/sec — compare `threads_1` against `threads_N`
/// to read the speedup. Near-linear scaling means a pool pays off; a flat line
/// means calls serialize somewhere and a pool would not help.
fn bench_parallel_scaling(c: &mut Criterion) {
    /// Parses each thread runs per iteration; large enough to dwarf the
    /// per-iteration thread spawn/join overhead.
    const BATCH_PER_THREAD: usize = 200;

    // A light and a heavy statement: if only the light one fails to scale, the
    // bottleneck is per-call fixed overhead (e.g. host allocator contention); if
    // both flatten, calls serialize regardless of workload.
    for (label, sql) in [("simple", "SELECT 1"), ("join", JOIN_SQL)] {
        let mut group = c.benchmark_group(format!("parallel_{label}"));
        for (name, make) in backends() {
            for threads in [1usize, 2, 4, 8] {
                // One instance per thread, created once so the timed loop measures
                // only steady-state parsing (as a warm pool would).
                let mut modules: Vec<Module> = (0..threads).map(|_| make()).collect();
                let elements =
                    u64::try_from(threads.checked_mul(BATCH_PER_THREAD).unwrap()).unwrap();
                group.throughput(Throughput::Elements(elements));
                group.bench_function(BenchmarkId::new(format!("threads_{threads}"), name), |b| {
                    b.iter(|| {
                        std::thread::scope(|scope| {
                            for module in &mut modules {
                                scope.spawn(move || {
                                    for _ in 0..BATCH_PER_THREAD {
                                        module.parse_statement(black_box(sql)).unwrap();
                                    }
                                });
                            }
                        });
                    });
                });
            }
        }
        group.finish();
    }
}

criterion_group!(
    benches,
    bench_module_new,
    bench_syntax,
    bench_analyzer,
    bench_parallel_scaling
);
criterion_main!(benches);
