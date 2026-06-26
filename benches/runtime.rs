//! Performance baseline for the runtime core (spec §13).
//!
//! Guards the perf-sensitive paths that exist today: VM cold start, the
//! load+exec boundary for a trivial script, and the interrupt/sandbox-hook
//! overhead on a compute-bound loop. New perf-sensitive features should add a
//! benchmark here as they land, so regressions are caught continuously.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lur::runtime::Runtime;

/// Cost of building a fresh sandboxed VM (sandbox + capability injection +
/// interrupt + memory cap).
fn vm_cold_start(c: &mut Criterion) {
    c.bench_function("vm_cold_start", |b| {
        b.iter(|| black_box(Runtime::new().expect("runtime builds")));
    });
}

/// Load + execute boundary for a trivial chunk on a warm VM.
fn trivial_script(c: &mut Criterion) {
    let rt = Runtime::new().expect("runtime builds");
    c.bench_function("trivial_script", |b| {
        b.iter(|| rt.run(black_box("local x = 1 + 1")).unwrap());
    });
}

/// A bounded numeric loop: the interrupt hook fires on back-edges, so this
/// captures sandbox-hook overhead over raw computation.
fn compute_loop_hook_overhead(c: &mut Criterion) {
    let rt = Runtime::new().expect("runtime builds");
    let src = "local s = 0 for i = 1, 10000 do s = s + i end return s";
    c.bench_function("compute_loop_hook_overhead", |b| {
        b.iter(|| rt.run(black_box(src)).unwrap());
    });
}

criterion_group!(
    benches,
    vm_cold_start,
    trivial_script,
    compute_loop_hook_overhead
);
criterion_main!(benches);
