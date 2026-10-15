//! `lur.http` — policy-gated async HTTP client. Every request and
//! redirect hop is checked against the allowlist and private-network deny.
//! Bodies are raw bytes, not auto-decompressed. `opts.cache` keeps 2xx GET
//! responses in a process-wide in-memory [`HttpCache`] shared by the pool.
//!
//! ## Design notes
//!
//! - **`lur.http` SSRF guard**:
//!   - The private-address list is curated (loopback, private, link-local, unique-local,
//!     CGNAT, cloud-metadata ranges, multicast, …), not the full IANA special-purpose
//!     registry. IPv6 forms embedding an IPv4 address (mapped, compatible, NAT64, 6to4) are
//!     judged by the embedded address; Teredo `2001::/32` is not unpacked.
//!   - IP literals skip `SsrfResolver` (the connector dials them directly), so `url_allowed`
//!     is their only private-address gate, for the first request and every redirect hop.
//!     Hostnames are filtered by the resolver, which also pins the connection to the vetted IP.
//!   - System proxy env vars (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`) are ignored on
//!     purpose: through a proxy the name is resolved remotely and bypasses the resolver.
//!   - Allowlist rules match IPs by address, strictly: an IPv4 rule does not match its
//!     IPv4-mapped IPv6 spelling, nor the reverse.
//! - **`res.text()` / `res.html()`** are dot-called closures like `res.json()` (one calling
//!   convention per response object). They share `charset::decode_bytes` with
//!   `lur.charset.decode`, but an unknown `charset=` in the *server's* header falls back to
//!   sniffing/UTF-8 rather than raising (unlike a script-supplied label to `decode`). The raw
//!   `body` is unchanged; the helpers decode on each call, so they work on cache hits too.
//! - **`opts.form`** is flat string/number/boolean only (nested tables raise: there is no
//!   agreed encoding) with keys sorted, because Lua table order is unspecified and tests/
//!   signatures need a stable body. A caller-set `Content-Type` is kept (unlike `json`, which
//!   always adds its own), since some endpoints want a variant such as a charset suffix. Repeated keys
//!   and multipart are not supported.
//! - **`lur.http` `cache` is an in-memory map, not `lur.kv`.** A cache is disposable, so it
//!   shouldn't demand a database: it works in one-shot and under `serve` with no `--db`. It
//!   lives in `RuntimeConfig` (an `Arc` shared by the pool, like `lur.state`), not in a VM.
//!   Costs: lost on restart, not shared between processes, useless across one-shot runs.
//!   Rejected: kv-backed (needs `--db`, which users asked why a cache requires) and "kv when
//!   `--db` is set, memory otherwise" (the same script would have different persistence per
//!   environment). A persistent store can be added later as an explicit opt-in, not an
//!   environment-dependent fallback.
//!   `cache` is a table (`{ ttl_ms }`) so the unit is explicit and `vary` has a place. The
//!   allowlist/SSRF check runs before the lookup so a cached body never outlives the policy that
//!   allowed it. Requests with `Authorization`/`Cookie`/`Proxy-Authorization` bypass the cache
//!   unless the header is listed in `vary`, and responses that set a cookie aren't stored: the
//!   cache is shared by every request, so a credentialed response must not be served to someone
//!   else.
//!   **Size is bounded by operator config, not a Lua API.** The first version had no cap (the
//!   script owned capacity), but a public service caching attacker-influenced URLs with a long
//!   `ttl_ms` could grow the process without limit, and the VM `--memory` limit does not cover
//!   Rust-side buffers. So `--http-cache-max-entries` (default 1024) and
//!   `--http-cache-max-bytes` (default `64m`) cap the cache; `0` = unlimited. An insert over a
//!   cap drops expired entries first (free), then evicts least recently used. A single response
//!   larger than the byte cap is not cached rather than flushing everything for one entry.
//!   Sizes are approximate (body + headers + key, not allocator overhead). Worst case is the
//!   entry cap times `--max-http-body`, further bounded by the byte cap. LRU is a `BTreeMap`
//!   recency index (no new dependency). Not done: per-call limits (`opts.cache` stays
//!   `{ ttl_ms, vary }`; capacity is the operator's decision), and the flags are CLI-only like
//!   `--max-http-body` because the config file holds only policy grants.
//!   Not done: serve-stale-on-error, stampede protection (N concurrent misses fetch N times),
//!   honoring `Cache-Control`/`ETag`, per-key invalidation.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use mlua::{Error, Lua, Table, Value};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Client, Method, Url, redirect};
use sha2::{Digest, Sha256};

