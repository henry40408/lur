window.BENCHMARK_DATA = {
  "lastUpdate": 1783845290614,
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
          "id": "4521cae1fa1ed708ddb751673408722409a16ef0",
          "message": "chore: adopt shared clippy lint set (#34)\n\nAdd the shared Embark-derived [lints.rust]/[lints.clippy] configuration to\nCargo.toml and resolve the resulting findings (idiomatic-Rust improvements:\ndoc_markdown, map_err_ignore, single_match_else, match_same_arms,\nunnested_or_patterns, needless_continue, map_unwrap_or). CI already runs clippy\nwith --all-targets.\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-27T22:42:04+08:00",
          "tree_id": "826f9a6ab91ff293bb26c7082fd29df5a8c9cd22",
          "url": "https://github.com/henry40408/lur/commit/4521cae1fa1ed708ddb751673408722409a16ef0"
        },
        "date": 1782571395850,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 255198,
            "range": "± 3657",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5342,
            "range": "± 31",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 206972,
            "range": "± 4355",
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
          "id": "2067b6eb5abdc2c1659da3562e22d4e907412f18",
          "message": "ci: drop job-level name fields, use job keys for display (#36)\n\nStandardize CI job naming across repos: rely on the job key for the\nActions display name instead of an explicit `name:`. Removes the\njob-level names (fmt + clippy, nextest, coverage, perf gate, cargo-deny)\nso the convention matches the majority of sibling projects. Step-level\nnames are unaffected. No required status checks reference these contexts.\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-27T23:30:17+08:00",
          "tree_id": "54ddafb8d32312af19c1dc90b5ad260e43b2a1e4",
          "url": "https://github.com/henry40408/lur/commit/2067b6eb5abdc2c1659da3562e22d4e907412f18"
        },
        "date": 1782574278206,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 255021,
            "range": "± 4633",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5185,
            "range": "± 30",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 211199,
            "range": "± 2017",
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
          "id": "224ea4c1f791cf9ce3478e3f84c551de466ba057",
          "message": "build: add multi-arch musl Docker image and release workflow (#37)\n\nShip lur as a static-musl Docker image for linux/amd64 and linux/arm64.\n\n- Dockerfile cross-compiles with cargo-zigbuild: the builder is pinned to\n  the native build platform and zig targets each arch's musl triple, so no\n  qemu emulation is needed (arm64 builds at native speed). The runtime stage\n  is gcr.io/distroless/static (CA certs bundled, non-root, no shell).\n- scripts/docker-build.sh wraps buildx for local single-arch (--load) or\n  multi-arch (--push) builds.\n- .github/workflows/docker.yml builds and pushes to GHCR: :main tracks the\n  main branch, releases publish :X.Y.Z, :X.Y, and :latest. Actions are pinned\n  to commit SHAs.\n- README documents pulling and building the image.\n\nVerified locally: both arches build, run --version, complete a live HTTPS\nrequest through the distroless CA store, and run as non-root; images are\n~5.9MB. Single-invocation multi-arch build succeeds.\n\nCo-authored-by: Claude Opus 4.8 <noreply@anthropic.com>",
          "timestamp": "2026-06-27T23:51:41+08:00",
          "tree_id": "406a1eea5e5c8d52fb21dff86ae71d0c875b0c58",
          "url": "https://github.com/henry40408/lur/commit/224ea4c1f791cf9ce3478e3f84c551de466ba057"
        },
        "date": 1782575572501,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 254106,
            "range": "± 1922",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5345,
            "range": "± 88",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 207453,
            "range": "± 1026",
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
          "id": "111ae734b69919a8344dc722833e11e1b1dd13fe",
          "message": "ci: cache Docker build layers via GitHub Actions cache backend (#38)\n\nThe Docker workflow rebuilt the image from scratch on every run: the\nephemeral runner had no layer cache and build-push-action set no\ncache-from/cache-to. Add type=gha cache (mode=max) so layers like the\napt install, cargo-zigbuild build, and zig download are reused across\nruns.\n\nNote: this does not persist the in-Dockerfile cargo/target cache mounts,\nwhich are local to a runner and would need a separate mechanism.\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-28T00:47:31+08:00",
          "tree_id": "39b19caf9ed1f2ebc4943b47dbc1cd84fc689b97",
          "url": "https://github.com/henry40408/lur/commit/111ae734b69919a8344dc722833e11e1b1dd13fe"
        },
        "date": 1782578924461,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 255820,
            "range": "± 17001",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5276,
            "range": "± 100",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 207132,
            "range": "± 4167",
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
          "id": "7f653bd20ccfda8e7ab8fa837b74f7902ec62fb1",
          "message": "feat: stamp lur --version with a layered version string (#40)\n\n`lur --version` reported the static Cargo.toml version (0.1.0) for every\nbuild. Resolve a meaningful, traceable version at compile time instead,\nlayered most-authoritative first:\n\n  1. the LUR_VERSION env var, if non-empty — the release workflow injects\n     the published tag, and the rolling :main image a <ref>-<short-sha>\n     marker;\n  2. else `git describe --tags --always --dirty` — local source builds get\n     <tag>-<n>-g<sha>[-dirty] (or a bare short sha);\n  3. else the literal \"dev\".\n\nA build.rs performs this resolution and emits it as a rustc-env that clap's\nversion attribute consumes via env!. The Docker build context excludes .git,\nso inside the image only steps 1 and 3 apply — which is why docker.yml\ncomputes the value (tag for a release, <ref>-<sha> otherwise) and passes it\nas a LUR_VERSION build-arg the Dockerfile exports into the cargo build.\nscripts/docker-build.sh forwards its TAG knob the same way.\n\nThis avoids depending on the Cargo.toml version field. The cli test pins the\noutput format and the override (lur <non-empty>, never 0.1.0) rather than a\nliteral, since the value is build-time-resolved. README documents the\nbehavior.\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-28T01:21:42+08:00",
          "tree_id": "866d4e9d8c3474d615a08f426319734622d4ea7a",
          "url": "https://github.com/henry40408/lur/commit/7f653bd20ccfda8e7ab8fa837b74f7902ec62fb1"
        },
        "date": 1782580975866,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 265221,
            "range": "± 11816",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5292,
            "range": "± 108",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 210879,
            "range": "± 6316",
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
          "id": "7cbf91946f81667664eb313d94011dbc5c5dbf93",
          "message": "refactor: rename LUR_VERSION build env var to GIT_VERSION (#41)\n\nAdopt the sibling `noadd` project's convention so the version-stamp knob\nhas the same name across both repos. The build.rs env layer now also\ntreats the literal `dev` as unset (alongside empty), falling through to\n`git describe` — which lets the Dockerfile default `ARG GIT_VERSION=dev`\nwithout coincidentally stamping a real \"dev\" version.\n\nNo behavior change for releases or the rolling `:main` image: docker.yml\nstill injects the resolved value, and the Docker context excludes .git so\nan un-injected build resolves to \"dev\" as before.\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-28T01:41:41+08:00",
          "tree_id": "937e88a0fa1b9bcf2415642127bb5e93882af56c",
          "url": "https://github.com/henry40408/lur/commit/7cbf91946f81667664eb313d94011dbc5c5dbf93"
        },
        "date": 1782582168225,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 253280,
            "range": "± 18090",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5275,
            "range": "± 211",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 206940,
            "range": "± 1993",
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
          "id": "93e56dad3add704a3d3ea647ee0eb68ca74e505b",
          "message": "ci: drop blank lines between docker workflow steps (#42)\n\nEach step already begins with its own `- uses:`/`- id:` marker, so the blank\nlines between steps add nothing; keep only the top-level block separators.\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-28T16:13:29+08:00",
          "tree_id": "3bc052081a861f327abab8f2607fa1268664388b",
          "url": "https://github.com/henry40408/lur/commit/93e56dad3add704a3d3ea647ee0eb68ca74e505b"
        },
        "date": 1782634483774,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 255099,
            "range": "± 4239",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5204,
            "range": "± 19",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 206915,
            "range": "± 4471",
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
          "id": "edb54a779fe55a2182b88b6b43616ca5f4618c2e",
          "message": "chore(deps): bump codecov/codecov-action in the actions group (#39)\n\nBumps the actions group with 1 update: [codecov/codecov-action](https://github.com/codecov/codecov-action).\n\n\nUpdates `codecov/codecov-action` from 5.5.5 to 7.0.0\n- [Release notes](https://github.com/codecov/codecov-action/releases)\n- [Changelog](https://github.com/codecov/codecov-action/blob/main/CHANGELOG.md)\n- [Commits](https://github.com/codecov/codecov-action/compare/0fb7174895f61a3b6b78fc075e0cd60383518dac...fb8b3582c8e4def4969c97caa2f19720cb33a72f)\n\n---\nupdated-dependencies:\n- dependency-name: codecov/codecov-action\n  dependency-version: 7.0.0\n  dependency-type: direct:production\n  update-type: version-update:semver-major\n  dependency-group: actions\n...\n\nSigned-off-by: dependabot[bot] <support@github.com>\nCo-authored-by: dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>",
          "timestamp": "2026-06-28T16:18:48+08:00",
          "tree_id": "333d9da53611d67d8860dcc3bb876b6008f522d8",
          "url": "https://github.com/henry40408/lur/commit/edb54a779fe55a2182b88b6b43616ca5f4618c2e"
        },
        "date": 1782634796109,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 221860,
            "range": "± 6947",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5047,
            "range": "± 42",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 390377,
            "range": "± 1334",
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
          "id": "690fb54e056c45819ded410625c039cae0fee320",
          "message": "docs: unify README format (#43)\n\n* docs: add status badges to README\n\nBring lur's README in line with the sibling projects (comics, noadd,\nliftlog, rdrs) by adding the standard badge row: CI, Codecov, Release,\nLicense, Rust toolchain, Docker, Casual Maintenance, and Vibe Coded.\n\nThe Rust toolchain badge reads the channel from rust-toolchain.toml,\nwhich is added here pinned to `stable` to match lur's existing\nfloating-stable setup (CI uses dtolnay stable; the Docker base is\nrust:1-bookworm). This keeps the toolchain declarative without changing\nthe resolved version.\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n* docs: unify README header block and section naming\n\nAdd a one-line blockquote tagline under the title and normalize the\n\"Quick start\" heading to \"Quick Start\", matching the shared README\nformat adopted across the sibling projects.\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n* chore: pin rust-toolchain to 1.96.0\n\nPin the toolchain channel to a specific version instead of `stable`, so\nthe Rust toolchain badge and builds match the sibling projects (comics,\nnoadd, liftlog, rdrs all pin 1.96.0).\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Opus 4.8 <noreply@anthropic.com>",
          "timestamp": "2026-06-28T17:11:11+08:00",
          "tree_id": "cb337af2296f8c3652f529b5dd28f2d91c6ebd9a",
          "url": "https://github.com/henry40408/lur/commit/690fb54e056c45819ded410625c039cae0fee320"
        },
        "date": 1782638079517,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 249092,
            "range": "± 2324",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5207,
            "range": "± 47",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 207266,
            "range": "± 1567",
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
          "id": "79acbfdbed7b70a4695927bf8dab04133d447012",
          "message": "feat: add lur.crypto capability (hashing, HMAC, hex, random, constant_eq) (#44)\n\n* docs: add lur.crypto design spec\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs: add lur.crypto implementation plan\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* feat(crypto): add lur.crypto module with hex encode/decode\n\n* feat(crypto): add sha256/sha512/sha1/md5 hashing\n\n* feat(crypto): add hmac_sha256/hmac_sha512/hmac_sha1\n\nAdds three HMAC functions to lur.crypto using hmac 0.12.1 (digest 0.10\ncompatible with existing sha2/sha1). Uses explicit per-algorithm closures\n(Hmac::<Sha256/Sha512/Sha1>::new_from_slice) rather than a generic helper\nbecause the D: Digest + BlockSizeUser bound requires additional CoreProxy\nconstraints that are fiddly to express generically. Tests assert RFC 4231\nJefe vectors for hmac_sha256/hmac_sha1 and byte-length for hmac_sha512.\n\n* feat(crypto): add constant_eq timing-safe comparison\n\n* feat(crypto): add random_bytes from the OS CSPRNG\n\n* docs(crypto): document lur.crypto and add end-to-end webhook test\n\n---------\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-28T23:06:37+08:00",
          "tree_id": "5ea846fe678fd64573f2f044f7b04170fae5266b",
          "url": "https://github.com/henry40408/lur/commit/79acbfdbed7b70a4695927bf8dab04133d447012"
        },
        "date": 1782659281673,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 231277,
            "range": "± 2665",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5347,
            "range": "± 30",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 206560,
            "range": "± 2375",
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
          "id": "a6d587b4961e09f339c5a428447c5f19d0666510",
          "message": "feat(serve): let handlers set response headers (#45)\n\n* docs: add serve response headers design spec\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs: add serve response headers implementation plan\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-28T23:56:40+08:00",
          "tree_id": "27b838f523f96b6c18f3d38e0e28a0f987bd5e93",
          "url": "https://github.com/henry40408/lur/commit/a6d587b4961e09f339c5a428447c5f19d0666510"
        },
        "date": 1782662271727,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 228536,
            "range": "± 2119",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5275,
            "range": "± 21",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 392056,
            "range": "± 12333",
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
          "id": "21b92634c6fc693210c8ee85f8d64468b45ff4f4",
          "message": "feat: add lur.cookie capability (parse & serialize) (#46)\n\n* docs: add lur.cookie design spec\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs: add lur.cookie implementation plan\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* feat: add lur.cookie.parse\n\n* feat: add lur.cookie.serialize\n\n* docs: document lur.cookie\n\nAdd lur.cookie entry to the README Data & I/O section with parse/serialize\ninterface documentation, and update ARCHITECTURE capability-order line to\nreflect cookie's position after crypto in the build sequence.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* test: broaden lur.cookie serialize rejection coverage; tighten max_age bound\n\n---------\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-29T22:37:11+08:00",
          "tree_id": "42642af28a1386e220fb043f6b943068f83d18fa",
          "url": "https://github.com/henry40408/lur/commit/21b92634c6fc693210c8ee85f8d64468b45ff4f4"
        },
        "date": 1782743903292,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 270313,
            "range": "± 2243",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5322,
            "range": "± 182",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 208178,
            "range": "± 926",
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
          "id": "ed2d8a4367e3d824fca2c281818ee16dd7d1e891",
          "message": "feat(serve): expose parsed cookies as req.cookies (#47)\n\n* docs: add req.cookies design spec\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs: add req.cookies implementation plan\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* refactor: extract cookie_pairs as the shared cookie parser\n\n* feat(serve): expose parsed cookies as req.cookies\n\n* docs: include cookies in build_req rustdoc\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-29T23:17:06+08:00",
          "tree_id": "d3e6b383166a82f8d5e1b316790f77441cd42a9b",
          "url": "https://github.com/henry40408/lur/commit/ed2d8a4367e3d824fca2c281818ee16dd7d1e891"
        },
        "date": 1782746300116,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 269534,
            "range": "± 2465",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5271,
            "range": "± 541",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 242638,
            "range": "± 4954",
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
          "id": "0ead9bfde77a993cb3a5bd5bb3a0d7ad236df2ae",
          "message": "feat: add lur.time capability (clocks + timestamp parsing) (#48)\n\n* docs: add lur.time design spec\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs: add lur.time implementation plan\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* feat: add lur.time clocks (now_ms, monotonic_ms)\n\n* feat: add lur.time parsers (parse_rfc3339, parse_http_date)\n\n* docs: document lur.time\n\n* refactor: tidy lur.time final-review nits\n\nMove httpdate to its correct alphabetical slot in Cargo.toml, order install_clocks before install_parsers, and note RFC 3339 requires an explicit offset.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-29T23:59:21+08:00",
          "tree_id": "6f5aa93ce02c8163ddaa5f804fbb09b53826851b",
          "url": "https://github.com/henry40408/lur/commit/0ead9bfde77a993cb3a5bd5bb3a0d7ad236df2ae"
        },
        "date": 1782748832041,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 269529,
            "range": "± 3189",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5359,
            "range": "± 171",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 207965,
            "range": "± 2640",
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
          "id": "42e142cb96b19d05e20fdeff29a4b0e82607b6f1",
          "message": "feat(diagnostics): point errors at the user's script, render rustc-style (#49)\n\n* docs: add diagnostics design spec\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs: correct diagnostics spec exit-code description\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs: add diagnostics plan 1 (chunk naming + rendering)\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* feat(diagnostics): name script chunks from the CLI path\n\n* feat(diagnostics): rustc-style error renderer in one-shot mode\n\n* fix(diagnostics): reject line 0 in parse_location to avoid underflow\n\n* feat(diagnostics): render server handler/cron errors; document diagnostics\n\n* test(diagnostics): cover syntax-error path and column caret\n\nCloses the spec testing gap flagged in final review (item 8): the syntax-error render path and the column-present caret alignment were untested.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-30T01:03:40+08:00",
          "tree_id": "2ee02b4e63336b59b21bb802f06c319d6bbdbe6f",
          "url": "https://github.com/henry40408/lur/commit/42e142cb96b19d05e20fdeff29a4b0e82607b6f1"
        },
        "date": 1782752732488,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 276725,
            "range": "± 2656",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5333,
            "range": "± 229",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 209736,
            "range": "± 5129",
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
          "id": "dd246ce6ce6d8af265efb1cf835b2ed7b6b52914",
          "message": "feat(diagnostics): lur-voiced capability argument errors (#50)\n\n* docs: add diagnostics plan 2 (capability arg messages)\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* feat(diagnostics): add argcheck helper for lur-voiced arg errors\n\n* feat(diagnostics): lur-voiced arg errors in lur.crypto\n\nMigrate all 8 create_function sites in lur.crypto to use\nargcheck::arg so type mismatches produce lur-voiced messages.\nThread fname through hash_fn<D>. Remove #[allow(dead_code)]\nfrom argcheck now that it has a real consumer.\n\nNote: in mlua+Luau, pcall errors from Rust are WrappedFailure\nuserdata (not plain strings); tostring() is required to get\nthe human-readable message in Lua assertions.\n\n* docs: fix plan 2 test idiom (tostring on pcall errors for luau)\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* feat(diagnostics): lur-voiced arg errors in base64/cookie/time/json\n\n* feat(diagnostics): lur-voiced arg errors in io/fs/env/log/state\n\n* docs: document lur-voiced capability argument errors\n\n---------\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-30T08:27:50+08:00",
          "tree_id": "b46e963adb9fb320f463f0f5426ff399f1e95e9f",
          "url": "https://github.com/henry40408/lur/commit/dd246ce6ce6d8af265efb1cf835b2ed7b6b52914"
        },
        "date": 1782779343826,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 272907,
            "range": "± 7732",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5141,
            "range": "± 175",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 207770,
            "range": "± 6834",
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
          "id": "a030a1dfcbac09be59743fe81fe2c55b46bbd820",
          "message": "feat(diagnostics): colorize error output (NO_COLOR) + tidy #49 deferred polish (#51)\n\n* chore(diagnostics): unify error fallback payload and tidy #49 deferred polish\n\n- Out-of-range locations now fall back to `lur: {body}` like the\n  unparsable/line-zero cases, preserving the location text instead of\n  dropping it to just the message.\n- serve.rs: `as_deref().unwrap_or().to_owned()` over `clone().unwrap_or_else`.\n- ARCHITECTURE: regroup the diagnostics module-map row next to its\n  runtime/serve consumers and note the `lur: {body}` fallback.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* feat(diagnostics): colorize error output, honoring NO_COLOR\n\nRender rustc-style diagnostics with ANSI color (bold-red `error:`/caret,\nbold-blue gutter and `-->`) when stderr is a TTY. Color is suppressed for\nnon-TTY stderr (pipe/redirect) and when NO_COLOR is set to a non-empty value\n(no-color.org de-facto standard).\n\n`render` gains a `color: bool`; the gate lives in `color_from_env` (pure,\nunit-tested truth table) wired by `stderr_color`. With color off every escape\nstring is empty, so plain output is byte-identical to before.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-30T18:05:19+08:00",
          "tree_id": "a0ee4644194e2fd2b32a7c3bcecba0f95b651082",
          "url": "https://github.com/henry40408/lur/commit/a030a1dfcbac09be59743fe81fe2c55b46bbd820"
        },
        "date": 1782813988430,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 268241,
            "range": "± 6173",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5204,
            "range": "± 115",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 208210,
            "range": "± 1938",
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
          "id": "c6da0b5aeb3ba298c268e79aa221a1126226791f",
          "message": "feat(docs): embedded `lur docs` cookbook — styled rendering + tested examples (#52)\n\n* docs(spec): guided-tour embedded `lur docs` cookbook design\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs(spec): render `lur docs` via pulldown-cmark + hand-rolled ANSI sink\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs(plan): guided-tour implementation plan; spec covers kv+async\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* refactor(color): extract shared NO_COLOR/TTY gate into src/color.rs\n\n* feat(docs): markdown renderer for lur docs over pulldown-cmark\n\n* feat(docs): add lur docs subcommand and guide skeleton\n\n* test(docs): run guide lua examples + assert capability coverage\n\n* docs(guide): json, base64, crypto, cookie, time sections\n\n* docs(guide): log, io, args, state, async sections\n\n* docs(guide): fs, env, db, kv sections\n\n* docs(guide): http, serve sections; document new modules\n\n* fix(docs): strengthen guide harness floor; tidy renderer per review\n\nCo-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>\n\n* docs(spec): lur docs render overhaul — glamour-aligned layout + Lua highlight\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs(plan): lur docs render overhaul implementation plan\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* feat(docs): hand-rolled Lua syntax highlighter\n\n* feat(docs): glamour-aligned heading hierarchy and framed code blocks\n\n* fix(docs): indent wrapped body lines to section margin; review tidy\n\nCo-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>\n\n* test(docs): function-level coverage guard; example every lur API\n\nA reflection-based guard walks the live lur table and asserts every exposed\nfunction is called in a guide code block, then fills the 8 gaps it surfaced\n(async.race/any, http.request/put/patch/delete/head, stdin.read).\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-30T22:30:58+08:00",
          "tree_id": "8e6c4e62080b2b3b1d3507238f39b222ae71661a",
          "url": "https://github.com/henry40408/lur/commit/c6da0b5aeb3ba298c268e79aa221a1126226791f"
        },
        "date": 1782830077013,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 272370,
            "range": "± 3407",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5367,
            "range": "± 97",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 207864,
            "range": "± 4350",
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
          "id": "6f8220bb09e4345e85e8aea1bf5390538c86d1fb",
          "message": "fix(diagnostics): align caret under tab-indented source lines (#53)\n\nThe caret line padded with single spaces counted from the byte offset of\nthe first non-whitespace char. A leading tab is one byte but renders to a\ntab stop, so the caret drifted left of the statement on tab-indented code.\nMirror the source prefix character-for-character instead — keep tabs as\ntabs, count other chars (multibyte-safe) as single spaces — so the caret\nlands at the same terminal tab stop as the code it points at.\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-30T22:51:24+08:00",
          "tree_id": "9a03665c1de1825096d82fed113e55aa177a0a59",
          "url": "https://github.com/henry40408/lur/commit/6f8220bb09e4345e85e8aea1bf5390538c86d1fb"
        },
        "date": 1782831212452,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 276697,
            "range": "± 2379",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5268,
            "range": "± 330",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 207331,
            "range": "± 1640",
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
          "id": "47bedea2dd1439bb7b2e3265b0816a949b30c460",
          "message": "feat(storage): atomic operations for lur.kv & lur.state + db busy handling (#54)\n\n* docs(spec): storage atomic operations — kv/state parity + db busy handling\n\nDesign for: lur.kv atomic ops (incr/decr, add, cas, update) over SQLite,\nmatching primitives on lur.state (cas, add, decr; incr → integer), and\nbusy handling for lur.db (busy_timeout + BEGIN IMMEDIATE for tx/update).\nAlso fixes a latent lur.kv.get crash on non-BLOB cells via a type-aware\ndecoder.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs(plan): storage atomic operations implementation plan\n\n8 tasks: extract kv module + type-aware get, db busy handling\n(busy_timeout + BEGIN IMMEDIATE), kv add/cas/incr/decr/update,\nstate integer incr + decr/cas/add, and docs.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* refactor(kv): extract lur.kv into its own module; type-aware get\n\n* feat(db): busy_timeout + BEGIN IMMEDIATE write transactions\n\n* feat(kv): add (set-if-absent) and cas (compare-and-set)\n\n* feat(kv): integer incr/decr counters with a non-integer guard\n\n* fix(kv): voice incr_by errors per-function (incr/decr)\n\n* feat(kv): update (read-modify-write) with a re-entry guard\n\n* fix(kv): roll back update transaction on read/write/commit errors\n\n* feat(state): integer incr + decr (reject fractional/non-integer)\n\n* fix(kv): reject fractional incr/decr step (Luau truncates silently)\n\nCo-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>\n\n* feat(state): add lur.state.cas (value-based CAS) and lur.state.add (set-if-absent)\n\n- Derive `PartialEq` on `Prim` to enable value comparison\n- Add `StateStore::cas_value(key, expected, new) -> bool`: snapshots current\n  value, compares by value (not version), then delegates to `compare_and_set`\n- Wire `lur.state.cas(key, expected, new)` and `lur.state.add(key, value)` into\n  the install function with `argcheck` key validation and re-entry guard\n- Add runnable assert-based examples for `cas` and `add` in docs/GUIDE.md\n- Add five integration tests covering all four nil/value combos and error voicing\n\n* refactor(state): use shared argcheck::integer_arg helper\n\n* docs: document kv/state atomic ops and db busy handling\n\n* fix(db): roll back tx on commit failure; test REAL decode; doc cas/update caveats\n\n---------\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-07-01T02:12:14+08:00",
          "tree_id": "8a27a83c98430fbc039d063879423980e11760fe",
          "url": "https://github.com/henry40408/lur/commit/47bedea2dd1439bb7b2e3265b0816a949b30c460"
        },
        "date": 1782843214768,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 291439,
            "range": "± 5771",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5207,
            "range": "± 60",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 210921,
            "range": "± 4445",
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
          "id": "a6dfcef728fb7d15858b62d29b3eeadd7cf86f0f",
          "message": "test(kv): verify incr atomicity under concurrent writers (#55)\n\nTwo writer threads, each its own Runtime + SQLite pool pointing at one db\nfile, run a tight lur.kv.incr loop; the final counter must equal threads ×\nper-thread exactly, proving the single guarded upsert loses no update when\nWAL serializes concurrent writers (one writes, the other waits out the lock\nvia busy_timeout and retries). The counter is seeded in one thread first so\nthe workers contend on writes, not on the cold-open WAL-mode switch.\n\nTwo writers, not more: 3+ threads hammering one key can thundering-herd into\nbusy_timeout exhaustion on a slow CI runner — a SQLite write-contention\ncharacteristic, not an atomicity defect — and two writers already exercise\nthe serialize-and-retry path the claim is about.\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-07-01T02:26:44+08:00",
          "tree_id": "bcc9cd3e1760b7df02a2274b0d663502d3bdb3ec",
          "url": "https://github.com/henry40408/lur/commit/a6dfcef728fb7d15858b62d29b3eeadd7cf86f0f"
        },
        "date": 1782844082200,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 292465,
            "range": "± 5525",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5243,
            "range": "± 100",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 208231,
            "range": "± 8971",
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
          "id": "400b98b60c2cf081a7b19b8657e061c94cb2abc7",
          "message": "feat(storage): retry-with-jitter for SQLite write-lock contention (#56)\n\n* docs(spec): DB write retry-with-jitter design\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs(plan): DB write retry-with-jitter implementation plan\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* feat(storage): retry-with-jitter for db.exec busy contention\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* feat(storage): retry begin_immediate lock acquisition (db.tx, kv.update)\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* feat(storage): retry-with-jitter for kv add/cas/incr/decr; bump concurrency guard to 4 writers\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs(storage): describe db retry-with-jitter and 200ms busy_timeout\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* test(storage): run is_busy syntax-error check on a held connection\n\nThe pool (max 2) is fully checked out by the two lock-holding connections, so\nexecuting the non-busy syntax query against the pool blocked on connection\nacquisition for the full 30 s timeout and asserted against a PoolTimedOut error\nrather than a genuine syntax error. Run it on the already-held connection: the\ntest now exercises a real non-busy DB error and finishes in milliseconds.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-07-01T09:18:30+08:00",
          "tree_id": "5c71c7a989d61b4c2b3edb241e8fdd4b554da809",
          "url": "https://github.com/henry40408/lur/commit/400b98b60c2cf081a7b19b8657e061c94cb2abc7"
        },
        "date": 1782868787653,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 263195,
            "range": "± 2931",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5200,
            "range": "± 308",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 206449,
            "range": "± 1548",
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
          "id": "31d220266aad6011134187a24d1e6e690ad8f57d",
          "message": "refactor(storage): extract backend seam (PostgreSQL support, Phase 1) (#57)\n\n* docs(spec): storage backend seam (Phase 1 of PostgreSQL support)\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs(plan): storage backend seam (PostgreSQL Phase 1) implementation plan\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* refactor(storage): move SQLite leaf helpers into storage/sqlite.rs\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* refactor(storage): add Backend seam and migrate lur.db onto it\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* fix(storage): bind kv.update value as BLOB via dedicated kv_update backend method\n\nFixes a TEXT-vs-BLOB storage-class regression from the generic bind path that broke lur.kv.cas on keys written via kv.update.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* refactor(storage): migrate lur.kv onto Backend methods; drop transitional pool accessor\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs(storage): document the backend seam in ARCHITECTURE\n\nPerf gate: cargo bench --bench runtime after Tasks 1-3 vs. the pre-refactor\nbaseline shows all three benchmarks flat/within noise (vm_cold_start +1.3-1.8%,\ntrivial_script +0.3-0.85%, compute_loop_hook_overhead -1.2-0%) — well under the\n5% regression threshold, so no blocking regression.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* fix(storage): scope kv.update reentrancy guard to the transform only\n\nThe IN_KV_UPDATE guard was spanning kv.update's whole transaction (begin/read/write/commit awaits), not just the user transform, so a sibling lur.async kv/db call polled while an update was parked on DB I/O was spuriously rejected as re-entry.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n* docs(spec): correct kv.update guard-scope equivalence premise\n\nThe earlier equivalence argument covered same-stack nesting but overlooked\nlur.async concurrent interleaving; require the narrow guard window instead.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-07-01T11:43:26+08:00",
          "tree_id": "18fdf1f66aa49435a02642212166fd8a80d48358",
          "url": "https://github.com/henry40408/lur/commit/31d220266aad6011134187a24d1e6e690ad8f57d"
        },
        "date": 1782877486868,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 262209,
            "range": "± 6046",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5178,
            "range": "± 178",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 208100,
            "range": "± 3198",
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
          "id": "85f6f04297c535bb26b59d6f0631a6bb7d5f1203",
          "message": "feat(storage): PostgreSQL backend (PG support Phase 2) (#58)\n\n* docs(storage): design spec for PostgreSQL backend (Phase 2)\n\nAdds the Postgres variant behind the Phase 1 storage seam. Key decisions:\nnative placeholders per backend (no translation), kind-discriminated kv\nschema mapping the neutral model 1:1, retry stays SQLite-only, and the\nload-bearing correctness model — single-statement ops at READ COMMITTED,\ndb.tx/kv.update at SERIALIZABLE and documented as fallible (40001 surfaces;\ncaller decides retry/pcall). rustls TLS via sslmode; docker-compose.yaml +\nCI service for tests.\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n* docs(storage): Phase 2 implementation plan + spec refinements\n\nPlan decomposes the PostgreSQL backend into vertical slices (db.exec/query →\ndb.tx → kv → kv.update → SERIALIZABLE fallibility → docs), each independently\ntestable against a docker-compose Postgres. Spec refined to match sqlx reality:\nrow mapping is R1 (core types map, non-core raise a cast-to-text error, since\nsqlx returns binary and cannot generically stringify), and RuntimeConfig.db_path\nstays Option<PathBuf> with scheme detection internal to storage (preserves the\nunchanged-existing-tests constraint).\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n* build(storage): enable sqlx postgres + rustls, add PG compose & CI service\n\n* feat(storage): PostgreSQL backend db.exec/db.query behind the seam\n\n* feat(storage): db.tx over a SERIALIZABLE Postgres transaction\n\n* feat(storage): Postgres kv get/set/delete/add/cas/incr\n\n* docs(storage): restore begin() doc comment orphaned by kv insertion\n\n* feat(storage): Postgres kv.update (SERIALIZABLE read-modify-write)\n\nImplements PgBackend::kv_update: a BEGIN ISOLATION LEVEL SERIALIZABLE\ntransaction on a pinned connection that reads the current value\n(type-aware, via kv_row_to_bytes), calls the Lua transform, then\nwrites the returned string as kind=0 bytes (cas-comparable) or deletes\non nil, committing or rolling back and re-raising on any error. No\nretry — a 40001 conflict surfaces to the caller unchanged (SQLite-only\nretry stays as-is). Replaces the last \"not yet implemented\" arm in\nstorage/mod.rs.\n\nAlso adds .config/nextest.toml to serialize the `pg` test binary: the\nnew kv_update tests run SERIALIZABLE transactions against the shared\nlur_kv table, and PostgreSQL's predicate locking can produce false-\npositive 40001 conflicts when those interleave with the other kv_*\npg tests running concurrently. Confirmed via repeated runs: reliably\ngreen serialized, reliably flaky (~every run) at default parallelism.\n\n* test(storage): Postgres SERIALIZABLE db.tx is fallible and pcall-catchable\n\n* docs(storage): document the PostgreSQL backend and fallible tx contract\n\n* ci(storage): add Postgres service to the coverage job\n\nThe coverage job runs the full suite (incl. tests/pg.rs) under llvm-cov\ninstrumentation. CI is set on every GitHub Actions job, so the pg tests'\nCI-hard-fail-when-unreachable path fired there because only the  job\nhad a Postgres service. Mirror that service onto the coverage job so the pg\ntests run (and get covered) instead of hard-failing.\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Opus 4.8 <noreply@anthropic.com>",
          "timestamp": "2026-07-02T01:01:35+08:00",
          "tree_id": "055f4fe4579f94707857423ceb29cf2c16984078",
          "url": "https://github.com/henry40408/lur/commit/85f6f04297c535bb26b59d6f0631a6bb7d5f1203"
        },
        "date": 1782925396770,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 294326,
            "range": "± 6526",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5214,
            "range": "± 55",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 208278,
            "range": "± 7573",
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
          "id": "8b70cd75029c8bbabdd876cc55cb2af78d5e53e4",
          "message": "fix: roll back cancelled db/kv transactions synchronously (#59)\n\n* docs(spec): transaction cancellation safety design\n\nFix two cancellation-during-transform resource leaks (backlog items 0\nand 0b): the IN_KV_UPDATE thread-local poison and the pinned connection\nleft mid-transaction. Design C: rollback-on-drop guard + synchronous\nRAII flag guard, preserving the existing isolation choices.\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n* docs(plan): transaction cancellation safety implementation plan\n\nThree tasks: (1) IN_KV_UPDATE RAII guard; (2) SQLite rollback-on-drop\nguard + kv_update refactor; (3) Postgres mirror + docs. Each with\ndeterministic single-connection regression tests.\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n* fix(kv): restore IN_KV_UPDATE via RAII guard on cancellation\n\nA kv.update transform cancelled mid-await left IN_KV_UPDATE stuck true,\npoisoning later kv/db calls on the pooled VM. Replace the manual\nset(true)/set(false) with an RAII guard whose Drop restores the prior\nvalue on every exit path (return, error, cancellation).\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n* fix(storage): roll back pinned SQLite tx on cancellation\n\nA db.tx body or kv.update transform cancelled mid-flight dropped the\npinned connection inside an open BEGIN IMMEDIATE, returning it to the\npool mid-transaction. Add a rollback-on-drop guard (Drop for\nSqliteTransaction + a PinnedTx wrapper for kv_update) that best-effort\nrolls back via a detached task so the connection returns clean.\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n* fix(storage): roll back pinned Postgres tx on cancellation\n\nMirror the SQLite cancellation-safety fix for Postgres, where a\ncancelled db.tx/kv.update left the connection idle-in-transaction holding\na SERIALIZABLE snapshot + row locks on the shared operator DB. Add\nDrop for PgTransaction + a PinnedTx wrapper for kv_update that detaches a\nbest-effort ROLLBACK. Adds a deterministic single-connection regression\ntest (in the pg-serial nextest group) and documents the invariant.\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n* fix(db): roll back run_tx transaction synchronously on cancellation\n\ndb.tx wrapped its Transaction in an Arc cloned into the exec/query Lua\nclosures, so a cancelled handler only dropped run_tx's local ref and the\nrollback was deferred to Luau GC — on Postgres holding SERIALIZABLE locks\non the shared DB until an idle VM's next GC. Hand the closures Weak refs so\nrun_tx holds the only strong Arc; cancellation drops it and fires\nTransaction::Drop (the detached ROLLBACK) synchronously, matching kv.update.\n\nAdd a run_tx-path regression test driving the real Arc/closure structure\nwith a single-connection pool.\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Opus 4.8 <noreply@anthropic.com>",
          "timestamp": "2026-07-02T08:58:18+08:00",
          "tree_id": "07bed2b89ec1a887476dce024d7e6ac7ec9c856a",
          "url": "https://github.com/henry40408/lur/commit/8b70cd75029c8bbabdd876cc55cb2af78d5e53e4"
        },
        "date": 1782953984722,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 294696,
            "range": "± 2473",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5187,
            "range": "± 442",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 209252,
            "range": "± 9020",
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
          "id": "829536c90a16702f4ce318336630010f85b00432",
          "message": "refactor: adopt tracing for serve-mode runtime events (#62)\n\nServe daemon runtime events (listening, connection error, shutdown\ndrain, handler error, cron timeout, cron error) now emit through\ntracing instead of eprintln!, gated by RUST_LOG (fallback\nerror,lur=info) and selectable via --log-format/LOG_FORMAT\n(full/compact/pretty/json). One-shot mode and user-facing CLI errors\nare unchanged and keep eprintln!.",
          "timestamp": "2026-07-05T20:53:13+08:00",
          "tree_id": "1acdff78c239156e865a8b4837d392f7714dd162",
          "url": "https://github.com/henry40408/lur/commit/829536c90a16702f4ce318336630010f85b00432"
        },
        "date": 1783256120671,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 261666,
            "range": "± 3749",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5179,
            "range": "± 88",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 206171,
            "range": "± 6958",
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
          "id": "b3a4d81d03716dae1f238d4052ac95c03604e749",
          "message": "chore(deps): bump taiki-e/install-action in the actions group (#60)\n\nBumps the actions group with 1 update: [taiki-e/install-action](https://github.com/taiki-e/install-action).\n\n\nUpdates `taiki-e/install-action` from 2.82.1 to 2.82.5\n- [Release notes](https://github.com/taiki-e/install-action/releases)\n- [Changelog](https://github.com/taiki-e/install-action/blob/main/CHANGELOG.md)\n- [Commits](https://github.com/taiki-e/install-action/compare/8b3c737da4b541bf0fb5a3e0488ff20535badac9...bffeee26d4db9be238a4ea78d8826604ebcb594d)\n\n---\nupdated-dependencies:\n- dependency-name: taiki-e/install-action\n  dependency-version: 2.82.5\n  dependency-type: direct:production\n  update-type: version-update:semver-patch\n  dependency-group: actions\n...\n\nSigned-off-by: dependabot[bot] <support@github.com>\nCo-authored-by: dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-05T21:19:27+08:00",
          "tree_id": "bd7d4f8fdd805089469b383bc07df5e03f85e571",
          "url": "https://github.com/henry40408/lur/commit/b3a4d81d03716dae1f238d4052ac95c03604e749"
        },
        "date": 1783257660071,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 299558,
            "range": "± 8748",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5341,
            "range": "± 96",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 207526,
            "range": "± 3391",
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
          "id": "ed93c137a51d03b47108e99dba3a8218167e6ec9",
          "message": "chore(deps): bump the cargo group across 1 directory with 5 updates (#61)\n\n* chore(deps): bump the cargo group across 1 directory with 5 updates\n\nBumps the cargo group with 5 updates in the / directory:\n\n| Package | From | To |\n| --- | --- | --- |\n| [getrandom](https://github.com/rust-random/getrandom) | `0.2.17` | `0.4.3` |\n| [hmac](https://github.com/RustCrypto/MACs) | `0.12.1` | `0.13.0` |\n| [md-5](https://github.com/RustCrypto/hashes) | `0.10.6` | `0.11.0` |\n| [sha1](https://github.com/RustCrypto/hashes) | `0.10.6` | `0.11.0` |\n| [sha2](https://github.com/RustCrypto/hashes) | `0.10.9` | `0.11.0` |\n\n\n\nUpdates `getrandom` from 0.2.17 to 0.4.3\n- [Changelog](https://github.com/rust-random/getrandom/blob/master/CHANGELOG.md)\n- [Commits](https://github.com/rust-random/getrandom/compare/v0.2.17...v0.4.3)\n\nUpdates `hmac` from 0.12.1 to 0.13.0\n- [Commits](https://github.com/RustCrypto/MACs/compare/hmac-v0.12.1...hmac-v0.13.0)\n\nUpdates `md-5` from 0.10.6 to 0.11.0\n- [Commits](https://github.com/RustCrypto/hashes/compare/md-5-v0.10.6...md2-v0.11.0)\n\nUpdates `sha1` from 0.10.6 to 0.11.0\n- [Commits](https://github.com/RustCrypto/hashes/compare/sha1-v0.10.6...sha1-v0.11.0)\n\nUpdates `sha2` from 0.10.9 to 0.11.0\n- [Commits](https://github.com/RustCrypto/hashes/compare/sha2-v0.10.9...sha2-v0.11.0)\n\n---\nupdated-dependencies:\n- dependency-name: getrandom\n  dependency-version: 0.4.3\n  dependency-type: direct:production\n  update-type: version-update:semver-minor\n  dependency-group: cargo\n- dependency-name: hmac\n  dependency-version: 0.13.0\n  dependency-type: direct:production\n  update-type: version-update:semver-minor\n  dependency-group: cargo\n- dependency-name: md-5\n  dependency-version: 0.11.0\n  dependency-type: direct:production\n  update-type: version-update:semver-minor\n  dependency-group: cargo\n- dependency-name: sha1\n  dependency-version: 0.11.0\n  dependency-type: direct:production\n  update-type: version-update:semver-minor\n  dependency-group: cargo\n- dependency-name: sha2\n  dependency-version: 0.11.0\n  dependency-type: direct:production\n  update-type: version-update:semver-minor\n  dependency-group: cargo\n...\n\nSigned-off-by: dependabot[bot] <support@github.com>\n\n* fix(crypto): adapt to getrandom 0.4 and RustCrypto 0.11/0.13 APIs\n\nThe dependency bump renames getrandom::getrandom to getrandom::fill and\nmoves Mac::new_from_slice to the KeyInit trait. Update the call sites and\nimports so the crate compiles against the new versions.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n---------\n\nSigned-off-by: dependabot[bot] <support@github.com>\nCo-authored-by: dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>\nCo-authored-by: Heng-Yi Wu <2316687+henry40408@users.noreply.github.com>\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-07-05T21:27:11+08:00",
          "tree_id": "bd7158af59cf2271ef82e54b4d188dbcc9346b2c",
          "url": "https://github.com/henry40408/lur/commit/ed93c137a51d03b47108e99dba3a8218167e6ec9"
        },
        "date": 1783258133213,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 295638,
            "range": "± 3207",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 5194,
            "range": "± 32",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 206353,
            "range": "± 3729",
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
          "id": "3a556a888ee109e1e77e4e09f1ddfef3c5f0d54b",
          "message": "chore(deps): bump the actions group with 5 updates (#63)\n\n* chore(deps): bump the actions group with 5 updates\n\nBumps the actions group with 5 updates:\n\n| Package | From | To |\n| --- | --- | --- |\n| [taiki-e/install-action](https://github.com/taiki-e/install-action) | `2.82.5` | `2.82.8` |\n| [docker/setup-buildx-action](https://github.com/docker/setup-buildx-action) | `4.1.0` | `4.2.0` |\n| [docker/login-action](https://github.com/docker/login-action) | `4.2.0` | `4.4.0` |\n| [docker/metadata-action](https://github.com/docker/metadata-action) | `6.1.0` | `6.2.0` |\n| [docker/build-push-action](https://github.com/docker/build-push-action) | `7.2.0` | `7.3.0` |\n\n\nUpdates `taiki-e/install-action` from 2.82.5 to 2.82.8\n- [Release notes](https://github.com/taiki-e/install-action/releases)\n- [Changelog](https://github.com/taiki-e/install-action/blob/main/CHANGELOG.md)\n- [Commits](https://github.com/taiki-e/install-action/compare/bffeee26d4db9be238a4ea78d8826604ebcb594d...c93ccc03e00cd0e08e494f5fd058a6c55a6a1907)\n\nUpdates `docker/setup-buildx-action` from 4.1.0 to 4.2.0\n- [Release notes](https://github.com/docker/setup-buildx-action/releases)\n- [Commits](https://github.com/docker/setup-buildx-action/compare/d7f5e7f509e45cec5c76c4d5afdd7de93d0b3df5...bb05f3f5519dd87d3ba754cc423b652a5edd6d2c)\n\nUpdates `docker/login-action` from 4.2.0 to 4.4.0\n- [Release notes](https://github.com/docker/login-action/releases)\n- [Commits](https://github.com/docker/login-action/compare/650006c6eb7dba73a995cc03b0b2d7f5ca915bee...af1e73f918a031802d376d3c8bbc3fe56130a9b0)\n\nUpdates `docker/metadata-action` from 6.1.0 to 6.2.0\n- [Release notes](https://github.com/docker/metadata-action/releases)\n- [Commits](https://github.com/docker/metadata-action/compare/80c7e94dd9b9319bd5eb7a0e0fe9291e23a2a2e9...dc802804100637a589fabce1cb79ff13a1411302)\n\nUpdates `docker/build-push-action` from 7.2.0 to 7.3.0\n- [Release notes](https://github.com/docker/build-push-action/releases)\n- [Commits](https://github.com/docker/build-push-action/compare/f9f3042f7e2789586610d6e8b85c8f03e5195baf...53b7df96c91f9c12dcc8a07bcb9ccacbed38856a)\n\n---\nupdated-dependencies:\n- dependency-name: taiki-e/install-action\n  dependency-version: 2.82.8\n  dependency-type: direct:production\n  update-type: version-update:semver-patch\n  dependency-group: actions\n- dependency-name: docker/setup-buildx-action\n  dependency-version: 4.2.0\n  dependency-type: direct:production\n  update-type: version-update:semver-minor\n  dependency-group: actions\n- dependency-name: docker/login-action\n  dependency-version: 4.4.0\n  dependency-type: direct:production\n  update-type: version-update:semver-minor\n  dependency-group: actions\n- dependency-name: docker/metadata-action\n  dependency-version: 6.2.0\n  dependency-type: direct:production\n  update-type: version-update:semver-minor\n  dependency-group: actions\n- dependency-name: docker/build-push-action\n  dependency-version: 7.3.0\n  dependency-type: direct:production\n  update-type: version-update:semver-minor\n  dependency-group: actions\n...\n\nSigned-off-by: dependabot[bot] <support@github.com>\n\n* chore(deny): temporarily ignore RUSTSEC-2026-0204\n\ncrossbeam-epoch 0.9.18 (dev/bench-only, via criterion) trips\nRUSTSEC-2026-0204. The fix (>=0.9.20) was published 2026-07-06, still\ninside the 7-day dependency cooldown, so ignore the advisory to unblock\nCI. Tracked for removal once the update lands.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>\n\n---------\n\nSigned-off-by: dependabot[bot] <support@github.com>\nCo-authored-by: dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>\nCo-authored-by: Heng-Yi Wu <2316687+henry40408@users.noreply.github.com>\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-07-12T16:30:54+08:00",
          "tree_id": "b732972ddcd2a9f7b7a46f5075a6a66b59ba4803",
          "url": "https://github.com/henry40408/lur/commit/3a556a888ee109e1e77e4e09f1ddfef3c5f0d54b"
        },
        "date": 1783845290313,
        "tool": "cargo",
        "benches": [
          {
            "name": "vm_cold_start",
            "value": 261443,
            "range": "± 3139",
            "unit": "ns/iter"
          },
          {
            "name": "trivial_script",
            "value": 4993,
            "range": "± 35",
            "unit": "ns/iter"
          },
          {
            "name": "compute_loop_hook_overhead",
            "value": 207189,
            "range": "± 3441",
            "unit": "ns/iter"
          }
        ]
      }
    ]
  }
}