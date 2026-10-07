use std::io::Write;
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

use lur::policy::Policy;
use lur::runtime::{Runtime, RuntimeConfig};
use wiremock::{Request, ResponseTemplate};

mod common;
use common::Server;

fn fixed(status: u16, body: &'static str) -> Server {
    Server::start(ResponseTemplate::new(status).set_body_string(body))
}

fn fixed_v6(status: u16, body: &'static str) -> Option<Server> {
    Server::start_v6(ResponseTemplate::new(status).set_body_string(body))
}

fn echo() -> Server {
    Server::start(|req: &Request| ResponseTemplate::new(200).set_body_bytes(req.body.clone()))
}

fn redirect(location: &str) -> Server {
    Server::start(ResponseTemplate::new(302).insert_header("Location", location))
}

fn runtime_with(policy: Policy) -> Runtime {
    Runtime::with_config(RuntimeConfig {
        policy: Arc::new(policy),
        ..Default::default()
    })
    .expect("runtime builds")
}

/// Permits the loopback test server (127.0.0.1 is private).
fn loopback_policy() -> Policy {
    Policy::strict()
        .with_net(vec!["127.0.0.1".to_string()])
        .allow_private()
}

#[test]
fn http_get_returns_status_and_body() {
    let srv = fixed(200, "hello-http");
    let port = srv.port();
    let rt = runtime_with(loopback_policy());
    rt.run(&format!(
        "local r = lur.http.get('http://127.0.0.1:{port}/')\n\
         assert(r.status == 200, 'status')\n\
         assert(r.body == 'hello-http', 'body')",
    ))
    .expect("GET works");
}

#[test]
fn http_post_sends_body_and_echoes() {
    let srv = echo();
    let port = srv.port();
    let rt = runtime_with(loopback_policy());
    rt.run(&format!(
        "local r = lur.http.post('http://127.0.0.1:{port}/', {{ body = 'ping' }})\n\
         assert(r.status == 200, 'status')\n\
         assert(r.body == 'ping', 'echoed body')",
    ))
    .expect("POST body works");
}

#[test]
fn http_res_json_decodes_body() {
    let srv = fixed(200, "{\"ok\":true,\"n\":7}");
    let port = srv.port();
    let rt = runtime_with(loopback_policy());
    rt.run(&format!(
        "local r = lur.http.get('http://127.0.0.1:{port}/')\n\
         local j = r.json()\n\
         assert(j.ok == true, 'ok')\n\
         assert(j.n == 7, 'n')",
    ))
    .expect("res.json works");
}

#[test]
fn http_json_opt_sets_body() {
    let srv = echo();
    let port = srv.port();
    let rt = runtime_with(loopback_policy());
    rt.run(&format!(
        "local r = lur.http.post('http://127.0.0.1:{port}/', {{ json = {{ a = 1 }} }})\n\
         assert(r.body == '{{\"a\":1}}', 'json body: ' .. r.body)",
    ))
    .expect("json opt works");
}

#[test]
fn http_denied_when_host_not_allowlisted() {
    let srv = fixed(200, "x");
    let port = srv.port();
    let rt = runtime_with(
        Policy::strict()
            .with_net(vec!["example.com".to_string()])
            .allow_private(),
    );
    assert!(
        rt.run(&format!("lur.http.get('http://127.0.0.1:{port}/')"))
            .is_err(),
        "request to non-allowlisted host must error"
    );
}

#[test]
fn http_private_ip_denied_by_default() {
    let srv = fixed(200, "x");
    let port = srv.port();
    // Allowlisted but private → SSRF guard blocks.
    let rt = runtime_with(Policy::strict().with_net(vec!["127.0.0.1".to_string()]));
    assert!(
        rt.run(&format!("lur.http.get('http://127.0.0.1:{port}/')"))
            .is_err(),
        "loopback must be denied without --allow-private"
    );
}

#[test]
fn http_body_exceeding_cap_errors() {
    let srv = fixed(200, "hello-http"); // 10 bytes
    let port = srv.port();
    let rt = Runtime::with_config(RuntimeConfig {
        policy: Arc::new(loopback_policy()),
        max_http_body: 4, // smaller than the response
        ..Default::default()
    })
    .expect("runtime builds");
    assert!(
        rt.run(&format!("lur.http.get('http://127.0.0.1:{port}/')"))
            .is_err(),
        "a response body over the cap must error"
    );
}

#[test]
fn http_redirect_to_disallowed_host_is_blocked() {
    let srv = redirect("http://evil.example:9/");
    let target = srv.port();
    let rt = runtime_with(loopback_policy());
    assert!(
        rt.run(&format!("lur.http.get('http://127.0.0.1:{target}/')"))
            .is_err(),
        "redirect to a disallowed host must be blocked"
    );
}