use super::kv::ttl_ms_opt;
use super::{charset, html, json};
use crate::policy::Policy;
use crate::runtime::RunError;

const MAX_REDIRECTS: usize = 10;

/// The client is built on first use: its rustls setup dominates VM cold start.
pub(crate) fn install(
    lua: &Lua,
    lur: &Table,
    policy: Arc<Policy>,
    max_body: usize,
    cache: Arc<HttpCache>,
) -> Result<(), RunError> {
    let cell: Arc<OnceLock<Client>> = Arc::new(OnceLock::new());
    let http = lua.create_table().map_err(RunError::Init)?;

    {
        let cache = Arc::clone(&cache);
        let clear = lua
            .create_function(move |_, ()| Ok(cache.clear()))
            .map_err(RunError::Init)?;
        http.set("cache_clear", clear).map_err(RunError::Init)?;
    }

    {
        let cell = Arc::clone(&cell);
        let policy = Arc::clone(&policy);
        let cache = Arc::clone(&cache);
        let request = lua
            .create_async_function(
                move |lua, (method, url, opts): (String, String, Option<Table>)| {
                    let cell = Arc::clone(&cell);
                    let policy = Arc::clone(&policy);
                    let cache = Arc::clone(&cache);
                    async move {
                        let client = ensure_client(&cell, &policy)?;
                        let ctx = Ctx {
                            client: &client,
                            policy: &policy,
                            cache: &cache,
                            max_body,
                        };
                        do_request(&lua, &ctx, &method, &url, opts).await
                    }
                },
            )
            .map_err(RunError::Init)?;
        http.set("request", request).map_err(RunError::Init)?;
    }

    for (name, method) in [
        ("get", "GET"),
        ("post", "POST"),
        ("put", "PUT"),
        ("patch", "PATCH"),
        ("delete", "DELETE"),
        ("head", "HEAD"),
    ] {
        let cell = Arc::clone(&cell);
        let policy = Arc::clone(&policy);
        let cache = Arc::clone(&cache);
        let method = method.to_string();
        let f = lua
            .create_async_function(move |lua, (url, opts): (String, Option<Table>)| {
                let cell = Arc::clone(&cell);
                let policy = Arc::clone(&policy);
                let cache = Arc::clone(&cache);
                let method = method.clone();
                async move {
                    let client = ensure_client(&cell, &policy)?;
                    let ctx = Ctx {
                        client: &client,
                        policy: &policy,
                        cache: &cache,
                        max_body,
                    };
                    do_request(&lua, &ctx, &method, &url, opts).await
                }
            })
            .map_err(RunError::Init)?;
        http.set(name, f).map_err(RunError::Init)?;
    }

    lur.set("http", http).map_err(RunError::Init)?;
    Ok(())
}

fn ensure_client(cell: &OnceLock<Client>, policy: &Arc<Policy>) -> mlua::Result<Client> {
    if let Some(client) = cell.get() {
        return Ok(client.clone());
    }
    let client = build_client(policy).map_err(Error::external)?;
    let _ = cell.set(client);
    Ok(cell.get().expect("client just set").clone())
}

/// Redirects re-check each hop; the resolver drops private IPs (SSRF).
///
/// `no_proxy` disables reqwest's `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`
/// pickup: through a proxy, the target host is never resolved locally, so
/// [`SsrfResolver`] would be bypassed and the proxy itself could sit on a
/// private address. Proxy support is intentionally not implemented.
fn build_client(policy: &Arc<Policy>) -> reqwest::Result<Client> {
    let redirect_policy = {
        let policy = Arc::clone(policy);
        redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("lur.http: too many redirects");
            }
            if url_allowed(&policy, attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("lur.http: redirect target not allowed by the policy")
            }
        })
    };
    let resolver = Arc::new(SsrfResolver {
        policy: Arc::clone(policy),
    });
    Client::builder()
        .redirect(redirect_policy)
        .dns_resolver(resolver)
        .no_proxy()
        .build()
}

