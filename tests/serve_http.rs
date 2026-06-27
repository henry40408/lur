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
impl Reaper {
    fn pid(&self) -> u32 {
        self.0.id()
    }

    /// Poll for the child to exit within `budget`, returning its status.
    fn wait_within(&mut self, budget: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + budget;
        loop {
            match self.0.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => return None,
            }
        }
    }
}
impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Send SIGTERM to a child process (no extra deps — shells out to `kill`).
fn send_sigterm(pid: u32) {
    Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .expect("send SIGTERM");
}

/// Spawn `lur serve` on a fresh port with `app_src` as the app. Returns the
/// bound address, the reaper, and the tempdir (kept alive for the app file).
fn spawn_server(app_src: &str) -> (String, Reaper, tempfile::TempDir) {
    spawn_server_args(app_src, &[])
}

/// As [`spawn_server`], with extra CLI arguments (e.g. `--pool-size`).
fn spawn_server_args(app_src: &str, extra: &[&str]) -> (String, Reaper, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app.lua");
    std::fs::write(&app, app_src).unwrap();

    let addr = format!("127.0.0.1:{}", free_port());
    let bin = assert_cmd::cargo::cargo_bin("lur");
    let child = Command::new(bin)
        .arg("serve")
        .arg("--bind")
        .arg(&addr)
        .args(extra)
        .arg(&app)
        .spawn()
        .expect("spawn lur serve");
    (addr, Reaper(child), dir)
}

/// Send a raw request and read the whole response (server closes on `Connection: close`).
fn round_trip(addr: &str, request: &str) -> String {
    let mut stream = wait_until_up(addr);
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn serve_answers_an_http_request() {
    let (addr, _reaper, _dir) = spawn_server(
        "lur.serve.http('GET', '/ping', function(req)\n\
         \treturn { status = 200, body = 'pong' }\n\
         end)",
    );

    let response = round_trip(
        &addr,
        "GET /ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    assert!(
        response.starts_with("HTTP/1.1 200"),
        "status line: {response:?}"
    );
    assert!(response.contains("pong"), "body missing: {response:?}");
}

#[test]
fn serve_threads_param_query_and_header_to_the_handler() {
    let (addr, _reaper, _dir) = spawn_server(
        "lur.serve.http('GET', '/users/:id', function(req)\n\
         \treturn { body = req.params.id .. ' ' .. req.query.q .. ' ' .. req.headers['x-test'] }\n\
         end)",
    );

    let response = round_trip(
        &addr,
        "GET /users/99?q=hi HTTP/1.1\r\nHost: localhost\r\nX-Test: yo\r\nConnection: close\r\n\r\n",
    );

    assert!(
        response.starts_with("HTTP/1.1 200"),
        "status line: {response:?}"
    );
    assert!(
        response.ends_with("99 hi yo"),
        "body should echo param/query/header: {response:?}"
    );
}

#[test]
fn cron_fires_on_schedule_over_http() {
    // A 1-second cron increments a lur.kv counter; an HTTP route reads it. After
    // a few seconds the counter must have advanced, proving the scheduler fires.
    let db_dir = tempfile::tempdir().unwrap();
    let db = db_dir.path().join("cron.db");
    let (addr, _reaper, _dir) = spawn_server_args(
        "lur.serve.cron('* * * * * *', function()\n\
         \tlocal n = (tonumber(lur.kv.get('c')) or 0) + 1\n\
         \tlur.kv.set('c', tostring(n))\n\
         end)\n\
         lur.serve.http('GET', '/c', function() return { body = lur.kv.get('c') or '0' } end)",
        &["--db", db.to_str().unwrap()],
    );
    drop(wait_until_up(&addr));

    std::thread::sleep(Duration::from_millis(3500));
    let response = round_trip(
        &addr,
        "GET /c HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    let body = response.rsplit("\r\n\r\n").next().unwrap_or("").trim();
    let count: i32 = body.parse().unwrap_or(0);
    assert!(
        count >= 2,
        "a 1s cron should have fired at least twice in ~3.5s, got {count} ({response:?})"
    );
}

#[test]
fn handler_exceeding_per_event_timeout_returns_503_over_http() {
    let (addr, _reaper, _dir) = spawn_server_args(
        "lur.serve.http('GET', '/slow', function(req)\n\
         \tlur.async.sleep(5000)\n\
         \treturn { body = 'never' }\n\
         end)",
        &["--timeout-ms", "50"],
    );

    let response = round_trip(
        &addr,
        "GET /slow HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    assert!(
        response.starts_with("HTTP/1.1 503"),
        "a timed-out handler should yield 503: {response:?}"
    );
}

#[test]
fn oversize_body_is_rejected_with_413_over_http() {
    let (addr, _reaper, _dir) = spawn_server_args(
        "lur.serve.http('POST', '/u', function(req) return { body = 'reached' } end)",
        &["--max-body", "4"],
    );

    let body = "toolong"; // 7 bytes > 4
    let request = format!(
        "POST /u HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let response = round_trip(&addr, &request);

    assert!(
        response.starts_with("HTTP/1.1 413"),
        "an oversize body should yield 413: {response:?}"
    );
    assert!(
        !response.contains("reached"),
        "the handler must not run for an oversize body: {response:?}"
    );
}

#[test]
fn sigterm_drains_in_flight_request_then_exits_cleanly() {
    // A handler that takes 400ms. We fire the request, SIGTERM the server while
    // it is in flight, and require: (1) the in-flight request still completes,
    // (2) the process then exits cleanly (0) rather than dying on the signal.
    let (addr, mut reaper, _dir) = spawn_server(
        "lur.serve.http('GET', '/slow', function()\n\
         \tlur.async.sleep(400)\n\
         \treturn { body = 'drained' }\n\
         end)",
    );
    let mut stream = wait_until_up(&addr);
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .unwrap();

    // Let the handler start, then ask the server to shut down.
    std::thread::sleep(Duration::from_millis(120));
    send_sigterm(reaper.pid());

    // The in-flight request must still get its full response.
    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    assert!(
        resp.contains("drained"),
        "in-flight request should drain to completion: {resp:?}"
    );

    // And the server should exit cleanly within the grace window.
    let status = reaper
        .wait_within(Duration::from_secs(5))
        .expect("server should exit after draining");
    assert!(
        status.success(),
        "graceful shutdown should exit 0, got {status:?}"
    );
}

#[test]
fn pool_serves_concurrent_requests_in_parallel() {
    // Each request sleeps 200ms. With a 2-VM pool, two concurrent requests run
    // on separate VMs and finish together (~200ms), not serialized (~400ms).
    let (addr, _reaper, _dir) = spawn_server_args(
        "lur.serve.http('GET', '/slow', function(req)\n\
         \tlur.async.sleep(200)\n\
         \treturn { body = 'done' }\n\
         end)",
        &["--pool-size", "2"],
    );
    // Make sure the server is up before timing.
    drop(wait_until_up(&addr));

    let request = "GET /slow HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    let start = Instant::now();
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let addr = addr.clone();
            std::thread::spawn(move || round_trip(&addr, request))
        })
        .collect();
    for t in threads {
        let resp = t.join().unwrap();
        assert!(resp.contains("done"), "handler response: {resp:?}");
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(350),
        "two 200ms requests on a 2-VM pool should overlap, took {elapsed:?}"
    );
}
