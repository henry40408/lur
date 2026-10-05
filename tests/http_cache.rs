//! `lur.http` `opts.cache`: responses stored through `lur.kv`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use lur::policy::Policy;
use lur::runtime::{Runtime, RuntimeConfig};

/// Answers every request with `n=<requests so far>`.
struct Server {
    port: u16,
    hits: Arc<AtomicUsize>,
}

impl Server {
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// `extra_headers` is raw header lines, each ending in `\r\n`.
fn serve(status: u16, extra_headers: &'static str) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut s = stream.unwrap();
            let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
            let mut buf = [0u8; 4096];
            let mut seen = Vec::new();
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(k) => seen.extend_from_slice(&buf[..k]),
                }
            }
            let body = format!("n={n}");
            let resp = format!(
                "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n{extra_headers}\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });
    Server { port, hits }
}

fn loopback() -> Policy {
    Policy::strict()
        .with_net(vec!["127.0.0.1".to_string()])
        .allow_private()
}

fn runtime(db: Option<PathBuf>, policy: Policy) -> Runtime {
    Runtime::with_config(RuntimeConfig {
        db_path: db,
        policy: Arc::new(policy),
        ..Default::default()
    })
    .expect("runtime builds")
}

fn cached_runtime(dir: &tempfile::TempDir) -> Runtime {
    runtime(Some(dir.path().join("cache.db")), loopback())
}

#[test]
fn second_call_is_served_from_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let srv = serve(200, "");
    cached_runtime(&dir)
        .run(&format!(
            "local url = 'http://127.0.0.1:{}/'\n\
             local a = lur.http.get(url, {{ cache = {{ ttl_ms = 60000 }} }})\n\
             local b = lur.http.get(url, {{ cache = {{ ttl_ms = 60000 }} }})\n\
             assert(a.body == 'n=1' and a.cached == false, 'first goes out')\n\
             assert(b.body == 'n=1' and b.cached == true, 'second is a hit')\n\
             assert(b.status == 200 and b.headers['content-length'] == '3', 'headers survive')",
            srv.port
        ))
        .expect("cache hit");
    assert_eq!(srv.hits(), 1);
}

#[test]
fn without_the_option_nothing_is_cached() {
    let dir = tempfile::tempdir().unwrap();
    let srv = serve(200, "");
    cached_runtime(&dir)
        .run(&format!(
            "local url = 'http://127.0.0.1:{}/'\n\
             local a = lur.http.get(url)\n\
             local b = lur.http.get(url)\n\
             assert(a.body == 'n=1' and b.body == 'n=2' and a.cached == nil)",
            srv.port
        ))
        .expect("uncached");
    assert_eq!(srv.hits(), 2);
}

#[test]
fn entries_expire() {
    let dir = tempfile::tempdir().unwrap();
    let srv = serve(200, "");
    cached_runtime(&dir)
        .run(&format!(
            "local url = 'http://127.0.0.1:{}/'\n\
             lur.http.get(url, {{ cache = {{ ttl_ms = 150 }} }})\n\
             lur.async.sleep(400)\n\
             local b = lur.http.get(url, {{ cache = {{ ttl_ms = 150 }} }})\n\
             assert(b.body == 'n=2' and b.cached == false, 'refetched')",
            srv.port
        ))
        .expect("expiry");
}

#[test]
fn the_query_is_part_of_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let srv = serve(200, "");
    cached_runtime(&dir)
        .run(&format!(
            "local url = 'http://127.0.0.1:{}/'\n\
             local c = {{ ttl_ms = 60000 }}\n\
             local a = lur.http.get(url, {{ query = {{ p = 1 }}, cache = c }})\n\
             local b = lur.http.get(url, {{ query = {{ p = 2 }}, cache = c }})\n\
             local a2 = lur.http.get(url, {{ query = {{ p = 1 }}, cache = c }})\n\
             assert(a.body == 'n=1' and b.body == 'n=2' and a2.body == 'n=1')",
            srv.port
        ))
        .expect("keys");
    assert_eq!(srv.hits(), 2);
}

#[test]
fn error_responses_are_not_cached() {
    let dir = tempfile::tempdir().unwrap();
    let srv = serve(500, "");
    cached_runtime(&dir)
        .run(&format!(
            "local url = 'http://127.0.0.1:{}/'\n\
             local c = {{ ttl_ms = 60000 }}\n\
             lur.http.get(url, {{ cache = c }})\n\
             local b = lur.http.get(url, {{ cache = c }})\n\
             assert(b.status == 500 and b.cached == false)",
            srv.port
        ))
        .expect("500s");
    assert_eq!(srv.hits(), 2);
}

