//! Throwaway side benchmark (not for merge): parsing OpenCC-style TSV text into
//! `(old, new)` pairs — Luau vs a Rust host function. No replacement, no Trie.
//!
//! Usage: `TSV_DIR=<dir of OpenCC *.txt> cargo bench --bench tsv_parse`
//!
//! Parsing rule (the one in pi-playbook's lur-app.lua): per line
//! `old<TAB>new[ other…]`; `old` = up to the first TAB (non-empty), `new` = the run of
//! non-space/non-TAB bytes right after it (non-empty); other lines are skipped.

use std::fs;
use std::hint::black_box;
use std::path::PathBuf;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use mlua::{Function, Lua, Table};

// ---------------------------------------------------------------- dataset

fn tsv_dir() -> PathBuf {
    PathBuf::from(std::env::var("TSV_DIR").expect("set TSV_DIR to the OpenCC dictionary dir"))
}

fn read_files(names: &[&str]) -> Vec<u8> {
    let dir = tsv_dir();
    let mut out = Vec::new();
    for n in names {
        let mut b = fs::read(dir.join(format!("{n}.txt"))).expect("dictionary file");
        out.append(&mut b);
        if out.last() != Some(&b'\n') {
            out.push(b'\n');
        }
    }
    out
}

/// The stage files an `s2twp`-like config reads at this OpenCC commit.
fn s2twp_set() -> Vec<u8> {
    read_files(&["STCharacters", "STPhrases", "TWVariants", "TWPhrases"])
}

fn all_set() -> Vec<u8> {
    let mut names: Vec<String> = fs::read_dir(tsv_dir())
        .unwrap()
        .filter_map(|e| {
            let n = e.ok()?.file_name().into_string().ok()?;
            n.strip_suffix(".txt").map(str::to_owned)
        })
        .collect();
    names.sort();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    read_files(&refs)
}

/// Cycle the real lines until the text has `n` lines.
fn scaled(base: &[u8], n: usize) -> Vec<u8> {
    let lines: Vec<&[u8]> = base
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    let mut out = Vec::with_capacity(base.len() * n / lines.len() + 64);
    for i in 0..n {
        out.extend_from_slice(lines[i % lines.len()]);
        out.push(b'\n');
    }
    out
}

fn count_lines(t: &[u8]) -> usize {
    t.iter().filter(|&&b| b == b'\n').count()
}

// ---------------------------------------------------------------- Rust parser

fn parse_ref(text: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut out = Vec::new();
    for line in text.split(|&b| b == b'\n' || b == b'\r') {
        if line.is_empty() {
            continue;
        }
        let Some(tab) = line.iter().position(|&b| b == b'\t') else {
            continue;
        };
        let (old, rest) = (&line[..tab], &line[tab + 1..]);
        if old.is_empty() {
            continue;
        }
        let end = rest
            .iter()
            .position(|&b| b == b' ' || b == b'\t')
            .unwrap_or(rest.len());
        let new = &rest[..end];
        if new.is_empty() {
            continue;
        }
        out.push((old, new));
    }
    out
}

/// R1: host function returning `{ {old, new}, … }`.
fn host_parse_pairs(lua: &Lua, text: mlua::String) -> mlua::Result<Table> {
    let bytes = text.as_bytes();
    let pairs = parse_ref(&bytes);
    let out = lua.create_table_with_capacity(pairs.len(), 0)?;
    for (i, (old, new)) in pairs.into_iter().enumerate() {
        let p = lua.create_table_with_capacity(2, 0)?;
        p.raw_set(1, lua.create_string(old)?)?;
        p.raw_set(2, lua.create_string(new)?)?;
        out.raw_set(i + 1, p)?;
    }
    Ok(out)
}

/// R2: host function returning two parallel arrays.
fn host_parse_arrays(lua: &Lua, text: mlua::String) -> mlua::Result<(Table, Table)> {
    let bytes = text.as_bytes();
    let pairs = parse_ref(&bytes);
    let olds = lua.create_table_with_capacity(pairs.len(), 0)?;
    let news = lua.create_table_with_capacity(pairs.len(), 0)?;
    for (i, (old, new)) in pairs.into_iter().enumerate() {
        olds.raw_set(i + 1, lua.create_string(old)?)?;
        news.raw_set(i + 1, lua.create_string(new)?)?;
    }
    Ok((olds, news))
}

// ---------------------------------------------------------------- variants