/// Filters private/loopback IPs out of DNS answers unless allowed, defeating
/// DNS rebinding to internal hosts.
struct SsrfResolver {
    policy: Arc<Policy>,
}

impl Resolve for SsrfResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = Arc::clone(&self.policy);
        Box::pin(async move {
            let host = name.as_str().to_string();
            let resolved = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let allow_private = policy.allows_private_net();
            let kept: Vec<SocketAddr> = resolved
                .filter(|a| allow_private || !Policy::is_private_ip(a.ip()))
                .collect();
            if kept.is_empty() {
                return Err("blocked: host resolves only to private addresses".into());
            }
            Ok(Box::new(kept.into_iter()) as Addrs)
        })
    }
}

/// Allowlist + IP-literal private deny; hostnames are checked by [`SsrfResolver`].
///
/// IP-literal hosts never reach the resolver (the connector dials them
/// directly), so this is the *only* private-address gate for them — both on
/// the initial request and on every redirect hop.
fn url_allowed(policy: &Policy, url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let Some(port) = url.port_or_known_default() else {
        return false;
    };
    if !policy.allows_net(host, port) {
        return false;
    }
    // `host_str` keeps IPv6 brackets (`[::1]`), which `IpAddr` won't parse.
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let ip = bare.parse::<IpAddr>().ok();
    if let Some(ip) = ip
        && !policy.allows_private_net()
        && Policy::is_private_ip(ip)
    {
        return false;
    }
    true
}

/// What one request needs besides its own arguments.
struct Ctx<'a> {
    client: &'a Client,
    policy: &'a Policy,
    cache: &'a HttpCache,
    max_body: usize,
}

/// A hit is shared, not copied: entries are `Arc`s.
struct CacheEntry {
    expires: Instant,
    raw: Arc<RawResponse>,
    /// Approximate resident size, charged against `max_bytes`.
    size: usize,
    /// Recency stamp; the key of this entry in `CacheInner::order`.
    tick: u64,
}

#[derive(Default)]
struct CacheInner {
    entries: HashMap<String, CacheEntry>,
    /// Recency index: `tick` -> key, oldest first. Mirrors `entries` exactly.
    order: BTreeMap<u64, String>,
    next_tick: u64,
    bytes: usize,
    last_sweep: Option<Instant>,
    /// Lower bound on the earliest `expires` of any entry; lets an over-capacity
    /// insert skip the expiry scan when nothing can have expired.
    earliest: Option<Instant>,
}

impl CacheInner {
    fn stamp(&mut self) -> u64 {
        self.next_tick += 1;
        self.next_tick
    }

    fn remove(&mut self, key: &str) {
        if let Some(e) = self.entries.remove(key) {
            self.order.remove(&e.tick);
            self.bytes -= e.size;
        }
    }

    fn drop_expired(&mut self, now: Instant) {
        if self.earliest.is_none_or(|e| e > now) {
            return;
        }
        let dead: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.expires <= now)
            .map(|(k, _)| k.clone())
            .collect();
        for key in dead {
            self.remove(&key);
        }
        self.earliest = self.entries.values().map(|e| e.expires).min();
    }
}

/// Default cap on the number of cached responses.
pub const DEFAULT_HTTP_CACHE_MAX_ENTRIES: usize = 1024;
/// Default cap on the total size of cached responses.
pub const DEFAULT_HTTP_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Process-wide response cache behind `opts.cache`, shared by every pooled VM
/// (like `lur.state`). Bounded by `--http-cache-max-entries` and
/// `--http-cache-max-bytes` (`0` = unlimited): an insert that exceeds either
/// evicts expired entries first, then the least recently used. A single
/// response larger than the byte cap is not stored. `--max-http-body` still caps
/// each body, and [`HttpCache::clear`] drops everything. Expired entries are also
/// dropped on lookup and swept at most once a minute on insert.
pub struct HttpCache {
    inner: Mutex<CacheInner>,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for HttpCache {
    fn default() -> Self {
        Self::with_limits(DEFAULT_HTTP_CACHE_MAX_ENTRIES, DEFAULT_HTTP_CACHE_MAX_BYTES)
    }
}

impl std::fmt::Debug for HttpCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpCache")
            .field("max_entries", &self.max_entries)
            .field("max_bytes", &self.max_bytes)
            .finish_non_exhaustive()
    }
}