#[test]
fn responses_that_set_cookies_are_not_cached() {
    let dir = tempfile::tempdir().unwrap();
    let srv = serve(200, "set-cookie: sid=1\r\n");
    cached_runtime(&dir)
        .run(&format!(
            "local url = 'http://127.0.0.1:{}/'\n\
             local c = {{ ttl_ms = 60000 }}\n\
             lur.http.get(url, {{ cache = c }})\n\
             assert(lur.http.get(url, {{ cache = c }}).cached == false)",
            srv.port
        ))
        .expect("cookies");
    assert_eq!(srv.hits(), 2);
}

#[test]
fn credentialed_requests_bypass_the_cache_unless_listed_in_vary() {
    let dir = tempfile::tempdir().unwrap();
    let srv = serve(200, "");
    cached_runtime(&dir)
        .run(&format!(
            "local url = 'http://127.0.0.1:{}/'\n\
             local h = {{ authorization = 'Bearer a' }}\n\
             lur.http.get(url, {{ headers = h, cache = {{ ttl_ms = 60000 }} }})\n\
             local b = lur.http.get(url, {{ headers = h, cache = {{ ttl_ms = 60000 }} }})\n\
             assert(b.cached == false, 'bypassed')",
            srv.port
        ))
        .expect("bypass");
    assert_eq!(srv.hits(), 2);

    let srv = serve(200, "");
    cached_runtime(&dir)
        .run(&format!(
            "local url = 'http://127.0.0.1:{}/'\n\
             local c = {{ ttl_ms = 60000, vary = {{ 'Authorization' }} }}\n\
             lur.http.get(url, {{ headers = {{ authorization = 'Bearer a' }}, cache = c }})\n\
             local same = lur.http.get(url, {{ headers = {{ authorization = 'Bearer a' }}, cache = c }})\n\
             local other = lur.http.get(url, {{ headers = {{ authorization = 'Bearer b' }}, cache = c }})\n\
             assert(same.cached == true, 'same credential hits')\n\
             assert(other.cached == false, 'other credential is a separate entry')",
            srv.port
        ))
        .expect("vary");
    assert_eq!(srv.hits(), 2);
}

#[test]
fn the_policy_is_checked_before_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("shared.db");
    let srv = serve(200, "");
    let script = format!(
        "lur.http.get('http://127.0.0.1:{}/', {{ cache = {{ ttl_ms = 60000 }} }})",
        srv.port
    );
    runtime(Some(db.clone()), loopback())
        .run(&script)
        .expect("allowed run fills the cache");
    let err = runtime(Some(db), Policy::strict())
        .run(&script)
        .expect_err("a stricter policy must not read the cached body");
    assert!(err.to_string().contains("not allowed"), "{err}");
}

#[test]
fn cache_needs_a_database() {
    let srv = serve(200, "");
    let err = runtime(None, loopback())
        .run(&format!(
            "lur.http.get('http://127.0.0.1:{}/', {{ cache = {{ ttl_ms = 1000 }} }})",
            srv.port
        ))
        .expect_err("no --db");
    assert!(err.to_string().contains("--db"), "{err}");
    assert_eq!(srv.hits(), 0, "rejected before any request");
}

#[test]
fn cache_option_is_validated() {
    let dir = tempfile::tempdir().unwrap();
    let srv = serve(200, "");
    let rt = cached_runtime(&dir);
    for (opts, want) in [
        ("cache = 60000", "must be a table"),
        ("cache = {}", "ttl_ms is required"),
        (
            "cache = { ttl_ms = 0 }",
            "ttl_ms must be a positive integer",
        ),
    ] {
        let err = rt
            .run(&format!(
                "lur.http.get('http://127.0.0.1:{}/', {{ {opts} }})",
                srv.port
            ))
            .expect_err(opts);
        assert!(err.to_string().contains(want), "{opts}: {err}");
    }
    let err = rt
        .run(&format!(
            "lur.http.post('http://127.0.0.1:{}/', {{ cache = {{ ttl_ms = 1000 }} }})",
            srv.port
        ))
        .expect_err("post");
    assert!(err.to_string().contains("only supports GET"), "{err}");
    assert_eq!(srv.hits(), 0);
}
