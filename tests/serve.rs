use std::time::Duration;

use lur::runtime::RuntimeConfig;
use lur::serve::{RawRequest, Server};

fn serve(source: &str) -> Server {
    Server::load(source, RuntimeConfig::default()).expect("app loads")
}

/// A single-VM server with a per-event timeout, for the timeout tests.
fn serve_with_timeout(source: &str, ms: u64) -> Server {
    Server::load(
        source,
        RuntimeConfig {
            per_event_timeout: Some(Duration::from_millis(ms)),
            pool_size: 1,
            ..Default::default()
        },
    )
    .expect("app loads")
}

/// A server with a request-body cap, for the 413 tests.
fn serve_with_max_body(source: &str, max_body: usize) -> Server {
    Server::load(
        source,
        RuntimeConfig {
            max_body: Some(max_body),
            ..Default::default()
        },
    )
    .expect("app loads")
}

fn request(method: &str, path: &str, query: &str) -> RawRequest {
    RawRequest {
        method: method.to_owned(),
        path: path.to_owned(),
        query: query.to_owned(),
        ..Default::default()
    }
}

#[test]
fn dispatch_invokes_registered_handler() {
    let s = serve(
        "lur.serve.http('GET', '/hello', function(req)\n\
         \treturn { status = 200, body = 'hi' }\n\
         end)",
    );
    let resp = s.dispatch("GET", "/hello", b"").expect("dispatch ok");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"hi");
}

#[test]
fn unmatched_route_is_404() {
    let s = serve("lur.serve.http('GET', '/hello', function(req) return {} end)");
    let resp = s.dispatch("GET", "/nope", b"").expect("dispatch ok");
    assert_eq!(resp.status, 404);
}

#[test]
fn method_mismatch_is_404() {
    let s = serve("lur.serve.http('GET', '/hello', function(req) return {} end)");
    let resp = s.dispatch("POST", "/hello", b"").expect("dispatch ok");
    assert_eq!(resp.status, 404);
}

#[test]
fn any_method_matches_every_verb() {
    let s = serve("lur.serve.http('ANY', '/hook', function(req) return { body = req.method } end)");
    assert_eq!(s.dispatch("GET", "/hook", b"").unwrap().body, b"GET");
    assert_eq!(s.dispatch("DELETE", "/hook", b"").unwrap().body, b"DELETE");
}

#[test]
fn status_defaults_to_200() {
    let s = serve("lur.serve.http('GET', '/x', function(req) return { body = 'ok' } end)");
    let resp = s.dispatch("GET", "/x", b"").expect("dispatch ok");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"ok");
}

#[test]
fn handler_receives_method_path_and_body() {
    let s = serve(
        "lur.serve.http('POST', '/echo', function(req)\n\
         \treturn { body = req.method .. ' ' .. req.path .. ' ' .. req.body }\n\
         end)",
    );
    let resp = s
        .dispatch("POST", "/echo", b"payload")
        .expect("dispatch ok");
    assert_eq!(resp.body, b"POST /echo payload");
}

#[test]
fn path_param_is_extracted_into_req_params() {
    let s = serve(
        "lur.serve.http('GET', '/users/:id', function(req)\n\
         \treturn { body = req.params.id }\n\
         end)",
    );
    let resp = s.dispatch("GET", "/users/42", b"").expect("dispatch ok");
    assert_eq!(resp.body, b"42");
}

#[test]
fn static_segment_beats_dynamic_param() {
    // Registration order is dynamic-first to prove resolution is order-free.
    let s = serve(
        "lur.serve.http('GET', '/users/:id', function(req) return { body = 'dynamic' } end)\n\
         lur.serve.http('GET', '/users/me', function(req) return { body = 'static' } end)",
    );
    assert_eq!(s.dispatch("GET", "/users/me", b"").unwrap().body, b"static");
    assert_eq!(
        s.dispatch("GET", "/users/42", b"").unwrap().body,
        b"dynamic"
    );
}