const SWEEP_EVERY: Duration = Duration::from_secs(60);

impl HttpCache {
    /// `0` for either limit means unlimited.
    #[must_use]
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            inner: Mutex::default(),
            max_entries,
            max_bytes,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheInner> {
        self.inner.lock().expect("http cache mutex poisoned")
    }

    fn over(&self, inner: &CacheInner) -> bool {
        (self.max_entries != 0 && inner.entries.len() > self.max_entries)
            || (self.max_bytes != 0 && inner.bytes > self.max_bytes)
    }

    fn get(&self, key: &str) -> Option<Arc<RawResponse>> {
        let mut inner = self.lock();
        let now = Instant::now();
        let (live, old_tick) = {
            let e = inner.entries.get(key)?;
            (e.expires > now, e.tick)
        };
        if !live {
            inner.remove(key);
            return None;
        }
        // Touch: most recently used.
        let tick = inner.stamp();
        inner.order.remove(&old_tick);
        inner.order.insert(tick, key.to_string());
        let e = inner.entries.get_mut(key)?;
        e.tick = tick;
        Some(Arc::clone(&e.raw))
    }

    fn put(&self, key: String, raw: RawResponse, ttl_ms: i64) {
        let size = raw.body.len()
            + raw
                .headers
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>()
            + key.len();
        if self.max_bytes != 0 && size > self.max_bytes {
            return;
        }
        let now = Instant::now();
        let mut inner = self.lock();
        if inner
            .last_sweep
            .is_none_or(|t| now.duration_since(t) >= SWEEP_EVERY)
        {
            inner.drop_expired(now);
            inner.last_sweep = Some(now);
        }
        // `ttl_ms` is validated positive; an absurd one must not overflow `Instant`.
        let expires = now
            .checked_add(Duration::from_millis(ttl_ms.cast_unsigned()))
            .unwrap_or_else(|| now + Duration::from_secs(10 * 365 * 24 * 3600));
        inner.remove(&key);
        inner.earliest = Some(inner.earliest.map_or(expires, |e| e.min(expires)));
        let tick = inner.stamp();
        inner.order.insert(tick, key.clone());
        inner.bytes += size;
        inner.entries.insert(
            key,
            CacheEntry {
                expires,
                raw: Arc::new(raw),
                size,
                tick,
            },
        );
        if self.over(&inner) {
            // Expired entries are free to drop; only then evict live ones.
            inner.drop_expired(now);
        }
        // The new entry has the newest tick and fits alone, so this stops before it.
        while self.over(&inner) {
            let Some((_, oldest)) = inner.order.pop_first() else {
                break;
            };
            if let Some(e) = inner.entries.remove(&oldest) {
                inner.bytes -= e.size;
            }
        }
    }

    /// Drops every entry; returns how many were held.
    fn clear(&self) -> usize {
        let mut inner = self.lock();
        let n = inner.entries.len();
        inner.entries.clear();
        inner.order.clear();
        inner.bytes = 0;
        inner.earliest = None;
        n
    }
}

