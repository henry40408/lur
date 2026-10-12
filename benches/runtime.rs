//! Runtime core performance baseline. Add a benchmark here for each
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

/// `lur.async.map` over a long list under a per-call limit: the window keeps
/// this linear in list length (it was quadratic with all futures created up
/// front).
fn async_map_limit(c: &mut Criterion) {
    let rt = Runtime::new().expect("runtime builds");
    let src = "local t = {} for i = 1, 20000 do t[i] = i end \
               return #lur.async.map(t, function(x) return x end, { limit = 64 })";
    c.bench_function("async_map_20k_limit_64", |b| {
        b.iter(|| rt.run(black_box(src)).unwrap());
    });
}

/// Many small `lur.async.all` calls: per-call combinator overhead (the
/// wrapper that re-raises error tables) rather than per-task cost.
fn async_all_small(c: &mut Criterion) {
    let rt = Runtime::new().expect("runtime builds");
    let src = "local f = function() return 1 end \
               local n = 0 for _ = 1, 1000 do n = n + #lur.async.all({ f, f }) end return n";
    c.bench_function("async_all_small_x1000", |b| {
        b.iter(|| rt.run(black_box(src)).unwrap());
    });
}

/// Many small `lur.async.race` calls (same wrapper, early-settling path).
fn async_race_small(c: &mut Criterion) {
    let rt = Runtime::new().expect("runtime builds");
    let src = "local f = function() return 1 end \
               local n = 0 for _ = 1, 1000 do n = n + lur.async.race({ f, f }) end return n";
    c.bench_function("async_race_small_x1000", |b| {
        b.iter(|| rt.run(black_box(src)).unwrap());
    });
}

criterion_group!(
    benches,
    vm_cold_start,
    trivial_script,
    compute_loop_hook_overhead,
    async_map_limit,
    async_all_small,
    async_race_small
);
criterion_main!(benches);