#[test]
fn param_is_percent_decoded_to_raw_bytes() {
    let s = serve(
        "lur.serve.http('GET', '/f/:name', function(req) return { body = req.params.name } end)",
    );
    // %2F is '/', %20 is space — decoded after segment splitting.
    let resp = s.dispatch("GET", "/f/a%2Fb%20c", b"").expect("dispatch ok");
    assert_eq!(resp.body, b"a/b c");
}

#[test]
fn duplicate_route_is_a_load_error() {
    let err = Server::load(
        "lur.serve.http('GET', '/u/:id', function() end)\n\
         lur.serve.http('GET', '/u/:name', function() end)",
        RuntimeConfig::default(),
    );
    assert!(err.is_err(), "same (method, pattern) must fail at load");
}

#[test]
fn query_keeps_last_value_and_query_all_keeps_every_value() {
    let s = serve(
        "lur.serve.http('GET', '/s', function(req)\n\
         \treturn { body = req.query.q .. '|' .. req.query_all.q[1] .. ',' .. req.query_all.q[2] }\n\
         end)",
    );
    let resp = s.dispatch_raw(&request("GET", "/s", "q=a&q=b")).unwrap();
    assert_eq!(resp.body, b"b|a,b");
}

#[test]
fn headers_are_lower_cased_and_case_insensitive() {
    let s = serve(
        "lur.serve.http('GET', '/h', function(req) return { body = req.headers['content-type'] } end)",
    );
    let mut req = request("GET", "/h", "");
    req.headers = vec![("Content-Type".to_owned(), "text/plain".to_owned())];
    assert_eq!(s.dispatch_raw(&req).unwrap().body, b"text/plain");
}

#[test]
fn req_json_decodes_the_body() {
    let s = serve(
        "lur.serve.http('POST', '/j', function(req)\n\
         \tlocal d = req.json()\n\
         \treturn { body = d.name }\n\
         end)",
    );
    let mut req = request("POST", "/j", "");
    req.body = br#"{"name":"alice"}"#.to_vec();
    assert_eq!(s.dispatch_raw(&req).unwrap().body, b"alice");
}

#[test]
fn req_read_without_arg_returns_whole_body() {
    let s = serve(
        "lur.serve.http('POST', '/r', function(req)\n\
         \treturn { body = req.read() }\n\
         end)",
    );
    let mut req = request("POST", "/r", "");
    req.body = b"hello world".to_vec();
    assert_eq!(s.dispatch_raw(&req).unwrap().body, b"hello world");
}

#[test]
fn req_read_chunked_advances_and_eofs_to_nil() {
    let s = serve(
        "lur.serve.http('POST', '/r', function(req)\n\
         \tlocal a = req.read(3)\n\
         \tlocal b = req.read(3)\n\
         \tlocal rest = req.read()\n\
         \tlocal eof = req.read(1)\n\
         \treturn { body = a .. '|' .. b .. '|' .. rest .. '|' .. tostring(eof) }\n\
         end)",
    );
    let mut req = request("POST", "/r", "");
    req.body = b"abcdefghij".to_vec();
    assert_eq!(s.dispatch_raw(&req).unwrap().body, b"abc|def|ghij|nil");
}

#[test]
fn req_body_unavailable_after_chunked_read() {
    let s = serve(
        "lur.serve.http('POST', '/r', function(req)\n\
         \treq.read(2)\n\
         \tlocal ok = pcall(function() return req.body end)\n\
         \treturn { body = tostring(ok) }\n\
         end)",
    );
    let mut req = request("POST", "/r", "");
    req.body = b"abcdef".to_vec();
    assert_eq!(s.dispatch_raw(&req).unwrap().body, b"false");
}

#[test]
fn req_json_unavailable_after_chunked_read() {
    let s = serve(
        "lur.serve.http('POST', '/r', function(req)\n\
         \treq.read(2)\n\
         \tlocal ok = pcall(function() return req.json() end)\n\
         \treturn { body = tostring(ok) }\n\
         end)",
    );
    let mut req = request("POST", "/r", "");
    req.body = br#"{"a":1}"#.to_vec();
    assert_eq!(s.dispatch_raw(&req).unwrap().body, b"false");
}

