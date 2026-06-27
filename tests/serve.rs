use lur::runtime::RuntimeConfig;
use lur::serve::{RawRequest, Server};

fn serve(source: &str) -> Server {
    Server::load(source, RuntimeConfig::default()).expect("app loads")
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
fn serve_http_is_a_registration_error_in_one_shot() {
    let rt = lur::runtime::Runtime::new().expect("runtime builds");
    assert!(
        rt.run("lur.serve.http('GET', '/x', function() end)")
            .is_err(),
        "lur.serve.http must error outside server mode"
    );
}
