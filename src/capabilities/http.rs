//! `lur.http` — policy-gated async HTTP client (spec §4/§5).
//!
//! Every request, and every redirect hop, is validated against the network
//! allowlist + private-network deny. Bodies are raw bytes in and out (no
//! auto-decompression); UTF-8 is assumed only at the `json` opt and `res.json()`.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use mlua::{Error, Lua, Table, Value};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Client, Method, Url, redirect};

use super::json;
use crate::policy::Policy;
use crate::runtime::RunError;

/// Cap on a redirect chain.
const MAX_REDIRECTS: usize = 10;

/// Install `lur.http.request` and the method sugar. The reqwest client (whose
/// rustls setup dominates VM cold start) is built lazily on the first call, so
/// a script that never touches `lur.http` pays nothing for it.
pub fn install(
    lua: &Lua,
    lur: &Table,
    policy: Arc<Policy>,
    max_body: usize,
) -> Result<(), RunError> {
    let cell: Arc<OnceLock<Client>> = Arc::new(OnceLock::new());
    let http = lua.create_table().map_err(RunError::Init)?;

    {
        let cell = Arc::clone(&cell);
        let policy = Arc::clone(&policy);
        let request = lua
            .create_async_function(
                move |lua, (method, url, opts): (String, String, Option<Table>)| {
                    let cell = Arc::clone(&cell);
                    let policy = Arc::clone(&policy);
                    async move {
                        let client = ensure_client(&cell, &policy)?;
                        do_request(&lua, &client, &policy, &method, &url, opts, max_body).await
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
        let method = method.to_string();
        let f = lua
            .create_async_function(move |lua, (url, opts): (String, Option<Table>)| {
                let cell = Arc::clone(&cell);
                let policy = Arc::clone(&policy);
                let method = method.clone();
                async move {
                    let client = ensure_client(&cell, &policy)?;
                    do_request(&lua, &client, &policy, &method, &url, opts, max_body).await
                }
            })
            .map_err(RunError::Init)?;
        http.set(name, f).map_err(RunError::Init)?;
    }

    lur.set("http", http).map_err(RunError::Init)?;
    Ok(())
}

/// Get the shared client, building it on first use (deferred rustls setup).
fn ensure_client(cell: &OnceLock<Client>, policy: &Arc<Policy>) -> mlua::Result<Client> {
    if let Some(client) = cell.get() {
        return Ok(client.clone());
    }
    let client = build_client(policy).map_err(Error::external)?;
    let _ = cell.set(client);
    Ok(cell.get().expect("client just set").clone())
}

/// Build a reqwest client whose redirect policy re-checks each hop and whose
/// DNS resolver drops private IPs (SSRF deny). TLS verification is always on.
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
        .build()
}

/// A DNS resolver that refuses to hand reqwest any private/loopback IP unless
/// the policy permits it — defeating DNS-rebinding to internal hosts.
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

/// Whether a URL's host:port is permitted (allowlist + IP-literal private deny).
/// Hostnames that resolve to private IPs are caught later by [`SsrfResolver`].
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
    if let Ok(ip) = host.parse::<IpAddr>()
        && !policy.allows_private_net()
        && Policy::is_private_ip(ip)
    {
        return false;
    }
    true
}

async fn do_request(
    lua: &Lua,
    client: &Client,
    policy: &Policy,
    method: &str,
    url_str: &str,
    opts: Option<Table>,
    max_body: usize,
) -> mlua::Result<Table> {
    let url = Url::parse(url_str)
        .map_err(|e| Error::runtime(format!("lur.http: invalid url {url_str:?}: {e}")))?;
    if !url_allowed(policy, &url) {
        return Err(Error::runtime(format!(
            "lur.http: {url} is not allowed by the policy"
        )));
    }
    let method = Method::from_bytes(method.to_uppercase().as_bytes())
        .map_err(|e| Error::runtime(format!("lur.http: bad method: {e}")))?;

    let mut req = client.request(method, url);
    if let Some(opts) = opts {
        req = apply_opts(req, &opts)?;
    }

    let resp = req
        .send()
        .await
        .map_err(|e| Error::runtime(format!("lur.http: {e}")))?;
    build_response(lua, resp, max_body).await
}

/// Apply the `opts` table (headers / query / body | json / timeout) to a request.
fn apply_opts(
    mut req: reqwest::RequestBuilder,
    opts: &Table,
) -> mlua::Result<reqwest::RequestBuilder> {
    if let Some(headers) = opts.get::<Option<Table>>("headers")? {
        for pair in headers.pairs::<String, mlua::String>() {
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

    let body = opts.get::<Option<mlua::String>>("body")?;
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

/// Build the `{ status, body, headers, headers_all, json() }` response table.
/// The body is buffered with a hard cap so an untrusted script can't be served
/// a response large enough to blow past the VM memory limit (which does not
/// cover reqwest's Rust-side allocation).
async fn build_response(
    lua: &Lua,
    mut resp: reqwest::Response,
    max_body: usize,
) -> mlua::Result<Table> {
    let status = resp.status().as_u16();

    let headers = lua.create_table()?;
    let headers_all = lua.create_table()?;
    for (name, value) in resp.headers() {
        let key = name.as_str().to_lowercase();
        let val = lua.create_string(value.as_bytes())?;
        headers.set(key.as_str(), &val)?; // last value wins
        let arr = match headers_all.get::<Option<Table>>(key.as_str())? {
            Some(t) => t,
            None => {
                let t = lua.create_table()?;
                headers_all.set(key.as_str(), &t)?;
                t
            }
        };
        let next = arr.raw_len() + 1;
        arr.raw_set(next as i64, &val)?;
    }

    // Reject early if the advertised length already exceeds the cap.
    if resp.content_length().is_some_and(|n| n as usize > max_body) {
        return Err(Error::runtime(format!(
            "lur.http: response body exceeds the {max_body}-byte limit"
        )));
    }
    // Stream so a chunked response without a length is also bounded.
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| Error::runtime(format!("lur.http: reading body: {e}")))?
    {
        if buf.len() + chunk.len() > max_body {
            return Err(Error::runtime(format!(
                "lur.http: response body exceeds the {max_body}-byte limit"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    let body = lua.create_string(&buf)?;

    let res = lua.create_table()?;
    res.set("status", status)?;
    res.set("body", &body)?;
    res.set("headers", headers)?;
    res.set("headers_all", headers_all)?;

    // res.json() — explicit shorthand; decodes the body only when called.
    let body_for_json = body.clone();
    let json_fn = lua.create_function(move |lua, ()| {
        let parsed: serde_json::Value = serde_json::from_slice(&body_for_json.as_bytes())
            .map_err(|e| Error::runtime(format!("res.json: {e}")))?;
        json::json_to_lua(lua, &parsed)
    })?;
    res.set("json", json_fn)?;

    Ok(res)
}