/// Asserts the script fails at the policy gate, not at connect time, so a
/// closed port cannot make a bypass look like a deny.
fn assert_policy_denied(rt: &Runtime, script: &str) {
    let err = rt
        .run(script)
        .expect_err("request must be denied")
        .to_string();
    assert!(
        err.contains("not allowed by the policy"),
        "expected a policy denial, got: {err}"
    );
}

#[test]
fn http_ipv6_loopback_literal_denied_by_default() {
    // IP literals skip the DNS resolver, so url_allowed is the only gate.
    let rt = runtime_with(Policy::strict().with_net(vec!["*".to_string()]));
    for host in ["[::1]", "[::]", "[0:0:0:0:0:0:0:1]"] {
        assert_policy_denied(&rt, &format!("lur.http.get('http://{host}:9/')"));
    }
}

#[test]
fn http_ipv4_mapped_ipv6_literal_denied_by_default() {
    let srv = fixed(200, "x");
    let port = srv.port();
    let rt = runtime_with(Policy::strict().with_net(vec!["*".to_string()]));
    for host in [
        "[::ffff:127.0.0.1]",
        "[::ffff:169.254.169.254]",
        "[::127.0.0.1]",
        "[64:ff9b::127.0.0.1]",
        "[2002:7f00:1::]",
    ] {
        assert_policy_denied(&rt, &format!("lur.http.get('http://{host}:{port}/')"));
    }
}

#[test]
fn http_ipv6_allowlist_entry_matches_url_literal() {
    let Some(srv) = fixed_v6(200, "v6") else {
        return;
    };
    let port = srv.port();
    for rule in [
        "::1".to_string(),
        "[::1]".to_string(),
        format!("[::1]:{port}"),
    ] {
        let rt = runtime_with(
            Policy::strict()
                .with_net(vec![rule.clone()])
                .allow_private(),
        );
        rt.run(&format!(
            "local r = lur.http.get('http://[::1]:{port}/')\n\
             assert(r.status == 200 and r.body == 'v6', 'response')",
        ))
        .unwrap_or_else(|e| panic!("rule {rule:?} should permit [::1]: {e}"));
    }
}

#[test]
fn http_ipv6_allowlist_entry_still_needs_allow_private() {
    let rt = runtime_with(Policy::strict().with_net(vec!["::1".to_string()]));
    assert_policy_denied(&rt, "lur.http.get('http://[::1]:9/')");
}

#[test]
fn http_redirect_to_ipv6_literal_is_checked() {
    let Some(v6_srv) = fixed_v6(200, "v6") else {
        return;
    };
    let target = format!("http://[::1]:{}/", v6_srv.port());

    // Not on the allowlist → the redirect hop is refused.
    let origin_srv = redirect(&target);
    let origin = origin_srv.port();
    let rt = runtime_with(loopback_policy());
    let err = rt
        .run(&format!("lur.http.get('http://127.0.0.1:{origin}/')"))
        .expect_err("redirect to a non-allowlisted IPv6 literal must be blocked")
        .to_string();
    // reqwest's Display hides the policy's message; the target is live, so a
    // failed redirect here can only come from the redirect policy.
    assert!(err.contains("error following redirect"), "got: {err}");

    // Allowlisted (and private allowed) → followed.
    let origin_srv = redirect(&target);
    let origin = origin_srv.port();
    let rt = runtime_with(
        Policy::strict()
            .with_net(vec!["127.0.0.1".to_string(), "::1".to_string()])
            .allow_private(),
    );
    rt.run(&format!(
        "local r = lur.http.get('http://127.0.0.1:{origin}/')\n\
         assert(r.status == 200 and r.body == 'v6', 'followed')",
    ))
    .expect("allowlisted IPv6 redirect target is followed");
}

#[test]
fn http_proxy_env_is_ignored() {
    use std::sync::atomic::{AtomicBool, Ordering};

    // A "proxy" on loopback that records whether anything connected to it.
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let hit = Arc::new(AtomicBool::new(false));
    {
        let hit = Arc::clone(&hit);
        thread::spawn(move || {
            for stream in proxy.incoming() {
                hit.store(true, Ordering::SeqCst);
                let mut s = stream.unwrap();
                let _ = s.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nproxied",
                );
            }
        });
    }

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("proxy.lua");
    // `.invalid` never resolves, so only a proxy could make this succeed.
    std::fs::write(&script, "lur.http.get('http://lur-proxy-test.invalid/')").unwrap();
    let out = assert_cmd::Command::cargo_bin("lur")
        .unwrap()
        .env("XDG_CONFIG_HOME", dir.path())
        .env("HTTP_PROXY", &proxy_url)
        .env("http_proxy", &proxy_url)
        .env("ALL_PROXY", &proxy_url)
        .env("all_proxy", &proxy_url)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .args(["--no-config", "--allow-net", "*"])
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "request must not go through the proxy"
    );
    assert!(!hit.load(Ordering::SeqCst), "proxy must never be contacted");
}
