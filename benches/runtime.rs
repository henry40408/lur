//! Runtime core performance baseline (spec §13). Add a benchmark here for each
//! new perf-sensitive path.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lur::runtime::Runtime;

/// Building a fresh sandboxed VM.
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

/// Interrupt-hook overhead on a compute loop (the hook fires on back-edges).
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