struct Variant {
    name: &'static str,
    /// Lua expression evaluating to `parse(text)`.
    parse: &'static str,
    /// Result is two parallel arrays instead of an array of pairs.
    arrays: bool,
}

const VARIANTS: &[Variant] = &[
    Variant {
        name: "L1_lua_gmatch_lines_match",
        parse: r#"function(text)
            local out = {}
            for line in string.gmatch(text, "[^\r\n]+") do
                local old, new = string.match(line, "^([^\t]+)\t([^ \t]+)")
                if old then out[#out + 1] = { old, new } end
            end
            return out
        end"#,
        arrays: false,
    },
    Variant {
        name: "L2_lua_gmatch_whole_pairs",
        parse: r#"function(text)
            local out, n = {}, 0
            for old, new in string.gmatch(text, "([^\t\r\n]+)\t([^ \t\r\n]+)") do
                n += 1
                out[n] = { old, new }
            end
            return out
        end"#,
        arrays: false,
    },
    Variant {
        name: "L3_lua_gmatch_whole_arrays",
        parse: r#"function(text)
            local olds, news, n = {}, {}, 0
            for old, new in string.gmatch(text, "([^\t\r\n]+)\t([^ \t\r\n]+)") do
                n += 1
                olds[n] = old
                news[n] = new
            end
            return olds, news
        end"#,
        arrays: true,
    },
    Variant {
        name: "R1_rust_host_pairs",
        parse: "parse_pairs",
        arrays: false,
    },
    Variant {
        name: "R2_rust_host_arrays",
        parse: "parse_arrays",
        arrays: true,
    },
];

fn new_state() -> Lua {
    let lua = Lua::new();
    let g = lua.globals();
    g.set(
        "parse_pairs",
        lua.create_function(|lua, t: mlua::String| host_parse_pairs(lua, t))
            .unwrap(),
    )
    .unwrap();
    g.set(
        "parse_arrays",
        lua.create_function(|lua, t: mlua::String| host_parse_arrays(lua, t))
            .unwrap(),
    )
    .unwrap();
    lua
}

/// `function(text) return #(parse(text)) end` — the bench entry point.
fn count_fn(lua: &Lua, v: &Variant) -> Function {
    let src = format!(
        "local parse = {}\nreturn function(text) return #(parse(text)) end",
        v.parse
    );
    lua.load(&src).eval().unwrap()
}

fn parse_fn(lua: &Lua, v: &Variant) -> Function {
    lua.load(&format!("return {}", v.parse)).eval().unwrap()
}

// ---------------------------------------------------------------- correctness

fn bytes_of(t: &Table, i: usize) -> Vec<u8> {
    let s: mlua::String = t.get(i).unwrap();
    s.as_bytes().to_vec()
}

fn check_correctness(text: &[u8]) {
    let expect: Vec<(Vec<u8>, Vec<u8>)> = parse_ref(text)
        .into_iter()
        .map(|(a, b)| (a.to_vec(), b.to_vec()))
        .collect();
    for v in VARIANTS {
        let lua = new_state();
        let f = parse_fn(&lua, v);
        let s = lua.create_string(text).unwrap();
        let mut got: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        if v.arrays {
            let (o, n): (Table, Table) = f.call(s).unwrap();
            for i in 1..=o.raw_len() {
                got.push((bytes_of(&o, i), bytes_of(&n, i)));
            }
        } else {
            let t: Table = f.call(s).unwrap();
            for i in 1..=t.raw_len() {
                let p: Table = t.get(i).unwrap();
                got.push((bytes_of(&p, 1), bytes_of(&p, 2)));
            }
        }
        let ok = got == expect;
        println!(
            "correctness {:<28} pairs={:<8} expected={:<8} {}",
            v.name,
            got.len(),
            expect.len(),
            if ok { "OK" } else { "MISMATCH" }
        );
    }
}

// ---------------------------------------------------------------- memory report

const MEM_CHUNK: &str = r#"
return function(parse, text)
    collectgarbage(); collectgarbage()
    local base = collectgarbage("count")
    collectgarbage("stop")
    local r1, r2 = parse(text)
    local peak = collectgarbage("count")
    collectgarbage("restart")
    collectgarbage(); collectgarbage()
    local live = collectgarbage("count")
    r1, r2 = nil, nil
    collectgarbage(); collectgarbage()
    local fin = collectgarbage("count")
    return peak - base, live - base, fin - base
end"#;

fn memory_report(label: &str, text: &[u8]) {
    println!(
        "memory [{label}] lines={} bytes={}",
        count_lines(text),
        text.len()
    );
    for v in VARIANTS {
        let lua = new_state();
        let measure: Function = lua.load(MEM_CHUNK).eval().unwrap();
        let f = parse_fn(&lua, v);
        let s = lua.create_string(text).unwrap();
        let (peak, live, fin): (f64, f64, f64) = measure.call((f, s)).unwrap();
        println!(
            "  {:<28} no-GC allocated={:>9.1} KiB   live after GC={:>9.1} KiB   after drop={:>7.1} KiB",
            v.name, peak, live, fin
        );
    }
}

// ---------------------------------------------------------------- timing

fn bench_fresh_vm(c: &mut Criterion, label: &str, text: &[u8], heavy: bool) {
    let mut g = c.benchmark_group(format!("tsv_parse/fresh_vm/{label}"));
    g.throughput(Throughput::Bytes(text.len() as u64));
    if heavy {
        g.sample_size(10);
        g.warm_up_time(std::time::Duration::from_secs(1));
    }
    g.bench_with_input(BenchmarkId::new("R0_rust_only", label), text, |b, text| {
        b.iter(|| black_box(parse_ref(black_box(text)).len()));
    });
    for v in VARIANTS {
        g.bench_with_input(BenchmarkId::new(v.name, label), text, |b, text| {
            b.iter_batched(
                || {
                    let lua = new_state();
                    let f = count_fn(&lua, v);
                    let s = lua.create_string(text).unwrap();
                    (lua, f, s)
                },
                |(lua, f, s)| {
                    let n: usize = f.call(s).unwrap();
                    black_box(n);
                    lua // dropped (heap freed) outside the timed section
                },
                BatchSize::PerIteration,
            );
        });
    }
    g.finish();
}

fn bench_warm_vm(c: &mut Criterion, label: &str, text: &[u8]) {
    let mut g = c.benchmark_group(format!("tsv_parse/warm_vm/{label}"));
    g.throughput(Throughput::Bytes(text.len() as u64));
    for v in VARIANTS {
        let lua = new_state();
        let f = count_fn(&lua, v);
        let s = lua.create_string(text).unwrap();
        g.bench_function(v.name, |b| {
            b.iter(|| {
                let n: usize = f.call(s.clone()).unwrap();
                black_box(n)
            });
        });
    }
    g.finish();
}

/// `k` VMs each parsing once, sequentially (every pooled VM running app.lua's top level).
fn bench_pool(c: &mut Criterion, label: &str, text: &[u8]) {
    let mut g = c.benchmark_group(format!("tsv_parse/pool/{label}"));
    g.sample_size(10);
    for &k in &[1usize, 4, 8] {
        for v in VARIANTS {
            g.bench_with_input(BenchmarkId::new(v.name, k), &k, |b, &k| {
                b.iter_batched(
                    || {
                        (0..k)
                            .map(|_| {
                                let lua = new_state();
                                let f = count_fn(&lua, v);
                                let s = lua.create_string(text).unwrap();
                                (lua, f, s)
                            })
                            .collect::<Vec<_>>()
                    },
                    |vms| {
                        for (_, f, s) in &vms {
                            let n: usize = f.call(s.clone()).unwrap();
                            black_box(n);
                        }
                        vms
                    },
                    BatchSize::PerIteration,
                );
            });
        }
    }
    g.finish();
}

fn main() {
    let real = s2twp_set();
    let all = all_set();

    println!(
        "dataset s2twp-set: lines={} bytes={}",
        count_lines(&real),
        real.len()
    );
    println!(
        "dataset all-files: lines={} bytes={}",
        count_lines(&all),
        all.len()
    );
    check_correctness(&real);
    check_correctness(&all);

    memory_report("s2twp-set", &real);
    memory_report("all-files", &all);
    for n in [10_000usize, 100_000, 1_000_000] {
        memory_report(&format!("synthetic-{n}"), &scaled(&all, n));
    }

    let mut c = Criterion::default().configure_from_args();
    bench_fresh_vm(&mut c, "s2twp-set", &real, false);
    bench_warm_vm(&mut c, "s2twp-set", &real);
    bench_pool(&mut c, "s2twp-set", &real);
    for (n, heavy) in [
        (1_000usize, false),
        (10_000, false),
        (100_000, true),
        (1_000_000, true),
    ] {
        bench_fresh_vm(&mut c, &format!("synthetic-{n}"), &scaled(&all, n), heavy);
    }
    c.final_summary();
}
