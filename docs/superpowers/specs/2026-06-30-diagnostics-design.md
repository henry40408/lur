# lur diagnostics — locate-the-error reporting

Status: design approved, ready for implementation plan.
Date: 2026-06-30.

## Motivation

Today every error a user sees points at lur's own internals, not at their
script. Because `lua.load(source)` is called without a chunk name, mlua's
`#[track_caller]` fallback uses the **Rust** call site as the chunk name. A
runtime error on line 2 of `app.lua` prints:

```
runtime error: src/runtime.rs:212:2: attempt to index nil with 'y'
stack traceback:
	src/runtime.rs:212:2: in function <src/runtime.rs:212:1>
```

The user's real line number (`2`) is mashed onto an internal path
(`src/runtime.rs:212`), which looks like a lur bug and gives the user no way to
find the failing line. This is the headline defect this work fixes.

Beyond that, the host-side presentation is inconsistent (script errors print
mlua's raw `Display`; everything else is prefixed `lur:`), tracebacks carry
contentless noise frames (`[C]: in ?`), there is no source context around the
failing line, and capability argument-type errors speak in mlua's generic voice
rather than lur's.

## Scope and ordering

Four components, in dependency order. The first three are one cohesive
"error presentation" effort; the fourth is independent and is the broadest, so
it lands last and may be split into its own implementation plan.

1. **Chunk naming** — the core fix; everything else builds on it.
2. **Host-side formatting** — consistent `lur:` framing and noise-frame filtering.
3. **rustc-style renderer** — human-readable source snippet with a caret.
4. **Per-capability argument messages** — lur-voiced arg-type errors (last).

**Explicitly out of scope:**
- **Exit codes** stay exactly as they are today and are not redesigned: the
  existing `main.rs` mapping is unchanged (usage/read error `2`, timeout `124`,
  OOM `137`, other script errors `1`; a top-level `return <number>` still sets a
  custom code per spec §8). We deliberately do **not** introduce a new per-kind
  scheme — there is no cross-tool standard and it burdens users.
- **Machine-readable (JSON / SARIF) diagnostics** — a separate axis, deferred to
  a follow-up. Runtimes generally don't emit structured error reports (that role
  belongs to compilers/linters); we revisit only if a concrete consumer (CI
  gate, editor integration) appears. The renderer in component 3 should keep its
  parsed-location data in a small internal struct so a future `--error-format=json`
  can reuse it, but no JSON is emitted now.

## Component 1 — Chunk naming

Every `lua.load(source)` site sets a chunk name derived from the script/app path
the user gave on the CLI, using Lua's file convention (a leading `@`), so the
chunk renders as `app.lua:2:` rather than `[string "..."]` or the Rust location.

- **One-shot** (`src/runtime.rs`): the chunk name is the path from `cli.script`.
- **Server** (`src/serve.rs`): the chunk name is the path from `cli.app`.
- **Path form:** the path **as the user typed it** on the CLI (e.g. `./app.lua`
  or an absolute path) — not canonicalized (no surprise absolute paths) and not
  reduced to a basename (ambiguous across directories). Copy-pasteable as-is.
- **Threading:** the name travels from the CLI down to each `load` site. The
  mechanism is an optional field on `RuntimeConfig` (e.g. `chunk_name:
  Option<String>`) and a parameter/field on the server loader, set from
  `cli.script`/`cli.app`. A `Runtime` built with no name (the test helper
  `Runtime::new()`, and any other nameless caller) falls back to the clean
  generic name `script` — **never** the Rust location.

After this component, the example above already becomes:

```
lur: app.lua:2: attempt to index nil with 'y'
```

## Component 2 — Host-side formatting

A single place formats a `RunError` for stderr, used by both `main.rs` (one-shot)
and the server's error/cron logging:

- **Consistent framing:** every error kind reads as lur output. Script/runtime,
  syntax, timeout, OOM, and async-runtime failures all render under a uniform
  `lur:` prefix. (`Timeout`/`OutOfMemory` keep their existing wording.)