#[test]
fn req_body_still_available_after_whole_read() {
    // read() with no arg is sugar, not a chunked consume — req.body stays usable.
    let s = serve(
        "lur.serve.http('POST', '/r', function(req)\n\
         \tlocal whole = req.read()\n\
         \treturn { body = whole .. '|' .. req.body }\n\
         end)",
    );
    let mut req = request("POST", "/r", "");
    req.body = b"xy".to_vec();
    assert_eq!(s.dispatch_raw(&req).unwrap().body, b"xy|xy");
}

#[test]
fn oversize_body_is_rejected_with_413() {
    let s = serve_with_max_body(
        "lur.serve.http('POST', '/u', function(req) return { body = 'reached' } end)",
        4,
    );
    let mut req = request("POST", "/u", "");
    req.body = b"toolong".to_vec();
    let resp = s.dispatch_raw(&req).unwrap();
    assert_eq!(resp.status, 413);
    assert_ne!(resp.body, b"reached");
}

#[test]
fn body_within_max_is_served() {
    let s = serve_with_max_body(
        "lur.serve.http('POST', '/u', function(req) return { body = req.body } end)",
        16,
    );
    let mut req = request("POST", "/u", "");
    req.body = b"fits".to_vec();
    let resp = s.dispatch_raw(&req).unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"fits");
}

#[test]
fn global_writes_do_not_bleed_across_requests() {
    // A handler that creates a NEW global. sandbox(true) does not stop this, and
    // without per-call isolation the value persists to the next request. Pinned
    // to a single-VM pool so both requests provably hit the same VM.
    let s = Server::load(
        "lur.serve.http('GET', '/c', function(req)\n\
         \tcounter = (counter or 0) + 1\n\
         \treturn { body = tostring(counter) }\n\
         end)",
        RuntimeConfig {
            pool_size: 1,
            ..Default::default()
        },
    )
    .expect("app loads");
    assert_eq!(s.dispatch("GET", "/c", b"").unwrap().body, b"1");
    assert_eq!(
        s.dispatch("GET", "/c", b"").unwrap().body,
        b"1",
        "a global written by one request must not leak into the next"
    );
}

#[test]
fn handlers_cannot_reach_the_global_env_escapes() {
    // getfenv/setfenv/loadstring would each reach the writable global env and
    // bleed state across requests on the same pooled VM; they must be gone.
    let s = serve(
        "lur.serve.http('GET', '/e', function()\n\
         \treturn { body = tostring(getfenv) .. ',' .. tostring(setfenv) .. ',' .. tostring(loadstring) }\n\
         end)",
    );
    assert_eq!(s.dispatch("GET", "/e", b"").unwrap().body, b"nil,nil,nil");
}

#[test]
fn multi_vm_pool_resolves_routes_on_every_vm() {
    // Build a 3-VM pool: every VM collects the same registrations, so the
    // host-assigned handler ids must line up and routing works on each.
    let s = Server::load(
        "lur.serve.http('GET', '/users/:id', function(req) return { body = req.params.id } end)",
        RuntimeConfig {
            pool_size: 3,
            ..Default::default()
        },
    )
    .expect("app loads");
    for _ in 0..6 {
        assert_eq!(s.dispatch("GET", "/users/7", b"").unwrap().body, b"7");
    }
}

#[test]
fn isolated_handler_still_reads_real_globals() {
    // The fresh per-call environment must fall through to the real globals, so
    // capability modules like lur.json stay usable.
    let s = serve(
        "lur.serve.http('GET', '/j', function(req)\n\
         \treturn { body = lur.json.encode({ ok = true }) }\n\
         end)",
    );
    assert_eq!(
        s.dispatch("GET", "/j", b"").unwrap().body,
        br#"{"ok":true}"#
    );
}

#[test]
fn io_parked_handler_exceeding_timeout_returns_503() {
    // Parked on async I/O (sleep) — the tokio wall-clock layer fires.
    let s = serve_with_timeout(
        "lur.serve.http('GET', '/slow', function(req)\n\
         \tlur.async.sleep(5000)\n\
         \treturn { body = 'never' }\n\
         end)",
        50,
    );
    let resp = s
        .dispatch("GET", "/slow", b"")
        .expect("dispatch returns, not error");
    assert_eq!(resp.status, 503);
}