async fn do_request(
    lua: &Lua,
    ctx: &Ctx<'_>,
    method: &str,
    url_str: &str,
    opts: Option<Table>,
) -> mlua::Result<Table> {
    let url = Url::parse(url_str)
        .map_err(|e| Error::runtime(format!("lur.http: invalid url {url_str:?}: {e}")))?;
    // Before any cache lookup: a cached body must never outlive the policy.
    if !url_allowed(ctx.policy, &url) {
        return Err(Error::runtime(format!(
            "lur.http: {url} is not allowed by the policy"
        )));
    }
    let method = Method::from_bytes(method.to_uppercase().as_bytes())
        .map_err(|e| Error::runtime(format!("lur.http: bad method: {e}")))?;

    let cache = match &opts {
        Some(opts) => parse_cache(opts)?,
        None => None,
    };
    if cache.is_some() && method != Method::GET {
        return Err(Error::runtime("lur.http: opts.cache only supports GET"));
    }

    let mut req = ctx.client.request(method, url);
    if let Some(opts) = opts {
        req = apply_opts(req, &opts)?;
    }
    let req = req
        .build()
        .map_err(|e| Error::runtime(format!("lur.http: {e}")))?;

    // `None` also when the request carries credentials the script didn't allow.
    let wants_cache = cache.is_some();
    let slot = cache.and_then(|c| cache_key(&req, &c).map(|key| (key, c.ttl_ms)));
    if let Some((key, _)) = &slot
        && let Some(raw) = ctx.cache.get(key)
    {
        let res = raw_to_table(lua, &raw)?;
        res.set("cached", true)?;
        return Ok(res);
    }

    let resp = ctx
        .client
        .execute(req)
        .await
        .map_err(|e| Error::runtime(format!("lur.http: {e}")))?;
    let raw = read_response(resp, ctx.max_body).await?;
    let res = raw_to_table(lua, &raw)?;
    if wants_cache {
        res.set("cached", false)?;
    }
    if let Some((key, ttl_ms)) = slot
        && cacheable(&raw)
    {
        ctx.cache.put(key, raw, ttl_ms);
    }
    Ok(res)
}

/// `opts.cache = { ttl_ms = …, vary = { "authorization" } }`.
struct CacheOpts {
    ttl_ms: i64,
    /// Credential headers the script has explicitly allowed into the cache.
    vary: Vec<String>,
}

/// Request headers that make a response user-specific.
const CREDENTIAL_HEADERS: [&str; 3] = ["authorization", "cookie", "proxy-authorization"];

fn parse_cache(opts: &Table) -> mlua::Result<Option<CacheOpts>> {
    let cache = match opts.get::<Value>("cache")? {
        Value::Nil | Value::Boolean(false) => return Ok(None),
        Value::Table(t) => t,
        _ => {
            return Err(Error::runtime(
                "lur.http: opts.cache must be a table such as { ttl_ms = 60000 }",
            ));
        }
    };
    let ttl_ms = ttl_ms_opt(&cache, "lur.http: opts.cache")?
        .ok_or_else(|| Error::runtime("lur.http: opts.cache.ttl_ms is required"))?;
    let mut vary = Vec::new();
    if let Some(list) = cache.get::<Option<Table>>("vary")? {
        for name in list.sequence_values::<String>() {
            vary.push(name?.to_ascii_lowercase());
        }
    }
    Ok(Some(CacheOpts { ttl_ms, vary }))
}

/// Digest of method, final URL (query included) and every request header.
/// `None`: the request carries a credential header not listed in `vary`, so
/// it must bypass the cache rather than share an entry across users.
fn cache_key(req: &reqwest::Request, cache: &CacheOpts) -> Option<String> {
    let mut headers: Vec<(&str, &[u8])> = req
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_bytes()))
        .collect();
    if headers
        .iter()
        .any(|(k, _)| CREDENTIAL_HEADERS.contains(k) && !cache.vary.iter().any(|v| v == k))
    {
        return None;
    }
    headers.sort_unstable();
    let mut hash = Sha256::new();
    hash.update(req.method().as_str().as_bytes());
    hash.update(b"\n");
    hash.update(req.url().as_str().as_bytes());
    for (k, v) in headers {
        hash.update(b"\n");
        hash.update(k.as_bytes());
        hash.update(b":");
        hash.update(v);
    }
    Some(format!("lur.http.cache:{}", hex::encode(hash.finalize())))
}

/// Only successful responses, and none that set cookies for one visitor.
fn cacheable(raw: &RawResponse) -> bool {
    (200..300).contains(&raw.status) && !raw.headers.iter().any(|(k, _)| k == "set-cookie")
}

