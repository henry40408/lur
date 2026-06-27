//! End-to-end test for `lur serve`: spawn the real binary, bind a port, and
//! drive it with a raw HTTP/1.1 request.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Grab an ephemeral port, then release it so the child can bind it.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Connect with retries until the server is accepting, or panic after a budget.
fn wait_until_up(addr: &str) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(stream) = TcpStream::connect(addr) {
            return stream;
        }
        if Instant::now() >= deadline {
            panic!("server never came up at {addr}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Kill the child on drop so a failed assertion doesn't leak the process.
struct Reaper(Child);
impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn serve_answers_an_http_request() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app.lua");
    std::fs::write(
        &app,
        "lur.serve.http('GET', '/ping', function(req)\n\
         \treturn { status = 200, body = 'pong' }\n\
         end)",
    )
    .unwrap();

    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let bin = assert_cmd::cargo::cargo_bin("lur");
    let child = Command::new(bin)
        .arg("serve")
        .arg("--bind")
        .arg(&addr)
        .arg(&app)
        .spawn()
        .expect("spawn lur serve");
    let _reaper = Reaper(child);

    let mut stream = wait_until_up(&addr);
    stream
        .write_all(b"GET /ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    assert!(
        response.starts_with("HTTP/1.1 200"),
        "status line: {response:?}"
    );
    assert!(response.contains("pong"), "body missing: {response:?}");
}
