//! Pingora as a reference application for rtlog: a reverse proxy whose
//! per-request access log is on the latency path of every request.
//!
//! One process holds everything, so a run needs no other tool:
//!
//! - an upstream HTTP server that answers every request with the same body;
//! - a Pingora proxy in front of it, whose `logging` hook writes one access
//!   log record per request, through the logger chosen on the command line;
//! - a load generator: keep-alive connections that send requests one after
//!   the other and time each one.
//!
//! ```text
//! rtlog-reference-pingora <none|log|rtlog> <out-dir> [seconds] [connections] [proxy-threads]
//! ```
//!
//! It prints one markdown table row: requests per second, latency
//! percentiles in microseconds, the mean time of the access-log call itself
//! in nanoseconds (read with `Instant`, which adds about 20 ns to each
//! variant alike), log bytes per request and records rtlog dropped.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pingora_core::server::configuration::ServerConf;
use pingora_core::server::Server;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_proxy::{ProxyHttp, Session};

const BODY: &str = "hello from the upstream\n";

/// Time spent inside the access-log call itself, summed over all requests,
/// and the number of calls: what the logger adds to each request.
static LOG_NANOS: AtomicU64 = AtomicU64::new(0);
static LOG_CALLS: AtomicU64 = AtomicU64::new(0);

/// Which logger the proxy's access log goes through.
#[derive(Clone, Copy, PartialEq)]
enum Logger {
    /// No access log: the cost of the proxy alone.
    None,
    /// `log::info!` with `env_logger` writing text to a file, one `write` per
    /// record, as with the usual stderr target.
    Log,
    /// `rtlog::info!` into the worker thread's ring; a drain thread writes
    /// the binary frames to a file.
    Rtlog,
}

struct Proxy {
    upstream_port: u16,
    logger: Logger,
}

#[async_trait]
impl ProxyHttp for Proxy {
    /// When the request arrived.
    type CTX = Instant;

    fn new_ctx(&self) -> Self::CTX {
        Instant::now()
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        Ok(Box::new(HttpPeer::new(
            ("127.0.0.1", self.upstream_port),
            false,
            String::new(),
        )?))
    }

