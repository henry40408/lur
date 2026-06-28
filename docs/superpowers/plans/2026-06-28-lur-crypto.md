# lur.crypto Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a pure-compute `lur.crypto` capability exposing hashing, HMAC, hex, secure random bytes, and constant-time comparison.

**Architecture:** A new `src/capabilities/crypto.rs` builds a `lur.crypto` sub-table and is installed by `capabilities::install` immediately after `base64`, before `sandbox(true)`. It is not policy-gated (like `lur.base64`/`lur.json`): raw bytes in, raw digest bytes out, with `lur.crypto.hex` as the bridge to the strings signatures are compared against. Behavioural tests live as self-asserting Lua scripts in `tests/capabilities.rs`, matching the existing convention for pure-compute capabilities.

**Tech Stack:** Rust (edition 2024), `mlua` (Luau), RustCrypto crates (`sha2`, `sha1`, `md-5`, `hmac`), plus `getrandom`, `subtle`, and `hex`.

## Global Constraints

- Run with `cargo nextest run` (NOT `cargo test`). MSRV/toolchain are managed separately — do not bump `rust-version`.
- Before every commit: `cargo fmt --all`, then `cargo clippy --all-targets -- -D warnings` must pass clean.
- All commits MUST be GPG-signed (`git commit -S`). Stage files explicitly by name — never `git add -A`/`git add .`.
- Dependency cooldown: every newly-added crate version MUST be at least 7 days old at implementation time. Verify with `cargo info <crate>` before pinning; if the listed version here is younger than 7 days, select the most recent version that is at least 7 days old. Run `cargo deny check` after editing `Cargo.toml`.
- Error convention: failures raise `mlua::Error::runtime("lur.crypto.<fn>: <detail>")`. Construction errors map to `RunError::Init`.
- All byte arguments/returns are `mlua::String` (arbitrary byte buffers in Luau).

---

## File Structure

- **Create:** `src/capabilities/crypto.rs` — the entire `lur.crypto` surface (hex, hashes, HMAC, constant_eq, random_bytes).
- **Modify:** `src/capabilities/mod.rs` — declare `pub mod crypto;` and call `crypto::install` after `base64`.
- **Modify:** `Cargo.toml` — add `sha2`, `sha1`, `md-5`, `hmac`, `getrandom`, `subtle`, `hex`.
- **Modify:** `tests/capabilities.rs` — add the Lua-script behavioural tests.
- **Modify:** `README.md` and `ARCHITECTURE.md` — document the capability and update the capability-order list.

---

### Task 1: Module scaffold + hex encode/decode

**Files:**
- Create: `src/capabilities/crypto.rs`
- Modify: `src/capabilities/mod.rs`
- Modify: `Cargo.toml`
- Test: `tests/capabilities.rs`

**Interfaces:**
- Consumes: `capabilities::install`'s table (`lur`), `RunError::Init`.
- Produces: `crypto::install(lua: &Lua, lur: &Table) -> Result<(), RunError>`; Lua-side `lur.crypto.hex.encode(bytes) -> string` (lowercase) and `lur.crypto.hex.decode(text) -> bytes`.

- [ ] **Step 1: Add the `hex` dependency**

In `Cargo.toml` under `[dependencies]` (keep the list alphabetically ordered), add:

```toml
hex = "0.4.3"
```

Verify the cooldown, then run `cargo deny check` and confirm it passes.

- [ ] **Step 2: Write the failing test**

Add to `tests/capabilities.rs`:

```rust
#[test]
fn crypto_hex_round_trips_and_rejects_bad_input() {
    run("local raw = string.char(0xde, 0xad, 0xbe, 0xef)\n\
         assert(lur.crypto.hex.encode(raw) == 'deadbeef', 'encode is lowercase hex')\n\
         assert(lur.crypto.hex.decode('DEADBEEF') == raw, 'decode accepts uppercase')\n\
         assert(pcall(function() return lur.crypto.hex.decode('abc') end) == false,\n\
         \t'odd length must be rejected')\n\
         assert(pcall(function() return lur.crypto.hex.decode('zz') end) == false,\n\
         \t'non-hex must be rejected')");
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo nextest run -E 'test(crypto_hex)'`
Expected: FAIL — `lur.crypto` is `nil`, so indexing it raises (the script errors).

