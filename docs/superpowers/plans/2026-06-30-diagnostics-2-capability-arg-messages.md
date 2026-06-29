# Diagnostics (2/2): per-capability argument messages — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace mlua's generic argument-type errors with lur-voiced messages of the form `lur.<cap>.<fn>: argument #<n> must be <type>, got <type>`, without changing any runtime behavior (coercion preserved).

**Architecture:** A shared `argcheck::arg::<T>(lua, value, fname, n, expected)` helper performs the same `FromLua` conversion mlua already does (so number→string coercion etc. is preserved) but, on failure, raises a lur-voiced message naming the function, argument position, expected type, and actual type. Each scalar-argument capability function is migrated from a typed closure arg (`|lua, s: mlua::String|`) to a raw `Value` arg plus an `argcheck::arg` call.

**Tech Stack:** Rust (edition 2024), mlua 0.11 (Luau). No new dependencies.

This is plan 2 of 2 for the diagnostics spec (`docs/superpowers/specs/2026-06-30-diagnostics-design.md`), implementing spec **component 4**. Plans 1's components 1–3 are already merged (#49).

## Global Constraints

- **Approach A — preserve coercion, message only.** `argcheck::arg` calls `T::from_lua` (identical coercion to today); it only customizes the error message on conversion failure. NO behavior change — any script that runs today still runs.
- **Message format (exact):** `lur.<cap>.<fn>: argument #<n> must be <expected>, got <actual>` where `<n>` is 1-based, `<expected>` is the human type word given at the call site (`"string"`, `"number"`, `"function"`), and `<actual>` is `Value::type_name()` of the supplied value (`"table"`, `"boolean"`, `"nil"`, …).
- **Scope = scalar-argument capability functions only.** Migrate the functions in the table below (`crypto`, `base64`, `cookie`, `time`, `json`, `io`, `fs`, `env`, `log`, `state`). Do **NOT** touch `http`, `serve`, `db`, `async` — they take tables/closures and already validate manually; forcing them through the helper adds risk without benefit. Arguments that are intentionally "any value" (e.g. `lur.json.encode(value)`, `lur.state.set`'s value) keep their existing `Value` binding and are NOT checked.
- No new dependencies.
- CI gates: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run`. Run `cargo fmt --all` before each commit.
- Commits GPG-signed (`git commit -S`). Stage files explicitly — never `git add -A`/`.`.

## Migration table (the complete scope)

| Function (set name)        | `fname` string                | Args to check (pos: expected) |
|----------------------------|-------------------------------|-------------------------------|
| crypto sha256/512/sha1/md5 | `lur.crypto.<name>`           | 1: string                     |
| crypto hmac_sha256/512/sha1| `lur.crypto.<name>`           | 1: string, 2: string          |
| crypto constant_eq         | `lur.crypto.constant_eq`      | 1: string, 2: string          |
| crypto random_bytes        | `lur.crypto.random_bytes`     | 1: number                     |
| crypto hex.encode          | `lur.crypto.hex.encode`       | 1: string                     |
| crypto hex.decode          | `lur.crypto.hex.decode`       | 1: string                     |
| base64 encode / decode     | `lur.base64.encode`/`.decode` | 1: string                     |
| cookie parse               | `lur.cookie.parse`            | 1: string                     |
| cookie serialize           | `lur.cookie.serialize`        | 1: string, 2: string (opts kept `Option<Table>`) |
| time parse_rfc3339         | `lur.time.parse_rfc3339`      | 1: string                     |
| time parse_http_date       | `lur.time.parse_http_date`    | 1: string                     |
| json decode                | `lur.json.decode`             | 1: string (encode kept `Value`) |
| io stdin.read              | `lur.stdin.read`              | 1: number (optional)          |
| io stdout.write            | `lur.stdout.write`            | 1: string                     |
| fs read                    | `lur.fs.read`                 | 1: string                     |
| fs write                   | `lur.fs.write`                | 1: string, 2: string          |
| env (callable)             | `lur.env`                     | 1: string                     |
| log info/warn/error        | `lur.log.<level>`             | 1: string                     |
| state get                  | `lur.state.get`               | 1: string                     |
| state set                  | `lur.state.set`               | 1: string (value kept `Value`)|
| state incr                 | `lur.state.incr`              | 1: string, 2: number (optional)|
| state update               | `lur.state.update`            | 1: string, 2: function        |

**The mechanical transform (applies to every row):**
1. Change the closure's typed arg(s) to raw `Value` (or a tuple of `Value`), keeping `Option<Value>` shape only where the original was optional — actually take `Value` for optional too and convert to `Option<T>` (Nil → None, no error). If the closure header is `|_, …|`, change `_` to `lua` (the helper needs `&Lua`).
2. At the top of the body, for each checked arg, insert:
   `let <name>: <T> = crate::capabilities::argcheck::arg(lua, <name>, "<fname>", <n>, "<expected>")?;`
   where `<T>` is the original type (`mlua::String`, `i64`, `usize`, `Option<usize>`, `Option<f64>`, `mlua::Function`).
3. Leave the rest of the body unchanged.

---

## File Structure

- **Create** `src/capabilities/argcheck.rs` — the helper.
- **Modify** `src/capabilities/mod.rs` — `pub(crate) mod argcheck;`.
- **Modify** `src/capabilities/{crypto,base64,cookie,time,json,io,fs,env,log,state}.rs` — migrate per the table.
- **Modify** `tests/capabilities.rs` — message assertions (added in the migration tasks).
- **Modify** `README.md` / `ARCHITECTURE.md` — short note (Task 5).

---

### Task 1: The `argcheck` helper

**Files:**
- Create: `src/capabilities/argcheck.rs`
- Modify: `src/capabilities/mod.rs`
- Test: inline `#[cfg(test)]` in `src/capabilities/argcheck.rs`

**Interfaces:**
- Produces: `pub(crate) fn arg<T: mlua::FromLua>(lua: &Lua, value: Value, fname: &str, n: usize, expected: &str) -> mlua::Result<T>`. Consumed by every migration task.

- [ ] **Step 1: Write the failing unit test**

Create `src/capabilities/argcheck.rs`:

```rust
//! Shared argument extraction that preserves mlua's coercions but raises a
//! lur-voiced message on a type mismatch:
//! `lur.<cap>.<fn>: argument #<n> must be <expected>, got <actual>`.

use mlua::{Error, FromLua, Lua, Value};

/// Convert `value` (the `n`-th argument to `fname`) to `T`. Coercion is
/// identical to mlua's default (e.g. a number is still accepted where a string
/// is wanted); only the failure message is customized.
pub(crate) fn arg<T: FromLua>(
    lua: &Lua,
    value: Value,
    fname: &str,
    n: usize,
    expected: &str,
) -> mlua::Result<T> {
    let got = value.type_name();
    T::from_lua(value, lua)
        .map_err(|_| Error::runtime(format!("{fname}: argument #{n} must be {expected}, got {got}")))
}

#[cfg(test)]
mod tests {
    use super::arg;
    use mlua::{Lua, Value};

    #[test]
    fn good_value_converts() {
        let lua = Lua::new();
        let s: mlua::String = arg(&lua, Value::Integer(5), "lur.x.y", 1, "string").unwrap();
        // mlua coerces a number to a string — behavior preserved.
        assert_eq!(s.to_str().unwrap(), "5");
    }

    #[test]
    fn wrong_type_raises_lur_voiced_message() {
        let lua = Lua::new();
        let tbl = Value::Table(lua.create_table().unwrap());
        let err = arg::<mlua::String>(&lua, tbl, "lur.crypto.sha256", 1, "string").unwrap_err();
        assert_eq!(
            err.to_string(),
            "runtime error: lur.crypto.sha256: argument #1 must be string, got table"
        );
    }

    #[test]
    fn nil_to_optional_is_none_not_error() {
        let lua = Lua::new();
        let got: Option<i64> = arg(&lua, Value::Nil, "lur.x.y", 1, "number").unwrap();
        assert_eq!(got, None);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -E 'test(argcheck)'`
Expected: FAIL — module not declared yet (compile error).

- [ ] **Step 3: Declare the module**

In `src/capabilities/mod.rs`, with the other `pub mod` lines, add:

```rust
pub(crate) mod argcheck;
```

- [ ] **Step 4: Run the tests (expect PASS) + lint**

Run: `cargo fmt --all && cargo nextest run -E 'test(argcheck)' && cargo clippy --all-targets -- -D warnings`
Expected: the 3 `argcheck` tests PASS; clippy clean.
Note: if the `Error::runtime` Display prefix differs from `"runtime error: "`, adjust the test's expected string to match mlua's actual `Display` (the message body after the prefix is what matters); let the test drive the exact prefix.

- [ ] **Step 5: Commit**

```bash
git add src/capabilities/argcheck.rs src/capabilities/mod.rs
git commit -S -m "feat(diagnostics): add argcheck helper for lur-voiced arg errors"
```

---

### Task 2: Migrate `crypto`

**Files:**
- Modify: `src/capabilities/crypto.rs`
- Test: `tests/capabilities.rs`

**Interfaces:**
- Consumes: `crate::capabilities::argcheck::arg` (Task 1).

- [ ] **Step 1: Write the failing message test**

Append to `tests/capabilities.rs`:

```rust
#[test]
fn crypto_arg_type_error_is_lur_voiced() {
    run("local ok, err = pcall(function() return lur.crypto.sha256({}) end)\n\
         assert(ok == false, 'table arg rejected')\n\
         assert(err:find('lur.crypto.sha256: argument #1 must be string, got table', 1, true),\n\
           'lur-voiced message: ' .. tostring(err))\n\
         local ok2, err2 = pcall(function() return lur.crypto.hmac_sha256('k', {}) end)\n\
         assert(ok2 == false and err2:find('lur.crypto.hmac_sha256: argument #2 must be string, got table', 1, true),\n\
           'second-arg message: ' .. tostring(err2))");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -E 'test(crypto_arg_type_error)'`
Expected: FAIL — today's message is mlua's generic `bad argument` wording, not the lur-voiced one.

- [ ] **Step 3: Thread `fname` through the generic `hash_fn` and migrate the closures**

In `src/capabilities/crypto.rs`, add at the top with the other `use`s:

```rust
use crate::capabilities::argcheck;
use mlua::Value;
```

Change `hash_fn` to accept the function name and check its argument:

```rust
/// One hashing function: raw bytes in, raw digest bytes out.
fn hash_fn<D: Digest>(lua: &Lua, fname: &'static str) -> Result<mlua::Function, RunError> {
    lua.create_function(move |lua, data: Value| {
        let data: mlua::String = argcheck::arg(lua, data, fname, 1, "string")?;
        lua.create_string(D::digest(data.as_bytes()).as_slice())
    })
    .map_err(RunError::Init)
}
```

Update `install_hashes` to pass each name:

```rust
    crypto
        .set("sha256", hash_fn::<Sha256>(lua, "lur.crypto.sha256")?)
        .map_err(RunError::Init)?;
    crypto
        .set("sha512", hash_fn::<Sha512>(lua, "lur.crypto.sha512")?)
        .map_err(RunError::Init)?;
    crypto
        .set("sha1", hash_fn::<Sha1>(lua, "lur.crypto.sha1")?)
        .map_err(RunError::Init)?;
    crypto
        .set("md5", hash_fn::<Md5>(lua, "lur.crypto.md5")?)
        .map_err(RunError::Init)?;
```

For the three HMAC closures, apply the transform. Example (`hmac_sha256`); do the same for `hmac_sha512` and `hmac_sha1` with their own `fname`:

```rust
    let hmac_sha256 = lua
        .create_function(|lua, (key, msg): (Value, Value)| {
            let key: mlua::String = argcheck::arg(lua, key, "lur.crypto.hmac_sha256", 1, "string")?;
            let msg: mlua::String = argcheck::arg(lua, msg, "lur.crypto.hmac_sha256", 2, "string")?;
            let mut mac = Hmac::<Sha256>::new_from_slice(&key.as_bytes())
                .map_err(|e| Error::runtime(format!("lur.crypto.hmac_sha256: {e}")))?;
            mac.update(&msg.as_bytes());
            lua.create_string(mac.finalize().into_bytes().as_slice())
        })
        .map_err(RunError::Init)?;
```

For `constant_eq` (note: change `|_, …|` to `|lua, …|`):

```rust
    let constant_eq = lua
        .create_function(|lua, (a, b): (Value, Value)| {
            let a: mlua::String = argcheck::arg(lua, a, "lur.crypto.constant_eq", 1, "string")?;
            let b: mlua::String = argcheck::arg(lua, b, "lur.crypto.constant_eq", 2, "string")?;
            let a = a.as_bytes();
            let b = b.as_bytes();
            if a.len() != b.len() {
                return Ok(false);
            }
            Ok(bool::from(a.ct_eq(&b)))
        })
        .map_err(RunError::Init)?;
```

For `random_bytes` (keep the existing `n <= 0` / `MAX_RANDOM_BYTES` checks):

```rust
    let random_bytes = lua
        .create_function(|lua, n: Value| {
            let n: i64 = argcheck::arg(lua, n, "lur.crypto.random_bytes", 1, "number")?;
            if n <= 0 {
                return Err(Error::runtime(
                    "lur.crypto.random_bytes: n must be a positive integer",
                ));
            }
            // … unchanged: MAX check, buffer fill, create_string …
        })
        .map_err(RunError::Init)?;
```

For `hex.encode` and `hex.decode`:

```rust
    let encode = lua
        .create_function(|lua, data: Value| {
            let data: mlua::String = argcheck::arg(lua, data, "lur.crypto.hex.encode", 1, "string")?;
            lua.create_string(hex::encode(data.as_bytes()))
        })
        .map_err(RunError::Init)?;
    // …
    let decode = lua
        .create_function(|lua, text: Value| {
            let text: mlua::String = argcheck::arg(lua, text, "lur.crypto.hex.decode", 1, "string")?;
            let bytes = hex::decode(text.as_bytes())
                .map_err(|e| Error::runtime(format!("lur.crypto.hex.decode: {e}")))?;
            lua.create_string(&bytes)
        })
        .map_err(RunError::Init)?;
```

- [ ] **Step 4: Run the crypto tests (expect PASS) + the existing crypto tests + lint**

Run: `cargo fmt --all && cargo nextest run -E 'test(crypto)' && cargo clippy --all-targets -- -D warnings`
Expected: the new `crypto_arg_type_error_is_lur_voiced` PASS; all existing `crypto_*` tests still PASS (coercion preserved); clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/capabilities/crypto.rs tests/capabilities.rs
git commit -S -m "feat(diagnostics): lur-voiced arg errors in lur.crypto"
```

---

### Task 3: Migrate `base64`, `cookie`, `time`, `json`

**Files:**
- Modify: `src/capabilities/{base64,cookie,time,json}.rs`
- Test: `tests/capabilities.rs`

**Interfaces:**
- Consumes: `crate::capabilities::argcheck::arg`.

Apply the mechanical transform to each function in the table for these four files. Each migrated file needs `use crate::capabilities::argcheck;` and `use mlua::Value;` (if `Value` is not already imported — `json.rs` already imports it).

- [ ] **Step 1: Write the failing message tests**

Append to `tests/capabilities.rs`:

```rust
#[test]
fn scalar_capabilities_arg_errors_are_lur_voiced() {
    run("local function msg(f) local ok, e = pcall(f); assert(ok == false); return e end\n\
         assert(msg(function() return lur.base64.encode({}) end)\n\
           :find('lur.base64.encode: argument #1 must be string, got table', 1, true), 'base64')\n\
         assert(msg(function() return lur.cookie.parse({}) end)\n\
           :find('lur.cookie.parse: argument #1 must be string, got table', 1, true), 'cookie')\n\
         assert(msg(function() return lur.cookie.serialize('n', {}) end)\n\
           :find('lur.cookie.serialize: argument #2 must be string, got table', 1, true), 'cookie2')\n\
         assert(msg(function() return lur.time.parse_rfc3339({}) end)\n\
           :find('lur.time.parse_rfc3339: argument #1 must be string, got table', 1, true), 'time')\n\
         assert(msg(function() return lur.json.decode({}) end)\n\
           :find('lur.json.decode: argument #1 must be string, got table', 1, true), 'json')");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -E 'test(scalar_capabilities_arg_errors)'`
Expected: FAIL — generic mlua messages.

- [ ] **Step 3: Migrate the four files**

Apply the transform. Worked example for `base64.rs` `encode` (do `decode` identically, and the `cookie`/`time`/`json` rows per the table):

```rust
    let encode = lua
        .create_function(|lua, data: Value| {
            let data: mlua::String = argcheck::arg(lua, data, "lur.base64.encode", 1, "string")?;
            lua.create_string(STANDARD.encode(data.as_bytes()))
        })
        .map_err(RunError::Init)?;
```

For `cookie.serialize` (a 3-arg function), only args 1 and 2 are checked; keep `opts: Option<Table>` as the third tuple element:

```rust
        .create_function(
            |lua, (name, value, opts): (Value, Value, Option<mlua::Table>)| {
                let name: mlua::String = argcheck::arg(lua, name, "lur.cookie.serialize", 1, "string")?;
                let value: mlua::String = argcheck::arg(lua, value, "lur.cookie.serialize", 2, "string")?;
                // … rest of serialize body unchanged (uses `name`, `value`, `opts`) …
            },
        )
```

For `json.decode` (`json.encode` keeps its `Value` arg, unchanged):

```rust
    let decode = lua
        .create_function(|lua, text: Value| {
            let text: mlua::String = argcheck::arg(lua, text, "lur.json.decode", 1, "string")?;
            // … rest unchanged …
        })
        .map_err(RunError::Init)?;
```

- [ ] **Step 4: Run the affected tests + lint**

Run: `cargo fmt --all && cargo nextest run -E 'test(scalar_capabilities_arg_errors) + test(base64) + test(cookie) + test(time) + test(json)' && cargo clippy --all-targets -- -D warnings`
Expected: the new test PASS; all existing `base64`/`cookie`/`time`/`json` tests still PASS; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/capabilities/base64.rs src/capabilities/cookie.rs src/capabilities/time.rs src/capabilities/json.rs tests/capabilities.rs
git commit -S -m "feat(diagnostics): lur-voiced arg errors in base64/cookie/time/json"
```

---

### Task 4: Migrate `io`, `fs`, `env`, `log`, `state`

**Files:**
- Modify: `src/capabilities/{io,fs,env,log,state}.rs`
- Test: `tests/capabilities.rs`

**Interfaces:**
- Consumes: `crate::capabilities::argcheck::arg`.

- [ ] **Step 1: Write the failing message tests**

Append to `tests/capabilities.rs`:

```rust
#[test]
fn io_fs_env_log_state_arg_errors_are_lur_voiced() {
    run("local function msg(f) local ok, e = pcall(f); assert(ok == false); return e end\n\
         assert(msg(function() return lur.stdout.write({}) end)\n\
           :find('lur.stdout.write: argument #1 must be string, got table', 1, true), 'stdout')\n\
         assert(msg(function() return lur.log.info({}) end)\n\
           :find('lur.log.info: argument #1 must be string, got table', 1, true), 'log')\n\
         assert(msg(function() return lur.state.get({}) end)\n\
           :find('lur.state.get: argument #1 must be string, got table', 1, true), 'state')\n\
         assert(msg(function() return lur.state.update('k', 'notfn') end)\n\
           :find('lur.state.update: argument #2 must be function, got string', 1, true), 'update')");
}
```

(NOTE on `lur.env`/`lur.fs`: `env` returns `nil` for denied/unset and is policy-gated; `fs` is policy-gated. Test the always-available surfaces above; `env`/`fs`/`stdin.read` are still migrated per the table — the implementer may add focused assertions if a non-gated path exists, but the table migration is the requirement, not extra tests for gated capabilities.)

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -E 'test(io_fs_env_log_state_arg_errors)'`
Expected: FAIL — generic messages (and `lur.state.update` with a non-function currently yields mlua's generic conversion error).

- [ ] **Step 3: Migrate the five files**

Apply the transform per the table. Notable rows:

`io.rs` `stdout.write` (string) and `stdin.read` (optional number — keep `Option<usize>`):

```rust
    let write = lua
        .create_function(|lua, data: Value| {
            let data: mlua::String = argcheck::arg(lua, data, "lur.stdout.write", 1, "string")?;
            // … rest unchanged …
        })
        .map_err(RunError::Init)?;

    let read = lua
        .create_function(|lua, n: Value| {
            let n: Option<usize> = argcheck::arg(lua, n, "lur.stdin.read", 1, "number")?;
            // … rest unchanged (uses `n`) …
        })
        .map_err(RunError::Init)?;
```

`log.rs` — thread the per-level name into the closure (build an owned `fname` per level):

```rust
    for level in ["info", "warn", "error"] {
        let fname = format!("lur.log.{level}");
        let f = lua
            .create_function(move |lua, msg: Value| {
                let msg: mlua::String = argcheck::arg(lua, msg, &fname, 1, "string")?;
                let mut err = std::io::stderr().lock();
                let _ = write!(err, "{level}: ");
                let _ = err.write_all(&msg.as_bytes());
                let _ = err.write_all(b"\n");
                Ok(())
            })
            .map_err(RunError::Init)?;
        log.set(level, f).map_err(RunError::Init)?;
    }
```

`state.rs` — `get` (string), `set` (string; value kept `Value`), `incr` (string + optional number), `update` (string + function):

```rust
    // get
    .create_function(move |lua, key: Value| {
        let key: mlua::String = argcheck::arg(lua, key, "lur.state.get", 1, "string")?;
        // … rest unchanged …
    })
    // set
    .create_function(move |lua, (key, value): (Value, Value)| {
        let key: mlua::String = argcheck::arg(lua, key, "lur.state.set", 1, "string")?;
        // value stays as-is (any value allowed); rest unchanged …
    })
    // incr
    .create_function(move |lua, (key, n): (Value, Value)| {
        let key: mlua::String = argcheck::arg(lua, key, "lur.state.incr", 1, "string")?;
        let n: Option<f64> = argcheck::arg(lua, n, "lur.state.incr", 2, "number")?;
        // … rest unchanged …
    })
    // update
    .create_function(move |lua, (key, func): (Value, Value)| {
        let key: mlua::String = argcheck::arg(lua, key, "lur.state.update", 1, "string")?;
        let func: mlua::Function = argcheck::arg(lua, func, "lur.state.update", 2, "function")?;
        // … rest unchanged …
    })
```

`fs.rs` (`read`: string; `write`: two strings — closures are `move`):

```rust
    .create_function(move |lua, path: Value| {
        let path: mlua::String = argcheck::arg(lua, path, "lur.fs.read", 1, "string")?;
        // … rest unchanged …
    })
    // write
    .create_function(move |lua, (path, data): (Value, Value)| {
        let path: mlua::String = argcheck::arg(lua, path, "lur.fs.write", 1, "string")?;
        let data: mlua::String = argcheck::arg(lua, data, "lur.fs.write", 2, "string")?;
        // … rest unchanged …
    })
```

`env.rs` (`move` closure; arg name `name`):

```rust
    .create_function(move |lua, name: Value| {
        let name: mlua::String = argcheck::arg(lua, name, "lur.env", 1, "string")?;
        // … rest unchanged …
    })
```

Each migrated file needs `use crate::capabilities::argcheck;` and `use mlua::Value;` if not already imported.

- [ ] **Step 4: Run the affected tests + FULL suite + lint**

Run: `cargo fmt --all && cargo nextest run && cargo clippy --all-targets -- -D warnings`
Expected: the new test PASS; the full suite green (all existing `io`/`fs`/`env`/`log`/`state` tests still pass — coercion preserved); clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/capabilities/io.rs src/capabilities/fs.rs src/capabilities/env.rs src/capabilities/log.rs src/capabilities/state.rs tests/capabilities.rs
git commit -S -m "feat(diagnostics): lur-voiced arg errors in io/fs/env/log/state"
```

---

### Task 5: Documentation

**Files:**
- Modify: `README.md`, `ARCHITECTURE.md`

**Interfaces:**
- Consumes: the migrated capabilities.
- Produces: docs only.

- [ ] **Step 1: Update the docs**

In `README.md`, in the "Diagnostics" section added by plan 1, append a sentence:

```markdown
Capability functions report argument-type mistakes in their own voice, e.g.
`lur.crypto.sha256: argument #1 must be string, got table`. Type coercion is
unchanged — only the error message is clearer.
```

In `ARCHITECTURE.md`, near the capability notes, add:

```markdown
Scalar-argument capability functions extract their arguments through
`capabilities::argcheck::arg`, which preserves mlua's coercion but raises
`lur.<cap>.<fn>: argument #<n> must be <type>, got <type>` on a type mismatch.
Table/closure-taking capabilities (`http`, `serve`, `db`, `async`) validate
their own arguments and are not routed through it.
```

- [ ] **Step 2: Sanity check + commit**

Run: `cargo fmt --all && cargo nextest run -E 'test(arg)'`
Expected: argument-message tests still pass (docs are prose only).

```bash
git add README.md ARCHITECTURE.md
git commit -S -m "docs: document lur-voiced capability argument errors"
```

---

## Self-Review

**Spec coverage (component 4):**
- Shared helper with the exact message format → Task 1. ✓
- Each scalar-argument capability migrated → Tasks 2–4 (table is the complete scope). ✓
- Coercion preserved (approach A, non-breaking) → `argcheck::arg` calls `T::from_lua`; existing capability tests must stay green (asserted in each task's Step 4). ✓
- `http`/`serve`/`db`/`async` excluded; "any value" args (`json.encode`, `state.set` value) not checked → Global Constraints + table. ✓
- Representative message tests (crypto, cookie, time, plus io/log/state) → Tasks 2–4. ✓
- Docs → Task 5. ✓

**Placeholder scan:** The migration uses a stated mechanical transform + a complete per-function table + worked code for every distinct shape (string, two-string, optional number, function, generic `hash_fn`, per-level `log`). The `// … rest unchanged …` markers denote deliberately-untouched existing code, not omitted new code. No TBD/TODO.

**Type consistency:** `argcheck::arg<T: FromLua>(lua, value, fname, n, expected) -> mlua::Result<T>` is defined in Task 1 and called identically everywhere; converted types match each function's original binding (`mlua::String`, `i64`, `Option<usize>`, `Option<f64>`, `mlua::Function`). ✓

## Execution Handoff

Two execution options:

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks.
2. **Inline Execution** — execute in this session with checkpoints.
