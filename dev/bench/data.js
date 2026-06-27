window.BENCHMARK_DATA = {
  "lastUpdate": 1782557308784,
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
          "id": "15487f806e1936693d1708528c0dd541f9498951",
          "message": "ci: Dependabot for cargo + github-actions with a 7-day cooldown (#26)\n\nWeekly update checks for Cargo crates and the SHA-pinned GitHub Actions.\nA `cooldown.default-days: 7` mirrors the project's supply-chain policy\n(CLAUDE.md): a new release is not proposed until it has been published\nfor at least 7 days, so a malicious or broken release has time to be\ncaught and yanked first. Updates are grouped per ecosystem to avoid PR\nspam.\n\nCo-authored-by: Claude Opus 4.8 <noreply@anthropic.com>",
          "timestamp": "2026-06-27T15:14:04+08:00",
          "tree_id": "1e4642e19fb79be81a14dc06e4e9776a9b305ede",
          "url": "https://github.com/henry40408/lur/commit/15487f806e1936693d1708528c0dd541f9498951"
        },
        "date": 1782544514453,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 262655,
            "range": "± 2800",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5330,
            "range": "± 155",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 207674,
            "range": "± 4825",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "49699333+dependabot[bot]@users.noreply.github.com",
            "name": "dependabot[bot]",
            "username": "dependabot[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "aa91bac2d2912951f21351464fd3df2498683cd0",
          "message": "chore(deps): bump the actions group with 2 updates (#29)\n\nBumps the actions group with 2 updates: [actions/checkout](https://github.com/actions/checkout) and [taiki-e/install-action](https://github.com/taiki-e/install-action).\n\n\nUpdates `actions/checkout` from 6.0.3 to 7.0.0\n- [Release notes](https://github.com/actions/checkout/releases)\n- [Changelog](https://github.com/actions/checkout/blob/main/CHANGELOG.md)\n- [Commits](https://github.com/actions/checkout/compare/df4cb1c069e1874edd31b4311f1884172cec0e10...9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0)\n\nUpdates `taiki-e/install-action` from 2.62.0 to 2.82.1\n- [Release notes](https://github.com/taiki-e/install-action/releases)\n- [Changelog](https://github.com/taiki-e/install-action/blob/main/CHANGELOG.md)\n- [Commits](https://github.com/taiki-e/install-action/compare/0e09747a63ae497bf945b3dcaf38fef0050d0109...8b3c737da4b541bf0fb5a3e0488ff20535badac9)\n\n---\nupdated-dependencies:\n- dependency-name: actions/checkout\n  dependency-version: 7.0.0\n  dependency-type: direct:production\n  update-type: version-update:semver-major\n  dependency-group: actions\n- dependency-name: taiki-e/install-action\n  dependency-version: 2.82.1\n  dependency-type: direct:production\n  update-type: version-update:semver-minor\n  dependency-group: actions\n...\n\nSigned-off-by: dependabot[bot] <support@github.com>\nCo-authored-by: dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>",
          "timestamp": "2026-06-27T15:18:37+08:00",
          "tree_id": "f7d956a370b590a09a5dc618239a2e4aa67c80cb",
          "url": "https://github.com/henry40408/lur/commit/aa91bac2d2912951f21351464fd3df2498683cd0"
        },
        "date": 1782544788741,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 223180,
            "range": "± 6514",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5173,
            "range": "± 88",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 391747,
            "range": "± 5596",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "49699333+dependabot[bot]@users.noreply.github.com",
            "name": "dependabot[bot]",
            "username": "dependabot[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "fc8d6d41ecbe38c09a5cb07cd0b57e850ddf2018",
          "message": "chore(deps): bump sqlx from 0.8.6 to 0.9.0 in the cargo group (#28)\n\n* chore(deps): bump sqlx from 0.8.6 to 0.9.0 in the cargo group\n\nBumps the cargo group with 1 update: [sqlx](https://github.com/launchbadge/sqlx).\n\n\nUpdates `sqlx` from 0.8.6 to 0.9.0\n- [Changelog](https://github.com/transact-rs/sqlx/blob/main/CHANGELOG.md)\n- [Commits](https://github.com/launchbadge/sqlx/compare/v0.8.6...v0.9.0)\n\n---\nupdated-dependencies:\n- dependency-name: sqlx\n  dependency-version: 0.9.0\n  dependency-type: direct:production\n  update-type: version-update:semver-minor\n  dependency-group: cargo\n...\n\nSigned-off-by: dependabot[bot] <support@github.com>\n\n* fix(db): migrate lur.db to the sqlx 0.9 API\n\nsqlx 0.9 has two breaking changes that touch src/capabilities/db.rs:\n\n- SqliteArguments no longer carries a lifetime parameter — drop it from\n  the Query type alias.\n- query()/query_as() now require a SqlSafeStr to discourage SQL\n  injection; a runtime-built String no longer coerces. lur.db runs\n  script-authored SQL by design (user input is bound separately via `?`\n  placeholders, never concatenated), so wrap the dynamic statements in\n  sqlx::AssertSqlSafe — the explicit, audited opt-in. The static `lur_kv`\n  literals already satisfy the bound unchanged.\n\n141 tests pass (incl. lur.db / lur.kv / lur.db.tx); clippy -D warnings\nand fmt clean.\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n---------\n\nSigned-off-by: dependabot[bot] <support@github.com>\nCo-authored-by: dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>\nCo-authored-by: Heng-Yi Wu <2316687+henry40408@users.noreply.github.com>\nCo-authored-by: Claude Opus 4.8 <noreply@anthropic.com>",
          "timestamp": "2026-06-27T15:30:28+08:00",
          "tree_id": "d28a8c1fa950b1438f886433243b5d052a248ed1",
          "url": "https://github.com/henry40408/lur/commit/fc8d6d41ecbe38c09a5cb07cd0b57e850ddf2018"
        },
        "date": 1782545517219,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 227118,
            "range": "± 4879",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5147,
            "range": "± 30",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 206162,
            "range": "± 2650",
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
          "id": "646ba7adbd39e9f9488646c1c3dbeb31276d7de2",
          "message": "ci: code coverage via cargo-llvm-cov, uploaded to Codecov (#30)\n\nAdd a coverage job: cargo-llvm-cov runs the suite through nextest with\nLLVM source-based instrumentation and emits lcov.info, which is uploaded\nto Codecov. Verified locally — 141 tests pass under instrumentation\n(~83% line / ~87% region coverage).\n\nCoverage is a signal, not a hard gate: codecov.yml marks the project and\npatch statuses informational, and the upload uses fail_ci_if_error:false,\nso neither a drop nor an upload hiccup blocks a merge (consistent with\nthe perf benchmark). The existing fmt/clippy/nextest jobs stay hard\ngates.\n\nNote: set a CODECOV_TOKEN repo secret (from codecov.io) so uploads are\nreliable; the public-repo tokenless path is a fallback.\n\nCo-authored-by: Claude Opus 4.8 <noreply@anthropic.com>",
          "timestamp": "2026-06-27T15:41:44+08:00",
          "tree_id": "7d4e7e2b4320b87b8dd2506ff9be68b8ef9ea81e",
          "url": "https://github.com/henry40408/lur/commit/646ba7adbd39e9f9488646c1c3dbeb31276d7de2"
        },
        "date": 1782546191261,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 256795,
            "range": "± 4910",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5063,
            "range": "± 45",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 206889,
            "range": "± 6574",
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
          "id": "e39c017a725b3d63f6fca700870eba46ff172121",
          "message": "docs: add README, ARCHITECTURE, and MIT license (#31)\n\n* docs: add README with CLI reference and Lua API\n\nDocument the two run modes, the capability sandbox (strict/loose profiles,\nremoved Luau globals), the full CLI flag set with SIZE/DURATION grammar and\nconfig-file resolution, and the complete lur.* Lua API surface. Examples are\nverified against the built binary.\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n* docs: add MIT license\n\nAdd LICENSE.txt (MIT), declare the license in Cargo.toml, and point the\nREADME License section at it.\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n* docs: add ARCHITECTURE.md for developers\n\nAdd a developer-facing architecture guide covering the shared core (build_lua,\nsandbox ordering, the two-layer timeout), the capability/policy layer, the\none-shot and server execution paths (VM pool checkout, router with :param\nprecedence, request lifecycle, per-call isolation, cron, graceful shutdown),\nstate/storage, the async core, and config resolution. Link it from the README.\n\nAlso correct two README inaccuracies found while reading the code: routing\nsupports :param segments with static-beats-dynamic precedence (not exact-match),\nand handler responses honor status/body only (response headers are not yet\nwired). Both examples verified against the built binary.\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Opus 4.8 <noreply@anthropic.com>",
          "timestamp": "2026-06-27T16:31:30+08:00",
          "tree_id": "a09ba65415d586e7b322682bea4ca1d4b52035b9",
          "url": "https://github.com/henry40408/lur/commit/e39c017a725b3d63f6fca700870eba46ff172121"
        },
        "date": 1782549158943,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 252805,
            "range": "± 3952",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5222,
            "range": "± 164",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 206202,
            "range": "± 5756",
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
          "id": "12bb0ab6be3f43b203fbb1f394c162053c16ff1f",
          "message": "chore: add cargo-deny supply-chain checks (#32)\n\nAdd deny.toml and a hard-gate `cargo-deny` CI job that checks advisories\n(RUSTSEC), bans, licenses, and sources on every push and PR. The license\nallowlist enumerates the permissive licenses present in the current graph;\nsources are restricted to crates.io; duplicate versions surface as warnings.\n\nAlso declare `license = \"MIT\"` in Cargo.toml so the crate itself passes the\nlicense check.\n\nCo-authored-by: Claude Opus 4.8 <noreply@anthropic.com>",
          "timestamp": "2026-06-27T16:34:13+08:00",
          "tree_id": "fc6f2bd2d10c7ed37ff6b0e0295d731a4d899b90",
          "url": "https://github.com/henry40408/lur/commit/12bb0ab6be3f43b203fbb1f394c162053c16ff1f"
        },
        "date": 1782549326560,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 261252,
            "range": "± 11771",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5291,
            "range": "± 23",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 208145,
            "range": "± 2803",
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
          "id": "0d32e0b09842bf40bd516d3cd8f6779f982c0006",
          "message": "docs: add CLAUDE.md for Claude Code guidance (#33)\n\nAdd a concise CLAUDE.md pointing to the existing README/ARCHITECTURE docs,\nplus the commands and load-bearing sandbox/pool invariants that constrain\nedits.\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-27T18:47:08+08:00",
          "tree_id": "b9f16aa936e2393e2cc4305aa025ab6537a4033a",
          "url": "https://github.com/henry40408/lur/commit/0d32e0b09842bf40bd516d3cd8f6779f982c0006"
        },
        "date": 1782557307902,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 253618,
            "range": "± 4291",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5198,
            "range": "± 63",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 209492,
            "range": "± 6703",
            "unit": "ns/iter"
          }
        ]
      }
    ]
  }
}