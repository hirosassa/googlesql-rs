//! Criterion benchmarks for the public query pipeline.
//!
//! These measure end-to-end latency of each high-level API against the shared
//! wasm module. The dominant cost is the number of wasm round-trips a call
//! makes (each is an alloc → write → invoke → read → free crossing), so the
//! `analyzer` group — especially `resolved_tree`, which walks every resolved
//! node — is the one to watch when the ABI or the traversal changes.
//!
//! Run with `cargo bench`; these are excluded from `cargo test` (the
//! `[[bench]]` target uses `harness = false`) and from CI's test run.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "bench code"
)]

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use googlesql::{ColumnDef, ColumnType, Module, TableDef};

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
    c.bench_function("module_new", |b| {
        b.iter(|| Module::new().unwrap());
    });
}

/// Parser and formatter: single-statement round-trips with no catalog.
fn bench_syntax(c: &mut Criterion) {
    let mut module = Module::new().unwrap();
    let mut group = c.benchmark_group("syntax");
    group.bench_function("parse_simple", |b| {
        b.iter(|| module.parse_statement(black_box("SELECT 1")).unwrap());
    });
    group.bench_function("parse_join", |b| {
        b.iter(|| module.parse_statement(black_box(JOIN_SQL)).unwrap());
    });
    group.bench_function("format", |b| {
        b.iter(|| module.format_sql(black_box(JOIN_SQL)).unwrap());
    });
    group.finish();
}

/// Analyzer: the round-trip-heavy paths, resolved against `catalog`.
/// `resolved_tree` walks every node and is the deepest of these.
fn bench_analyzer(c: &mut Criterion) {
    let mut module = Module::new().unwrap();
    let cat = catalog();
    let mut group = c.benchmark_group("analyzer");
    group.bench_function("analyze_trivial", |b| {
        b.iter(|| {
            module
                .analyze_output_columns(black_box("SELECT 1"), &[])
                .unwrap()
        });
    });
    group.bench_function("output_columns", |b| {
        b.iter(|| {
            module
                .analyze_output_columns(black_box(JOIN_SQL), &cat)
                .unwrap()
        });
    });
    group.bench_function("referenced_tables", |b| {
        b.iter(|| module.referenced_tables(black_box(JOIN_SQL), &cat).unwrap());
    });
    group.bench_function("resolved_tree", |b| {
        b.iter(|| module.resolved_tree(black_box(JOIN_SQL), &cat).unwrap());
    });
    group.finish();
}

criterion_group!(benches, bench_module_new, bench_syntax, bench_analyzer);
criterion_main!(benches);