fn apply_opts(
    mut req: reqwest::RequestBuilder,
    opts: &Table,
) -> mlua::Result<reqwest::RequestBuilder> {
    if let Some(headers) = opts.get::<Option<Table>>("headers")? {
        for pair in headers.pairs::<String, mlua::LuaString>() {
            let (k, v) = pair?;
            req = req.header(k.as_str(), v.as_bytes().as_ref());
        }
    }
    if let Some(query) = opts.get::<Option<Table>>("query")? {
        let mut pairs = Vec::new();
        for pair in query.pairs::<String, Value>() {
            let (k, v) = pair?;
            pairs.push((k, value_to_string(&v)?));
        }
        req = req.query(&pairs);
    }

    let body = opts.get::<Option<mlua::LuaString>>("body")?;
    let json_val = opts.get::<Option<Value>>("json")?;
    let form = match opts.get::<Value>("form")? {
        Value::Nil => None,
        Value::Table(t) => Some(t),
        other => {
            return Err(Error::runtime(format!(
                "lur.http: opts.form must be a table, got {}",
                other.type_name()
            )));
        }
    };
    let given = [body.is_some(), json_val.is_some(), form.is_some()];
    if given.iter().filter(|g| **g).count() > 1 {
        return Err(Error::runtime(
            "lur.http: opts.body, opts.json and opts.form are mutually exclusive",
        ));
    }
    if let Some(b) = body {
        req = req.body(b.as_bytes().to_vec());
    } else if let Some(v) = json_val {
        let json = json::lua_to_json(&v)?;
        let bytes = serde_json::to_vec(&json)
            .map_err(|e| Error::runtime(format!("lur.http: encoding opts.json: {e}")))?;
        req = req.header("content-type", "application/json").body(bytes);
    } else if let Some(form) = form {
        let encoded = encode_form(&form)?;
        // Unlike `json`, an explicit Content-Type from the caller is kept.
        if !has_content_type(opts)? {
            req = req.header("content-type", "application/x-www-form-urlencoded");
        }
        req = req.body(encoded.into_bytes());
    }

    if let Some(ms) = opts.get::<Option<u64>>("timeout")? {
        req = req.timeout(Duration::from_millis(ms));
    }
    Ok(req)
}

