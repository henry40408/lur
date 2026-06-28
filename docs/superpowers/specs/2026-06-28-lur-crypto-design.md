# `lur.crypto` — cryptographic primitives

Status: design approved, ready for implementation plan.
Date: 2026-06-28.

## Motivation

The most common piece missing for real-world automation is cryptographic
hashing and message authentication. Verifying an inbound webhook signature
(GitHub, Stripe, Slack), signing an outbound request, and generating
unguessable tokens all require primitives `lur` does not currently expose.
Scripts can base64- and JSON-encode data but cannot hash it, HMAC it, compare
it in constant time, or draw secure random bytes.

`lur.crypto` fills that gap as a pure-compute capability.

## Positioning

- **Pure-compute capability, not policy-gated.** Like `lur.base64` and
  `lur.json`, it touches no host resource that the sandbox arbitrates. Secure
  randomness comes from the OS CSPRNG, which is not a sandbox-relevant side
  effect (no filesystem, network, or env reach). It is therefore available in
  both `strict` and `loose` profiles with no grant.
- **Raw bytes in, raw bytes out.** Consistent with `lur.base64`/`lur.json`'s
  "bytes in/out, let encoders bridge" philosophy. Digests and MACs are returned
  as raw digest bytes; callers pipe them through `lur.crypto.hex` or
  `lur.base64` as the destination format requires. This keeps a single
  canonical representation and a minimal surface.
- **Errors raise.** Failures raise a Lua error of the form
  `lur.crypto.<fn>: <detail>` (catchable with `pcall`), matching the existing
  capability convention.
- **Install order.** Installed in `capabilities::install` immediately after
  `base64` (pure-compute neighbours), before `sandbox(true)`. The capability
  order line in ARCHITECTURE.md is updated to include `crypto`.

## API surface

All byte arguments and return values are Lua strings (`mlua::String`), which in
Luau are arbitrary byte buffers.

### Hashing

Input bytes → raw digest bytes.

```
lur.crypto.sha256(data) → bytes      -- 32-byte digest
lur.crypto.sha512(data) → bytes      -- 64-byte digest
lur.crypto.sha1(data)   → bytes      -- 20-byte digest (legacy compat)
lur.crypto.md5(data)    → bytes      -- 16-byte digest (legacy compat)
```

`sha1` and `md5` are cryptographically broken and provided only for
interoperating with legacy systems (e.g. older webhook signatures). The
algorithm is explicit in the function name; the caller owns the risk.

### HMAC

`(key, msg)` both bytes → raw MAC bytes.

```
lur.crypto.hmac_sha256(key, msg) → bytes
lur.crypto.hmac_sha512(key, msg) → bytes
lur.crypto.hmac_sha1(key, msg)   → bytes      -- legacy compat
```

`hmac_md5` is intentionally omitted — HMAC-MD5 is effectively extinct in
practice. `md5` remains available as a plain hash.

### Hex encoding

The bridge for turning raw digests into the hex strings most signatures are
compared against.

```
lur.crypto.hex.encode(bytes) → string   -- lowercase, no separators
lur.crypto.hex.decode(text)  → bytes     -- accepts upper- or lowercase;
                                         -- odd length or non-hex char → error
```

### Secure randomness

```
lur.crypto.random_bytes(n) → bytes       -- n bytes from the OS CSPRNG
```

`n` must be a positive integer within a sane bound; `n <= 0` or an
excessively large `n` raises. `random_hex` is intentionally omitted —
`hex.encode(random_bytes(n))` composes it, preserving the raw-bytes philosophy.

### Constant-time comparison

```
lur.crypto.constant_eq(a, b) → bool      -- bytes vs bytes
```

Used to compare a computed MAC against an attacker-supplied one without leaking
timing information. Length mismatch returns `false` immediately (the length of a
signature is not secret); when lengths are equal the byte contents are compared
in constant time.

## Worked example — verifying a GitHub-style webhook

```lua
lur.serve.http("POST", "/webhook", function(req)
  local mac = lur.crypto.hmac_sha256(secret, req.body)   -- raw bytes
  local got = lur.crypto.hex.encode(mac)                  -- "a1b2…"
  local want = req.headers["x-hub-signature-256"]         -- "sha256=a1b2…"
  if not want or not lur.crypto.constant_eq(got, want:sub(8)) then
    return { status = 401 }
  end
  -- … process the verified payload …
  return { status = 200 }
end)
```

Signatures delivered as base64 (e.g. Slack, some AWS flows) work the same way
with `lur.base64.encode(mac)` instead of `hex.encode`.

## Dependencies

RustCrypto family and supporting crates:

- `sha2` — SHA-256 / SHA-512
- `sha1` — SHA-1 (legacy)
- `md-5` — MD5 (legacy)
- `hmac` — generic HMAC over the above digests
- `getrandom` — OS CSPRNG for `random_bytes`
- `subtle` — constant-time equality for `constant_eq`
- `hex` — hex encode/decode

Per the dependency cooldown policy, each selected version must be at least 7
days old at implementation time; pin accordingly and run `cargo deny check`.

## Testing

Inline unit tests in `src/capabilities/crypto.rs`:

- Known-answer vectors: empty-string digests for each hash; RFC 4231 HMAC-SHA
  test vectors.
- `hex.encode`/`hex.decode` round-trip; decode rejects odd length and non-hex
  input.
- `constant_eq`: equal inputs → `true`; differing inputs of equal length →
  `false`; unequal lengths → `false`.
- `random_bytes(n)`: returns exactly `n` bytes; two successive calls differ;
  invalid `n` raises.

Integration coverage follows the existing `tests/` style with a script that
runs a full webhook-verification flow end to end.

## Documentation

- README.md "Data & I/O" section gains a `lur.crypto` entry.
- ARCHITECTURE.md capability-order diagram and list include `crypto`.

## Out of scope (possible follow-ups)

- Symmetric encryption (AES-GCM, ChaCha20-Poly1305).
- Asymmetric signing/verification (Ed25519, RSA).
- `hmac_md5`, `random_hex` — deliberately excluded above.
- Key-derivation functions (PBKDF2, Argon2, HKDF).
