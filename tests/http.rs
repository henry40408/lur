use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

use lur::policy::Policy;
use lur::runtime::{Runtime, RuntimeConfig};

fn runtime_with(policy: Policy) -> Runtime {
    Runtime::with_config(RuntimeConfig {
        policy: Arc::new(policy),
        ..Default::default()
    })
    .expect("runtime builds")
}

/// loopback policy that permits the test server (127.0.0.1 is private).
fn loopback_policy() -> Policy {
    Policy::strict()
        .with_net(vec!["127.0.0.1".to_string()])
        .allow_private()
}

enum Resp {
    Fixed(u16, &'static str),
    Echo,
    Redirect(String),
}

/// A tiny one-request-per-connection HTTP/1.1 server in a background thread.
fn spawn(resp: Resp) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut s = stream.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let body = loop {
                let n = s.read(&mut tmp).unwrap_or(0);
                if n == 0 {
                    break Vec::new();
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = find(&buf, b"\r\n\r\n") {
                    let cl = content_length(&buf[..pos]);
                    let start = pos + 4;
                    while buf.len() - start < cl {
                        let n = s.read(&mut tmp).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    break buf[start..(start + cl).min(buf.len())].to_vec();
                }
            };
            let mut out = Vec::new();
            match &resp {
                Resp::Fixed(code, b) => {
                    out.extend_from_slice(
                        format!(
                            "HTTP/1.1 {code} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            b.len()
                        )
                        .as_bytes(),
                    );
                    out.extend_from_slice(b.as_bytes());
                }
                Resp::Echo => {
                    out.extend_from_slice(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    );
                    out.extend_from_slice(&body);
                }
                Resp::Redirect(loc) => {
                    out.extend_from_slice(
                        format!(
                            "HTTP/1.1 302 Found\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    );
                }
            }
            let _ = s.write_all(&out);
            let _ = s.flush();
        }
    });
    port
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn content_length(headers: &[u8]) -> usize {
    let text = String::from_utf8_lossy(headers).to_lowercase();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("content-length:") {
            return v.trim().parse().unwrap_or(0);
        }
    }
    0
}

#[test]
fn http_get_returns_status_and_body() {
    let port = spawn(Resp::Fixed(200, "hello-http"));
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
    let port = spawn(Resp::Echo);
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
    let port = spawn(Resp::Fixed(200, "{\"ok\":true,\"n\":7}"));
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
    let port = spawn(Resp::Echo);
    let rt = runtime_with(loopback_policy());
    rt.run(&format!(
        "local r = lur.http.post('http://127.0.0.1:{port}/', {{ json = {{ a = 1 }} }})\n\
         assert(r.body == '{{\"a\":1}}', 'json body: ' .. r.body)",
    ))
    .expect("json opt works");
}

#[test]
fn http_denied_when_host_not_allowlisted() {
    let port = spawn(Resp::Fixed(200, "x"));
    // allowlist a different host; the request target is not permitted.
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
    let port = spawn(Resp::Fixed(200, "x"));
    // Host allowlisted, but private network not permitted → blocked (SSRF deny).
    let rt = runtime_with(Policy::strict().with_net(vec!["127.0.0.1".to_string()]));
    assert!(
        rt.run(&format!("lur.http.get('http://127.0.0.1:{port}/')"))
            .is_err(),
        "loopback must be denied without --allow-private"
    );
}

#[test]
fn http_redirect_to_disallowed_host_is_blocked() {
    // Redirect to a host that is not on the allowlist must be refused per-hop.
    let target = spawn(Resp::Redirect("http://evil.example:9/".to_string()));
    let rt = runtime_with(loopback_policy());
    assert!(
        rt.run(&format!("lur.http.get('http://127.0.0.1:{target}/')"))
            .is_err(),
        "redirect to a disallowed host must be blocked"
    );
}