- [ ] **Step 4: Create the module with hex support**

Create `src/capabilities/crypto.rs`:

```rust
//! `lur.crypto` — hashing, HMAC, secure random, and constant-time compare.
//!
//! Pure-compute capability with no policy gate, in the spirit of `lur.base64`:
//! raw bytes in, raw digest bytes out. `lur.crypto.hex` bridges a raw digest to
//! the lowercase hex string most signatures are compared against.

use mlua::{Error, Lua, Table};

use crate::runtime::RunError;

/// Install the flat `lur.crypto` table.
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let crypto = lua.create_table().map_err(RunError::Init)?;

    install_hex(lua, &crypto)?;

    lur.set("crypto", crypto).map_err(RunError::Init)?;
    Ok(())
}

/// `lur.crypto.hex.encode` / `lur.crypto.hex.decode`.
fn install_hex(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    let hex = lua.create_table().map_err(RunError::Init)?;

    let encode = lua
        .create_function(|lua, data: mlua::String| lua.create_string(hex::encode(data.as_bytes())))
        .map_err(RunError::Init)?;
    hex.set("encode", encode).map_err(RunError::Init)?;

    let decode = lua
        .create_function(|lua, text: mlua::String| {
            let bytes = hex::decode(text.as_bytes())
                .map_err(|e| Error::runtime(format!("lur.crypto.hex.decode: {e}")))?;
            lua.create_string(&bytes)
        })
        .map_err(RunError::Init)?;
    hex.set("decode", decode).map_err(RunError::Init)?;

    crypto.set("hex", hex).map_err(RunError::Init)?;
    Ok(())
}
```

- [ ] **Step 5: Wire the module into `capabilities::install`**

In `src/capabilities/mod.rs`, add the module declaration in alphabetical order among the existing `pub mod` lines:

```rust
pub mod crypto;
```

Then add the install call immediately after the `base64` line inside `install`:

```rust
    base64::install(lua, &lur)?;
    crypto::install(lua, &lur)?;
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo nextest run -E 'test(crypto_hex)'`
Expected: PASS.