- **Noise-frame filtering:** in a traceback, drop frames that carry no location
  or name — specifically the exact line `[C]: in ?`. Keep every frame that names
  a function or a source position (including `[C]: in function 'error'`, which is
  informative). This is a safe whole-line filter, not location parsing.
- **Traceback default:** kept (filtered), not hidden. Locating an error inside
  nested calls is the whole point; the traceback follows the primary snippet.

## Component 3 — rustc-style renderer

A new `src/diagnostics.rs` module renders a runtime/syntax error against the
script source, rustc-style:

```
error: attempt to index nil with 'y'
 --> app.lua:2:11
  |
2 | print(x.y)
  |           ^ attempt to index nil with 'y'
```

- **Inputs:** the source text, the chunk name we set, the error kind
  (runtime/syntax), the message, and the (filtered) traceback.
- **Location parsing is controllable, not fragile:** because **we** set the chunk
  name (`@<path>`), the renderer strips that exact known prefix from the error
  line and then parses the trailing `:line[:col]:`. It never blind-splits on `:`
  (so paths containing `:` are safe).
- **Column may be absent:** Luau runtime errors do not always carry a column
  (syntax errors do). When a column is present, the caret `^` points at it; when
  absent, the snippet shows the line with a line-level marker (caret under the
  first non-whitespace column, or a bare `^`), and the `-->` line omits the
  column. Degrades gracefully, never panics.
- **Out-of-range / unparsable location:** if the line cannot be parsed or is out
  of range for the source (e.g. an error with no position, or a frame from a
  chunk we didn't name), the renderer falls back to the Component-2 plain
  `lur: <message>` form rather than guessing. Fail safe, never panic.
- **Internal struct:** the parsed `{ path, line, col, severity, message }` is held
  in a small struct so a future JSON mode can reuse it (no JSON now).

The renderer is the presentation path for one-shot script errors and for the
server's handler/cron error logging (which have the app source available).

## Component 4 — Per-capability argument messages (last)

Capability functions currently rely on mlua's automatic `FromLua` conversion for
argument typing, so a wrong-typed argument yields mlua's generic phrasing
(`bad argument #1 ... error converting Lua table to string`). This component
gives each `lur.*` function a lur-voiced argument error:

```
lur.crypto.sha256: argument #1 must be a string (bytes), got table
```

- A shared helper (e.g. in a small `capabilities::args` module) extracts and
  validates each argument with a uniform message format
  `lur.<cap>.<fn>: argument #<n> must be <expected>, got <actual>`.
- Each capability function is migrated to it, function by function.
- This is the broadest, most mechanical change (it touches every `lur.*`
  function) and the lowest-coupling one, so it is implemented **last** and **may
  be carved into its own implementation plan**. The earlier components do not
  depend on it.

## Testing

- **Chunk naming:** a script with a runtime error (named `Runtime`) → stderr
  contains `app.lua:<line>:` and **does not** contain `src/runtime.rs`. Syntax
  error likewise. A nameless `Runtime` → `script:` not a Rust path.
- **Host formatting:** traceback output contains no `[C]: in ?` line but retains
  real frames; every error kind is `lur:`-framed.
- **Renderer:** golden-style assertions on the rendered block for a runtime error
  (caret present when a column exists) and a syntax error; the no-column
  fallback; the unparsable-location fallback to the plain form (no panic).
- **Server:** a handler that errors → the logged line carries `app.lua:<line>:`.
- **Per-capability:** representative functions (e.g. one each from `crypto`,
  `cookie`, `time`) raise the `argument #<n> must be <type>` message on a
  wrong-typed argument; existing capability tests stay green.

## Documentation

- README: a short "Diagnostics" note under the server/CLI docs — errors are
  reported against the script path with a source snippet; tracebacks are shown.
- ARCHITECTURE: note the chunk-naming invariant (every `lua.load` is named from
  the CLI path; nameless callers use `script`) and the `src/diagnostics.rs`
  renderer in the error-handling flow.

## Out of scope (possible follow-ups)

- `--error-format=json` (or SARIF) machine-readable diagnostics.
- Source-map-style remapping (not applicable — lur runs the source directly).
- Per-kind exit codes.