#[test]
fn cpu_bound_handler_exceeding_timeout_returns_503() {
    // A tight loop — the deadline interrupt fires on a back-edge.
    let s = serve_with_timeout(
        "lur.serve.http('GET', '/spin', function(req)\n\
         \twhile true do end\n\
         end)",
        50,
    );
    let resp = s
        .dispatch("GET", "/spin", b"")
        .expect("dispatch returns, not error");
    assert_eq!(resp.status, 503);
}

#[test]
fn vm_recovers_after_a_timeout() {
    // After a handler is aborted by the timeout, its VM returns to the pool and
    // serves the next request normally.
    let s = serve_with_timeout(
        "lur.serve.http('GET', '/slow', function(req) lur.async.sleep(5000) return {} end)\n\
         lur.serve.http('GET', '/ok', function(req) return { body = 'fine' } end)",
        50,
    );
    assert_eq!(s.dispatch("GET", "/slow", b"").unwrap().status, 503);
    let resp = s.dispatch("GET", "/ok", b"").expect("dispatch ok");
    assert_eq!(resp.body, b"fine");
}

#[test]
fn serve_http_is_a_registration_error_in_one_shot() {
    let rt = lur::runtime::Runtime::new().expect("runtime builds");
    assert!(
        rt.run("lur.serve.http('GET', '/x', function() end)")
            .is_err(),
        "lur.serve.http must error outside server mode"
    );
}

#[test]
fn req_cookies_parses_the_cookie_header() {
    let s = serve(
        "lur.serve.http('GET', '/c', function(req)\n\
         \treturn { body = (req.cookies.sid or '?') .. '|' .. (req.cookies.theme or '?') } end)",
    );
    let mut req = request("GET", "/c", "");
    req.headers = vec![("Cookie".to_owned(), "sid=abc; theme=dark".to_owned())];
    assert_eq!(s.dispatch_raw(&req).unwrap().body, b"abc|dark");
}

#[test]
fn req_cookies_is_empty_table_when_absent() {
    let s = serve(
        "lur.serve.http('GET', '/c', function(req)\n\
         \treturn { body = (next(req.cookies) == nil) and 'empty' or 'nonempty' } end)",
    );
    let resp = s.dispatch("GET", "/c", b"").expect("dispatch ok");
    assert_eq!(resp.body, b"empty");
}

#[test]
fn req_cookies_merges_multiple_headers_later_wins() {
    let s = serve(
        "lur.serve.http('GET', '/c', function(req)\n\
         \treturn { body = req.cookies.a .. '|' .. req.cookies.b } end)",
    );
    let mut req = request("GET", "/c", "");
    req.headers = vec![
        ("Cookie".to_owned(), "a=1; b=2".to_owned()),
        ("Cookie".to_owned(), "b=3".to_owned()),
    ];
    assert_eq!(s.dispatch_raw(&req).unwrap().body, b"1|3");
}

#[test]
fn handler_error_carries_location_for_diagnostics() {
    // A handler that raises a Lua error is returned from dispatch as
    // Err(RunError::Script(...)). The error must carry a parsable chunk+line
    // location so that diagnostics::render (called in the hyper handle layer)
    // can produce a rustc-style snippet. We verify the location is present; the
    // render output itself is covered by diagnostics::tests.
    let s = serve(
        "lur.serve.http('GET', '/boom', function(req)\n\
         \tlocal x = nil\n\
         \treturn x.y\n\
         end)",
    );
    // dispatch propagates Lua errors as Err; the 500 is produced by the hyper adapter.
    let err = s
        .dispatch("GET", "/boom", b"")
        .expect_err("a handler error must be returned as Err from dispatch");
    // With chunk_name defaulting to "script", the error must contain "script:3".
    let msg = err.to_string();
    assert!(
        msg.contains("script:3"),
        "error must carry the chunk name and line so the renderer can locate it: {msg}"
    );
}
