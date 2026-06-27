use lur::runtime::RuntimeConfig;
use lur::serve::Server;

fn serve(source: &str) -> Server {
    Server::load(source, RuntimeConfig::default()).expect("app loads")
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
fn serve_http_is_a_registration_error_in_one_shot() {
    let rt = lur::runtime::Runtime::new().expect("runtime builds");
    assert!(
        rt.run("lur.serve.http('GET', '/x', function() end)")
            .is_err(),
        "lur.serve.http must error outside server mode"
    );
}
