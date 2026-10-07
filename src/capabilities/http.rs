//! `lur.http` — policy-gated async HTTP client. Every request and
//! redirect hop is checked against the allowlist and private-network deny.
//! Bodies are raw bytes, not auto-decompressed. `opts.cache` keeps 2xx GET
//! responses in a process-wide in-memory [`HttpCache`] shared by the pool.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use mlua::{Error, Lua, Table, Value};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Client, Method, Url, redirect};
use sha2::{Digest, Sha256};

use super::json;
use super::kv::ttl_ms_opt;
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
}

#[derive(Default)]
struct CacheInner {
    entries: HashMap<String, CacheEntry>,
    last_sweep: Option<Instant>,
}

/// Process-wide response cache behind `opts.cache`, shared by every pooled VM
/// (like `lur.state`). Deliberately unbounded in size: memory is managed by the
/// script through `ttl_ms`, what it chooses to cache, `--max-http-body` (caps
/// each entry) and [`HttpCache::clear`]. Expired entries are dropped on lookup
/// and swept at most once a minute on insert, so a TTL does release memory.
#[derive(Default)]
pub struct HttpCache {
    inner: Mutex<CacheInner>,
}

impl std::fmt::Debug for HttpCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpCache").finish_non_exhaustive()
    }
}

const SWEEP_EVERY: Duration = Duration::from_secs(60);

impl HttpCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, CacheInner> {
        self.inner.lock().expect("http cache mutex poisoned")
    }

    fn get(&self, key: &str) -> Option<Arc<RawResponse>> {
        let mut inner = self.lock();
        match inner.entries.get(key) {
            Some(e) if e.expires > Instant::now() => Some(Arc::clone(&e.raw)),
            Some(_) => {
                inner.entries.remove(key);
                None
            }
            None => None,
        }
    }

    fn put(&self, key: String, raw: RawResponse, ttl_ms: i64) {
        let now = Instant::now();
        let mut inner = self.lock();
        if inner
            .last_sweep
            .is_none_or(|t| now.duration_since(t) >= SWEEP_EVERY)
        {
            inner.entries.retain(|_, e| e.expires > now);
            inner.last_sweep = Some(now);
        }
        // `ttl_ms` is validated positive; an absurd one must not overflow `Instant`.
        let expires = now
            .checked_add(Duration::from_millis(ttl_ms.cast_unsigned()))
            .unwrap_or_else(|| now + Duration::from_secs(10 * 365 * 24 * 3600));
        inner.entries.insert(
            key,
            CacheEntry {
                expires,
                raw: Arc::new(raw),
            },
        );
    }

    /// Drops every entry; returns how many were held.
    fn clear(&self) -> usize {
        let mut inner = self.lock();
        let n = inner.entries.len();
        inner.entries.clear();
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
    match (body, json_val) {
        (Some(_), Some(_)) => {
            return Err(Error::runtime(
                "lur.http: opts.body and opts.json are mutually exclusive",
            ));
        }
        (Some(b), None) => {
            req = req.body(b.as_bytes().to_vec());
        }
        (None, Some(v)) => {
            let json = json::lua_to_json(&v)?;
            let bytes = serde_json::to_vec(&json)
                .map_err(|e| Error::runtime(format!("lur.http: encoding opts.json: {e}")))?;
            req = req.header("content-type", "application/json").body(bytes);
        }
        (None, None) => {}
    }

    if let Some(ms) = opts.get::<Option<u64>>("timeout")? {
        req = req.timeout(Duration::from_millis(ms));
    }
    Ok(req)
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
    let json_fn = lua.create_function(move |lua, ()| {
        let parsed: serde_json::Value = serde_json::from_slice(&body_for_json.as_bytes())
            .map_err(|e| Error::runtime(format!("res.json: {e}")))?;
        json::json_to_lua(lua, &parsed)
    })?;
    res.set("json", json_fn)?;

    Ok(res)
}