- [ ] **Step 7: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
git add Cargo.toml Cargo.lock src/capabilities/crypto.rs src/capabilities/mod.rs tests/capabilities.rs
git commit -S -m "feat(crypto): add lur.crypto module with hex encode/decode"
```

---

### Task 2: Hashing (sha256, sha512, sha1, md5)

**Files:**
- Modify: `src/capabilities/crypto.rs`
- Modify: `Cargo.toml`
- Test: `tests/capabilities.rs`

**Interfaces:**
- Consumes: `lur.crypto.hex.encode` (Task 1) to assert raw digests in tests.
- Produces: `lur.crypto.sha256/sha512/sha1/md5(data) -> bytes` (raw digest bytes).

- [ ] **Step 1: Add the hashing dependencies**

In `Cargo.toml` `[dependencies]` (alphabetical), add:

```toml
md-5 = "0.10.6"
sha1 = "0.10.6"
sha2 = "0.10.9"
```

Verify cooldown for each, then run `cargo deny check`.

- [ ] **Step 2: Write the failing test**

Add to `tests/capabilities.rs`:

```rust
#[test]
fn crypto_hashes_match_known_vectors() {
    run("local hex = lur.crypto.hex.encode\n\
         assert(hex(lur.crypto.sha256('abc')) ==\n\
         \t'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad', 'sha256')\n\
         assert(hex(lur.crypto.sha256('')) ==\n\
         \t'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', 'sha256 empty')\n\
         assert(hex(lur.crypto.sha512('abc')) ==\n\
         \t'ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a' ..\n\
         \t'2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f', 'sha512')\n\
         assert(hex(lur.crypto.sha1('abc')) ==\n\
         \t'a9993e364706816aba3e25717850c26c9cd0d89d', 'sha1')\n\
         assert(hex(lur.crypto.md5('abc')) ==\n\
         \t'900150983cd24fb0d6963f7d28e17f72', 'md5')");
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo nextest run -E 'test(crypto_hashes)'`
Expected: FAIL — `lur.crypto.sha256` is `nil`.

- [ ] **Step 4: Implement the hash functions**

In `src/capabilities/crypto.rs`, add the imports at the top (below the existing `use` lines):

```rust
use digest::Digest;
use md5::Md5;
use sha1::Sha1;
use sha2::{Sha256, Sha512};
```

Note: `Digest` re-exports through any RustCrypto hash crate; importing it from `sha2` also works (`use sha2::Digest;`). If `digest` is not a direct dependency, use `use sha2::Digest;` instead and drop the `digest` import.

Add a generic helper and a new install function, and call it from `install`:

```rust
/// One hashing function: raw bytes in, raw digest bytes out.
fn hash_fn<D: Digest>(lua: &Lua) -> Result<mlua::Function, RunError> {
    lua.create_function(|lua, data: mlua::String| {
        lua.create_string(D::digest(data.as_bytes()).as_slice())
    })
    .map_err(RunError::Init)
}

/// `lur.crypto.sha256` / `sha512` / `sha1` / `md5`.
fn install_hashes(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    crypto
        .set("sha256", hash_fn::<Sha256>(lua)?)
        .map_err(RunError::Init)?;
    crypto
        .set("sha512", hash_fn::<Sha512>(lua)?)
        .map_err(RunError::Init)?;
    crypto
        .set("sha1", hash_fn::<Sha1>(lua)?)
        .map_err(RunError::Init)?;
    crypto
        .set("md5", hash_fn::<Md5>(lua)?)
        .map_err(RunError::Init)?;
    Ok(())
}
```

In `install`, add the call after `install_hex`:

```rust
    install_hex(lua, &crypto)?;
    install_hashes(lua, &crypto)?;
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo nextest run -E 'test(crypto_hashes)'`
Expected: PASS.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
git add Cargo.toml Cargo.lock src/capabilities/crypto.rs tests/capabilities.rs
git commit -S -m "feat(crypto): add sha256/sha512/sha1/md5 hashing"
```

---

### Task 3: HMAC (hmac_sha256, hmac_sha512, hmac_sha1)

**Files:**
- Modify: `src/capabilities/crypto.rs`
- Modify: `Cargo.toml`
- Test: `tests/capabilities.rs`

**Interfaces:**
- Consumes: `lur.crypto.hex.encode` (Task 1); `Sha256`/`Sha512`/`Sha1` imports (Task 2).
- Produces: `lur.crypto.hmac_sha256/hmac_sha512/hmac_sha1(key, msg) -> bytes` (raw MAC bytes).

- [ ] **Step 1: Add the hmac dependency**

In `Cargo.toml` `[dependencies]` (alphabetical), add:

```toml
hmac = "0.12.1"
```

Verify cooldown, then run `cargo deny check`.

- [ ] **Step 2: Write the failing test**

Add to `tests/capabilities.rs` (RFC 4231 / RFC 2202 "Jefe" vectors):

```rust
#[test]
fn crypto_hmac_matches_rfc_vectors() {
    run("local hex = lur.crypto.hex.encode\n\
         local key, msg = 'Jefe', 'what do ya want for nothing?'\n\
         assert(hex(lur.crypto.hmac_sha256(key, msg)) ==\n\
         \t'5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843', 'hmac_sha256')\n\
         assert(hex(lur.crypto.hmac_sha1(key, msg)) ==\n\
         \t'effcdf6ae5eb2fa2d27416d5f184df9c259a7c79', 'hmac_sha1')\n\
         assert(#lur.crypto.hmac_sha512(key, msg) == 64, 'hmac_sha512 is 64 bytes')");
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo nextest run -E 'test(crypto_hmac)'`
Expected: FAIL — `lur.crypto.hmac_sha256` is `nil`.

- [ ] **Step 4: Implement the HMAC functions**

In `src/capabilities/crypto.rs`, add the import:

```rust
use hmac::{Hmac, Mac};
```

Add a generic helper and install function:

```rust
/// One HMAC function: `(key, msg)` bytes in, raw MAC bytes out.
fn hmac_fn<D>(lua: &Lua, name: &'static str) -> Result<mlua::Function, RunError>
where
    D: Digest + digest::core_api::BlockSizeUser + 'static,
{
    lua.create_function(move |lua, (key, msg): (mlua::String, mlua::String)| {
        let key_bytes = key.as_bytes();
        let mut mac = Hmac::<D>::new_from_slice(&key_bytes)
            .map_err(|e| Error::runtime(format!("lur.crypto.{name}: {e}")))?;
        mac.update(&msg.as_bytes());
        lua.create_string(mac.finalize().into_bytes().as_slice())
    })
    .map_err(RunError::Init)
}

/// `lur.crypto.hmac_sha256` / `hmac_sha512` / `hmac_sha1`.
fn install_hmac(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    crypto
        .set("hmac_sha256", hmac_fn::<Sha256>(lua, "hmac_sha256")?)
        .map_err(RunError::Init)?;
    crypto
        .set("hmac_sha512", hmac_fn::<Sha512>(lua, "hmac_sha512")?)
        .map_err(RunError::Init)?;
    crypto
        .set("hmac_sha1", hmac_fn::<Sha1>(lua, "hmac_sha1")?)
        .map_err(RunError::Init)?;
    Ok(())
}
```

Note on bounds: `Hmac<D>` requires `D` to implement the RustCrypto core-API traits. If the `D: Digest + BlockSizeUser` bound does not resolve cleanly, replace the generic helper with three explicit closures (one per algorithm) using `Hmac::<Sha256>::new_from_slice(...)` directly — the per-call body is identical. Prefer the generic form if it compiles.

In `install`, add the call after `install_hashes`:

```rust
    install_hashes(lua, &crypto)?;
    install_hmac(lua, &crypto)?;
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo nextest run -E 'test(crypto_hmac)'`
Expected: PASS.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
git add Cargo.toml Cargo.lock src/capabilities/crypto.rs tests/capabilities.rs
git commit -S -m "feat(crypto): add hmac_sha256/hmac_sha512/hmac_sha1"
```

---

### Task 4: Constant-time comparison

**Files:**
- Modify: `src/capabilities/crypto.rs`
- Modify: `Cargo.toml`
- Test: `tests/capabilities.rs`

**Interfaces:**
- Produces: `lur.crypto.constant_eq(a, b) -> bool`. Returns `false` on length mismatch; constant-time content compare on equal length.

- [ ] **Step 1: Add the subtle dependency**

In `Cargo.toml` `[dependencies]` (alphabetical), add:

```toml
subtle = "2.6.1"
```

Verify cooldown, then run `cargo deny check`.

- [ ] **Step 2: Write the failing test**

Add to `tests/capabilities.rs`:

```rust
#[test]
fn crypto_constant_eq_compares_bytes() {
    run("assert(lur.crypto.constant_eq('abc', 'abc') == true, 'equal')\n\
         assert(lur.crypto.constant_eq('abc', 'abd') == false, 'differ same length')\n\
         assert(lur.crypto.constant_eq('ab', 'abc') == false, 'differ length')\n\
         assert(lur.crypto.constant_eq('', '') == true, 'empty equal')");
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo nextest run -E 'test(crypto_constant_eq)'`
Expected: FAIL — `lur.crypto.constant_eq` is `nil`.

- [ ] **Step 4: Implement constant_eq**

In `src/capabilities/crypto.rs`, add the import:

```rust
use subtle::ConstantTimeEq;
```

Add an install function:

```rust
/// `lur.crypto.constant_eq` — timing-safe byte comparison.
fn install_constant_eq(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    let constant_eq = lua
        .create_function(|_, (a, b): (mlua::String, mlua::String)| {
            let a = a.as_bytes();
            let b = b.as_bytes();
            // Length is not secret; bail before the constant-time content compare.
            if a.len() != b.len() {
                return Ok(false);
            }
            Ok(bool::from(a.ct_eq(&b)))
        })
        .map_err(RunError::Init)?;
    crypto
        .set("constant_eq", constant_eq)
        .map_err(RunError::Init)?;
    Ok(())
}
```

In `install`, add the call after `install_hmac`:

```rust
    install_hmac(lua, &crypto)?;
    install_constant_eq(lua, &crypto)?;
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo nextest run -E 'test(crypto_constant_eq)'`
Expected: PASS.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
git add Cargo.toml Cargo.lock src/capabilities/crypto.rs tests/capabilities.rs
git commit -S -m "feat(crypto): add constant_eq timing-safe comparison"
```

---

### Task 5: Secure random bytes

**Files:**
- Modify: `src/capabilities/crypto.rs`
- Modify: `Cargo.toml`
- Test: `tests/capabilities.rs`

**Interfaces:**
- Produces: `lur.crypto.random_bytes(n) -> bytes`. `n` is a positive integer `<= MAX_RANDOM_BYTES` (1 MiB); otherwise raises.

- [ ] **Step 1: Add the getrandom dependency**

In `Cargo.toml` `[dependencies]` (alphabetical), add:

```toml
getrandom = "0.2.15"
```

Verify cooldown, then run `cargo deny check`. (This plan targets the `0.2` API, `getrandom::getrandom(&mut [u8])`. If you pin `0.3`, use `getrandom::fill` instead.)

- [ ] **Step 2: Write the failing test**

Add to `tests/capabilities.rs`:

```rust
#[test]
fn crypto_random_bytes_length_and_bounds() {
    run("local a = lur.crypto.random_bytes(16)\n\
         assert(#a == 16, 'returns n bytes')\n\
         local b = lur.crypto.random_bytes(16)\n\
         assert(a ~= b, 'two draws differ')\n\
         assert(pcall(function() return lur.crypto.random_bytes(0) end) == false, 'n=0 rejected')\n\
         assert(pcall(function() return lur.crypto.random_bytes(-1) end) == false, 'negative rejected')");
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo nextest run -E 'test(crypto_random_bytes)'`
Expected: FAIL — `lur.crypto.random_bytes` is `nil`.

- [ ] **Step 4: Implement random_bytes**

In `src/capabilities/crypto.rs`, add a module constant near the top (below the `use` lines):

```rust
/// Upper bound on a single `random_bytes` draw (1 MiB) — a guard against a
/// script accidentally requesting an enormous allocation.
const MAX_RANDOM_BYTES: i64 = 1 << 20;
```

Add an install function:

```rust
/// `lur.crypto.random_bytes` — `n` bytes from the OS CSPRNG.
fn install_random(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    let random_bytes = lua
        .create_function(|lua, n: i64| {
            if n <= 0 {
                return Err(Error::runtime(
                    "lur.crypto.random_bytes: n must be a positive integer",
                ));
            }
            if n > MAX_RANDOM_BYTES {
                return Err(Error::runtime(format!(
                    "lur.crypto.random_bytes: n must be <= {MAX_RANDOM_BYTES}"
                )));
            }
            let mut buf = vec![0u8; n as usize];
            getrandom::getrandom(&mut buf)
                .map_err(|e| Error::runtime(format!("lur.crypto.random_bytes: {e}")))?;
            lua.create_string(&buf)
        })
        .map_err(RunError::Init)?;
    crypto
        .set("random_bytes", random_bytes)
        .map_err(RunError::Init)?;
    Ok(())
}
```

In `install`, add the call after `install_constant_eq`:

```rust
    install_constant_eq(lua, &crypto)?;
    install_random(lua, &crypto)?;
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo nextest run -E 'test(crypto_random_bytes)'`
Expected: PASS.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
git add Cargo.toml Cargo.lock src/capabilities/crypto.rs tests/capabilities.rs
git commit -S -m "feat(crypto): add random_bytes from the OS CSPRNG"
```

---

### Task 6: End-to-end webhook test + documentation

**Files:**
- Test: `tests/capabilities.rs`
- Modify: `README.md`
- Modify: `ARCHITECTURE.md`

**Interfaces:**
- Consumes: all of `lur.crypto` from Tasks 1–5.

- [ ] **Step 1: Write the end-to-end verification test**

Add to `tests/capabilities.rs` — a full GitHub-style HMAC-SHA256 signature check:

```rust
#[test]
fn crypto_verifies_a_webhook_signature_end_to_end() {
    run("local secret, body = 'Jefe', 'what do ya want for nothing?'\n\
         local mac = lur.crypto.hmac_sha256(secret, body)\n\
         local got = lur.crypto.hex.encode(mac)\n\
         local want = '5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843'\n\
         assert(lur.crypto.constant_eq(got, want), 'signature must verify')");
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo nextest run -E 'test(crypto_verifies_a_webhook)'`
Expected: PASS (all primitives already exist).

- [ ] **Step 3: Document `lur.crypto` in the README**

In `README.md`, under the **Data & I/O** subsection of the Lua API (after the `lur.base64` bullet), add:

```markdown
- **`lur.crypto`** — pure-compute crypto (no policy needed). Hashing
  `sha256`/`sha512`/`sha1`/`md5(data) → bytes`; HMAC `hmac_sha256`/`hmac_sha512`/
  `hmac_sha1(key, msg) → bytes`; `hex.encode(bytes) → string` / `hex.decode(text)
  → bytes`; `random_bytes(n) → bytes` from the OS CSPRNG; and `constant_eq(a, b)
  → bool` for timing-safe comparison. Digests are raw bytes — bridge to hex or
  `lur.base64` as the destination format needs. `sha1`/`md5` are for legacy
  interop only.
```

- [ ] **Step 4: Update ARCHITECTURE.md capability order**

In `ARCHITECTURE.md`, update the capability-order line in the "Capability layer" section to include `crypto` after `base64`:

Change:

```
null · log · json · base64 · io · fs · http · env · db · async · args · serve · state
```

to:

```
null · log · json · base64 · crypto · io · fs · http · env · db · async · args · serve · state
```

- [ ] **Step 5: Run the full suite and lint gate**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo nextest run
```

Expected: all tests pass, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add tests/capabilities.rs README.md ARCHITECTURE.md
git commit -S -m "docs(crypto): document lur.crypto and add end-to-end webhook test"
```

---

## Self-Review

**Spec coverage:**
- Pure-compute, not policy-gated → Task 1 (no `Arc<Policy>` passed). ✓
- Raw bytes in/out → all functions return raw bytes; hex bridges. ✓
- sha256/sha512/sha1/md5 → Task 2. ✓
- hmac_sha256/512/1, no hmac_md5 → Task 3. ✓
- hex.encode/decode → Task 1. ✓
- random_bytes, no random_hex → Task 5. ✓
- constant_eq → Task 4. ✓
- Install after base64 + ARCHITECTURE order → Task 1 wiring + Task 6 docs. ✓
- RFC 4231 HMAC vectors, empty-string digests, hex round-trip, random length/uniqueness, constant_eq cases → Tasks 1–5 tests. ✓
- README Data & I/O entry → Task 6. ✓
- Dependency cooldown / `cargo deny` → Global Constraints + each dep step. ✓

**Note on test location:** The spec proposed inline unit tests in `crypto.rs`; this plan places behavioural tests in `tests/capabilities.rs` to follow the established convention for pure-compute capabilities (`base64`/`json` are tested there, not inline). This is a faithful refinement, not a scope change.

**Type consistency:** `install` signature `(lua, lur)` matches `base64`. Sub-installers (`install_hex`, `install_hashes`, `install_hmac`, `install_constant_eq`, `install_random`) are named and called consistently. Lua function names (`sha256`, `hmac_sha256`, `hex.encode`, `constant_eq`, `random_bytes`) are identical across tests and implementation.

**Placeholder scan:** None — every step shows complete code or exact commands.
