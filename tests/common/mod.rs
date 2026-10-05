//! A wiremock HTTP server for synchronous integration tests. It runs on its own
//! tokio runtime so tests stay plain `#[test]` and `lur::Runtime` can block freely.

// Each test binary uses a different subset of this module.
#![allow(dead_code)]

use std::net::TcpListener;

use tokio::runtime::{Builder, Runtime};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Respond};

pub struct Server {
    // Dropped before `rt`, which it needs to shut down.
    inner: MockServer,
    rt: Runtime,
}

impl Server {
    /// Answers every request on 127.0.0.1 with `responder`.
    pub fn start(responder: impl Respond + 'static) -> Self {
        Self::on("127.0.0.1:0", responder).expect("bind 127.0.0.1")
    }

    /// Like [`start`](Self::start) on IPv6 loopback; `None` if the host has no `::1`.
    pub fn start_v6(responder: impl Respond + 'static) -> Option<Self> {
        match Self::on("[::1]:0", responder) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("skipping: cannot bind [::1]: {e}");
                None
            }
        }
    }

    fn on(addr: &str, responder: impl Respond + 'static) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        let rt = Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        let inner = rt.block_on(async {
            let server = MockServer::builder().listener(listener).start().await;
            Mock::given(any())
                .respond_with(responder)
                .mount(&server)
                .await;
            server
        });
        Ok(Self { inner, rt })
    }

    pub fn port(&self) -> u16 {
        self.inner.address().port()
    }

    /// Requests received so far.
    pub fn hits(&self) -> usize {
        self.rt
            .block_on(self.inner.received_requests())
            .map_or(0, |r| r.len())
    }
}