/// Whether `opts.headers` already names `Content-Type` (any case).
fn has_content_type(opts: &Table) -> mlua::Result<bool> {
    let Some(headers) = opts.get::<Option<Table>>("headers")? else {
        return Ok(false);
    };
    for pair in headers.pairs::<String, Value>() {
        let (k, _) = pair?;
        if k.eq_ignore_ascii_case("content-type") {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `opts.form` → `application/x-www-form-urlencoded`: a flat table of
/// string/number/boolean values, keys sorted so the body is deterministic.
fn encode_form(form: &Table) -> mlua::Result<String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for pair in form.pairs::<Value, Value>() {
        let (k, v) = pair?;
        let Value::String(k) = k else {
            return Err(Error::runtime(format!(
                "lur.http: opts.form keys must be strings, got {}",
                k.type_name()
            )));
        };
        let key = String::from_utf8_lossy(&k.as_bytes()).into_owned();
        let value = match &v {
            Value::Number(n) if !n.is_finite() => {
                return Err(Error::runtime(format!(
                    "lur.http: opts.form.{key} must be a finite number"
                )));
            }
            Value::String(_) | Value::Integer(_) | Value::Number(_) | Value::Boolean(_) => {
                value_to_string(&v)?
            }
            other => {
                return Err(Error::runtime(format!(
                    "lur.http: opts.form.{key} must be a string, number or boolean, got {}",
                    other.type_name()
                )));
            }
        };
        pairs.push((key, value));
    }
    pairs.sort();
    Ok(url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(&pairs)
        .finish())
}

fn value_to_string(v: &Value) -> mlua::Result<String> {
    match v {
        Value::String(s) => Ok(String::from_utf8_lossy(&s.as_bytes()).into_owned()),
        Value::Integer(i) => Ok(i.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Boolean(b) => Ok(b.to_string()),
        other => Err(Error::runtime(format!(
            "lur.http: cannot use a {} value in opts.query",
            other.type_name()
        ))),
    }
}

/// A response as read off the wire, before it becomes a Lua table.
struct RawResponse {
    status: u16,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}

/// Reads the body, capped at `max_body` because the VM memory limit doesn't
/// cover Rust-side buffering.
async fn read_response(mut resp: reqwest::Response, max_body: usize) -> mlua::Result<RawResponse> {
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().to_lowercase(), value.as_bytes().to_vec()))
        .collect();

    if resp.content_length().is_some_and(|n| n as usize > max_body) {
        return Err(Error::runtime(format!(
            "lur.http: response body exceeds the {max_body}-byte limit"
        )));
    }
    // Also bounds chunked responses without a length.
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| Error::runtime(format!("lur.http: reading body: {e}")))?
    {
        if body.len() + chunk.len() > max_body {
            return Err(Error::runtime(format!(
                "lur.http: response body exceeds the {max_body}-byte limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(RawResponse {
        status,
        headers,
        body,
    })
}

fn raw_to_table(lua: &Lua, raw: &RawResponse) -> mlua::Result<Table> {
    let headers = lua.create_table()?;
    let headers_all = lua.create_table()?;
    for (key, value) in &raw.headers {
        let val = lua.create_string(value)?;
        headers.set(key.as_str(), &val)?; // last value wins
        let arr = if let Some(t) = headers_all.get::<Option<Table>>(key.as_str())? {
            t
        } else {
            let t = lua.create_table()?;
            headers_all.set(key.as_str(), &t)?;
            t
        };
        let next = arr.raw_len() + 1;
        arr.raw_set(next as i64, &val)?;
    }
    let body = lua.create_string(&raw.body)?;

    let res = lua.create_table()?;
    res.set("status", raw.status)?;
    res.set("body", &body)?;
    res.set("headers", headers)?;
    res.set("headers_all", headers_all)?;

    let body_for_json = body.clone();
    let json_fn = lua.create_function(move |lua, opts: Value| {
        // Options are checked first so a bad `opts` is reported even for a bad body.
        // `r:json(...)` passes the response table itself as the first argument.
        if let Value::Table(t) = &opts
            && !t.get::<Value>("status")?.is_nil()
            && !t.get::<Value>("body")?.is_nil()
        {
            return Err(Error::runtime("res.json: call with a dot, not a colon"));
        }
        let drop_null = json::parse_decode_opts(lua, opts, "res.json", 1)?;
        let parsed: serde_json::Value = serde_json::from_slice(&body_for_json.as_bytes())
            .map_err(|e| Error::runtime(format!("res.json: {e}")))?;
        json::json_to_lua(lua, &parsed, drop_null)
    })?;
    res.set("json", json_fn)?;

    // Decoded with the Content-Type charset hint (last header wins, like
    // `headers`); same rules as `lur.charset.decode`.
    let content_type = raw
        .headers
        .iter()
        .rev()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| String::from_utf8_lossy(v).into_owned());
    let text_of = move |body: &mlua::LuaString| {
        charset::decode_content_type(&body.as_bytes(), content_type.as_deref())
    };
    let (body_t, text_of_t) = (body.clone(), text_of.clone());
    let text_fn = lua.create_function(move |lua, ()| lua.create_string(text_of_t(&body_t)))?;
    res.set("text", text_fn)?;
    let body_h = body.clone();
    let html_fn = lua.create_function(move |lua, ()| {
        lua.create_userdata(html::parse_document(text_of(&body_h)))
    })?;
    res.set("html", html_fn)?;

    Ok(res)
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    fn raw(n: usize) -> RawResponse {
        RawResponse {
            status: 200,
            headers: Vec::new(),
            body: vec![0; n],
        }
    }

    fn has(c: &HttpCache, k: &str) -> bool {
        c.lock().entries.contains_key(k)
    }

    #[test]
    fn full_of_live_entries_evicts_lru() {
        let c = HttpCache::with_limits(3, 0);
        for k in ["a", "b", "c"] {
            c.put(k.into(), raw(1), 60_000);
        }
        assert!(c.get("a").is_some()); // touch: b is now oldest
        c.put("d".into(), raw(1), 60_000);
        assert!(!has(&c, "b"));
        assert!(has(&c, "a") && has(&c, "c") && has(&c, "d"));
        assert_eq!(c.lock().entries.len(), 3);
        assert_eq!(c.lock().order.len(), 3);
    }

    #[test]
    fn expired_entries_go_before_live_lru() {
        let c = HttpCache::with_limits(3, 0);
        c.put("old".into(), raw(1), 60_000); // oldest, live
        c.put("short".into(), raw(1), 1);
        c.put("live".into(), raw(1), 60_000);
        std::thread::sleep(Duration::from_millis(20));
        c.put("new".into(), raw(1), 60_000);
        // `short` expired and is dropped; the LRU live `old` survives.
        assert!(!has(&c, "short"));
        assert!(has(&c, "old") && has(&c, "live") && has(&c, "new"));
    }

    #[test]
    fn byte_limit_evicts_expired_then_lru() {
        let one = 100 + 1; // body + key "x"
        let c = HttpCache::with_limits(0, one * 2);
        c.put("a".into(), raw(100), 60_000);
        c.put("b".into(), raw(100), 1);
        std::thread::sleep(Duration::from_millis(20));
        c.put("c".into(), raw(100), 60_000);
        assert!(!has(&c, "b"));
        assert!(has(&c, "a") && has(&c, "c"));
        c.put("d".into(), raw(100), 60_000);
        assert!(!has(&c, "a"), "LRU evicted once nothing is expired");
        assert!(has(&c, "c") && has(&c, "d"));
        assert_eq!(c.lock().bytes, one * 2);
    }

    #[test]
    fn expiry_after_bound_was_set_still_found() {
        let c = HttpCache::with_limits(2, 0);
        c.put("a".into(), raw(1), 60_000);
        c.put("short".into(), raw(1), 30);
        // Nothing expired yet: LRU evicts.
        c.put("b".into(), raw(1), 60_000);
        assert!(!has(&c, "a"));
        std::thread::sleep(Duration::from_millis(60));
        c.put("c".into(), raw(1), 60_000);
        assert!(!has(&c, "short"));
        assert!(has(&c, "b") && has(&c, "c"));
    }

    #[test]
    fn clear_resets() {
        let c = HttpCache::with_limits(2, 0);
        c.put("a".into(), raw(1), 1);
        assert_eq!(c.clear(), 1);
        assert!(c.lock().earliest.is_none());
        c.put("b".into(), raw(1), 60_000);
        assert!(has(&c, "b"));
    }

    #[test]
    fn a_full_cache_with_nothing_expiring_does_not_rescan() {
        let c = HttpCache::with_limits(2, 0);
        c.put("a".into(), raw(1), 60_000);
        c.put("b".into(), raw(1), 60_000);
        let before = c.lock().earliest;
        assert!(before.is_some());
        c.put("c".into(), raw(1), 60_000); // evicts LRU, no scan
        assert_eq!(c.lock().earliest, before);
    }

    #[test]
    fn bound_is_recomputed_after_a_scan() {
        let c = HttpCache::with_limits(3, 0);
        c.put("long".into(), raw(1), 400);
        c.put("short".into(), raw(1), 30);
        c.put("live".into(), raw(1), 60_000);
        std::thread::sleep(Duration::from_millis(80));
        c.put("x".into(), raw(1), 60_000); // scan drops `short`
        assert!(!has(&c, "short") && has(&c, "long"));
        std::thread::sleep(Duration::from_millis(450));
        c.put("y".into(), raw(1), 60_000);
        // The recomputed bound finds `long`; LRU `live` must survive.
        assert!(!has(&c, "long"));
        assert!(has(&c, "live") && has(&c, "x") && has(&c, "y"));
    }

    /// Cost of cache-full inserts of live entries (prints; `--run-ignored all --no-capture`).
    #[test]
    #[ignore = "timing probe"]
    fn full_insert_timing() {
        let c = HttpCache::with_limits(DEFAULT_HTTP_CACHE_MAX_ENTRIES, 0);
        for i in 0..DEFAULT_HTTP_CACHE_MAX_ENTRIES {
            c.put(format!("k{i}"), raw(16), 3_600_000);
        }
        let n = 20_000;
        let t = Instant::now();
        for i in 0..n {
            c.put(format!("n{i}"), raw(16), 3_600_000);
        }
        let el = t.elapsed();
        println!("full_insert: {n} puts in {el:?} ({:?}/put)", el / n);
    }
}
