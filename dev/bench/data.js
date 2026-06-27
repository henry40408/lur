window.BENCHMARK_DATA = {
  "lastUpdate": 1782544381146,
  "repoUrl": "https://github.com/henry40408/lur",
  "entries": {
    "lur criterion": [
      {
        "commit": {
          "author": {
            "email": "2316687+henry40408@users.noreply.github.com",
            "name": "Heng-Yi Wu",
            "username": "henry40408"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "7ade5d669d60a283301558235f0c206fefffb917",
          "message": "ci: GitHub Actions — fmt/clippy/nextest gates + criterion perf gate (#25)\n\nThe repo had no CI. Add a workflow (push to main + every PR):\n\n- lint: cargo fmt --check and clippy -D warnings;\n- test: cargo nextest run;\n- perf: benchmark-action/github-action-benchmark gates regressions.\n  cargo bench emits the libtest \"bencher\" format (criterion\n  --output-format bencher); a push to main records the moving baseline\n  on gh-pages, and each PR compares to it, failing (fail-on-alert) when a\n  benchmark exceeds the alert-threshold. The threshold is a deliberately\n  loose 1.5x: wall-clock on shared runners is noisy, so this catches\n  clear regressions without flaking; tighten once the history shows the\n  noise floor.\n\nEvery `uses:` is pinned to a full commit SHA (not a movable tag) to\nresist supply-chain attacks, each annotated with its version; all are\nwell past the 7-day cooldown.\n\nCo-authored-by: Claude Opus 4.8 <noreply@anthropic.com>",
          "timestamp": "2026-06-27T14:57:48+08:00",
          "tree_id": "1dd11cf1f2ab7efbd4c4a3ddcb15a2617f1fa555",
          "url": "https://github.com/henry40408/lur/commit/7ade5d669d60a283301558235f0c206fefffb917"
        },
        "date": 1782543668723,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 226826,
            "range": "± 5614",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5244,
            "range": "± 23",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 210353,
            "range": "± 3307",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "2316687+henry40408@users.noreply.github.com",
            "name": "Heng-Yi Wu",
            "username": "henry40408"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "afdc7fdb29058be4f5bfd5d9fa2bbb9aec6d486e",
          "message": "ci: make the perf benchmark informational, not a hard gate (#27)\n\nThe github-action-benchmark gate compares against a baseline recorded on\na different shared runner, and wall-clock variance there is large: a\nno-op change (the dependabot PR, identical runtime code) measured 1.85x\nvs the baseline and tripped the 1.5x threshold — a false positive.\n\nSet fail-on-alert: false so the benchmark comparison is still posted as a\nPR comment and job summary (useful signal) but never blocks a merge.\nlint (fmt + clippy) and test (nextest) remain hard gates. A deterministic\nhard perf gate would need instruction-level measurement (e.g. CodSpeed),\nwhich is a larger change left for later.\n\nCo-authored-by: Claude Opus 4.8 <noreply@anthropic.com>",
          "timestamp": "2026-06-27T15:12:01+08:00",
          "tree_id": "840224206e68c0c03600f105e8ff0c17a3957028",
          "url": "https://github.com/henry40408/lur/commit/afdc7fdb29058be4f5bfd5d9fa2bbb9aec6d486e"
        },
        "date": 1782544380872,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 256878,
            "range": "± 2758",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5333,
            "range": "± 26",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 207122,
            "range": "± 5763",
            "unit": "ns/iter"
          }
        ]
      }
    ]
  }
}