    /// The access log: one record per request with the same six fields for
    /// both loggers.
    async fn logging(
        &self,
        session: &mut Session,
        _e: Option<&pingora_core::Error>,
        ctx: &mut Self::CTX,
    ) {
        if self.logger == Logger::None {
            return;
        }
        let req = session.req_header();
        let method = req.method.as_str();
        let path = req.uri.path();
        let status = session
            .response_written()
            .map_or(0, |resp| resp.status.as_u16());
        let bytes = session.body_bytes_sent() as u64;
        let peer_port = session
            .client_addr()
            .and_then(|a| a.as_inet())
            .map_or(0, |a| a.port());
        let dur_us = ctx.elapsed().as_micros() as u64;
        let call = Instant::now();
        match self.logger {
            Logger::Log => log::info!(
                "access method={method} path={path} status={status} bytes={bytes} peer_port={peer_port} dur_us={dur_us}"
            ),
            Logger::Rtlog => rtlog::info!(
                "access method={} path={} status={} bytes={} peer_port={} dur_us={}",
                method,
                path,
                status,
                bytes,
                peer_port,
                dur_us
            ),
            Logger::None => {}
        }
        LOG_NANOS.fetch_add(call.elapsed().as_nanos() as u64, Ordering::Relaxed);
        LOG_CALLS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Read one HTTP message head (up to the blank line) and, for a response,
/// its body. Returns `false` when the peer closed the connection.
fn read_message(stream: &mut TcpStream, buf: &mut Vec<u8>, with_body: bool) -> bool {
    buf.clear();
    let mut chunk = [0u8; 2048];
    loop {
        if let Some(head) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let body = if with_body {
                let text = String::from_utf8_lossy(&buf[..head]).to_ascii_lowercase();
                text.lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0)
            } else {
                0
            };
            if buf.len() >= head + 4 + body {
                return true;
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return false,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// The upstream: every request gets the same small response.
fn upstream(listener: TcpListener) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{BODY}",
        BODY.len()
    );
    for stream in listener.incoming().flatten() {
        let response = response.clone();
        std::thread::spawn(move || {
            let mut stream = stream;
            let _ = stream.set_nodelay(true);
            let mut buf = Vec::new();
            while read_message(&mut stream, &mut buf, false) {
                if stream.write_all(response.as_bytes()).is_err() {
                    break;
                }
            }
        });
    }
}

/// One keep-alive client: requests one after the other until `stop`;
/// returns each request's latency in microseconds.
fn client(proxy_port: u16, id: usize, stop: &AtomicBool) -> Vec<u32> {
    let mut stream = loop {
        match TcpStream::connect(("127.0.0.1", proxy_port)) {
            Ok(s) => break s,
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let _ = stream.set_nodelay(true);
    let request = format!("GET /api/v1/items/{id} HTTP/1.1\r\nHost: bench.local\r\n\r\n");
    let mut latencies = Vec::with_capacity(1 << 20);
    let mut buf = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let start = Instant::now();
        if stream.write_all(request.as_bytes()).is_err()
            || !read_message(&mut stream, &mut buf, true)
        {
            break;
        }
        latencies.push(start.elapsed().as_micros().min(u128::from(u32::MAX)) as u32);
    }
    latencies
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("a free TCP port")
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let usage = "usage: rtlog-reference-pingora <none|log|rtlog> <out-dir> [seconds] [connections] [proxy-threads]";
    let logger = match args.get(1).map(String::as_str) {
        Some("none") => Logger::None,
        Some("log") => Logger::Log,
        Some("rtlog") => Logger::Rtlog,
        _ => return Err(usage.into()),
    };
    let out = PathBuf::from(args.get(2).ok_or(usage)?);
    let number =
        |i: usize, default: usize| args.get(i).and_then(|v| v.parse().ok()).unwrap_or(default);
    let (seconds, connections, threads) = (number(3, 10), number(4, 32), number(5, 4));
    std::fs::create_dir_all(&out)?;

    // The logger under test.
    let text_log = out.join("access.log");
    let binary_log = out.join("access.rtlog");
    let mut drain = None;
    match logger {
        Logger::None => {}
        Logger::Log => {
            let file = std::fs::File::create(&text_log)?;
            env_logger::Builder::new()
                .filter_level(log::LevelFilter::Info)
                .target(env_logger::Target::Pipe(Box::new(file)))
                .init();
        }
        Logger::Rtlog => {
            rtlog::contract::write(out.join("access.contract.jsonl"))?;
            let file = std::io::BufWriter::new(std::fs::File::create(&binary_log)?);
            drain = Some(rtlog::spawn_drain(
                rtlog::IoTransport(file),
                Duration::from_millis(1),
            ));
        }
    }

    // Upstream, then the proxy in front of it.
    let upstream_listener = TcpListener::bind("127.0.0.1:0")?;
    let upstream_port = upstream_listener.local_addr()?.port();
    std::thread::spawn(move || upstream(upstream_listener));

    let proxy_port = free_port();
    std::thread::spawn(move || {
        let conf = ServerConf {
            threads,
            ..ServerConf::default()
        };
        let mut server = Server::new_with_opt_and_conf(None, conf);
        server.bootstrap();
        let mut service = pingora_proxy::http_proxy_service(
            &server.configuration,
            Proxy {
                upstream_port,
                logger,
            },
        );
        service.add_tcp(&format!("127.0.0.1:{proxy_port}"));
        server.add_service(service);
        server.run_forever();
    });

    // Load: a warm-up that is not counted, then the measured run.
    for (run_seconds, measured) in [(1.max(seconds / 5), false), (seconds, true)] {
        let before = (file_len(&text_log), file_len(&binary_log));
        let calls_before = (
            LOG_NANOS.load(Ordering::Relaxed),
            LOG_CALLS.load(Ordering::Relaxed),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let clients: Vec<_> = (0..connections)
            .map(|id| {
                let stop = stop.clone();
                std::thread::spawn(move || client(proxy_port, id, &stop))
            })
            .collect();
        let start = Instant::now();
        std::thread::sleep(Duration::from_secs(run_seconds as u64));
        stop.store(true, Ordering::Relaxed);
        let mut latencies: Vec<u32> = clients
            .into_iter()
            .flat_map(|c| c.join().unwrap_or_default())
            .collect();
        let elapsed = start.elapsed().as_secs_f64();
        if !measured {
            continue;
        }
        // Let the drain write what the rings still hold.
        std::thread::sleep(Duration::from_millis(200));
        latencies.sort_unstable();
        let n = latencies.len().max(1);
        let pct = |p: f64| {
            latencies
                .get(((n as f64 * p) as usize).min(n - 1))
                .copied()
                .unwrap_or(0)
        };
        let log_bytes = (file_len(&text_log) - before.0) + (file_len(&binary_log) - before.1);
        let nanos = LOG_NANOS.load(Ordering::Relaxed) - calls_before.0;
        let calls = (LOG_CALLS.load(Ordering::Relaxed) - calls_before.1).max(1);
        println!(
            "| {} | {:.0} | {} | {} | {} | {} | {:.1} | {} |",
            args[1],
            latencies.len() as f64 / elapsed,
            pct(0.50),
            pct(0.99),
            pct(0.999),
            nanos / calls,
            log_bytes as f64 / n as f64,
            rtlog::stats().drops,
        );
    }
    if let Some(drain) = drain {
        let mut file = drain.stop();
        rtlog::Transport::flush(&mut file);
    }
    // The proxy thread never returns; end the process.
    std::process::exit(0);
}
