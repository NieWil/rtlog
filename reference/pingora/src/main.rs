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
//! rtlog-reference-pingora <none|log|log-buf|tracing|rtlog> <out-dir> [seconds] [connections] [proxy-threads]
//! ```
//!
//! It prints one markdown table row: requests per second, request latency
//! percentiles in microseconds, the time of the access-log call itself in
//! nanoseconds (mean, median, 99th and 99.9th percentile, longest; read with
//! `Instant`, which adds about 20 ns to each variant alike), log bytes per
//! request, the process's peak resident memory and records rtlog dropped.
//!
//! The access-log call is one function, `pingora_access_log`. With `icount`
//! as the first argument the program starts no proxy: it sets the logger up
//! the same way and makes 20 000 of those calls with fixed fields from one
//! thread, so that Valgrind's callgrind can count the instructions of the
//! call (`--toggle-collect=pingora_access_log`) and get the same number on
//! every run (`run.sh`).

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
/// Set during the measured run: the warm-up is not timed.
static MEASURING: AtomicBool = AtomicBool::new(false);
/// Calls made through `pingora_access_log` over the whole run, for the
/// instruction count (each thread's first call goes elsewhere).
static COUNTED_CALLS: AtomicU64 = AtomicU64::new(0);
/// How long each access-log call took: a histogram with eight buckets per
/// power of two of nanoseconds, and the longest call.
static CALL_HISTOGRAM: [AtomicU64; 512] = [const { AtomicU64::new(0) }; 512];
static CALL_MAX_NANOS: AtomicU64 = AtomicU64::new(0);

fn bucket(nanos: u64) -> usize {
    if nanos < 8 {
        return nanos as usize;
    }
    let exponent = 63 - nanos.leading_zeros() as usize;
    (exponent - 2) * 8 + ((nanos >> (exponent - 3)) & 7) as usize
}

/// Lower bound, in nanoseconds, of the values counted in bucket `index`.
fn bucket_floor(index: usize) -> u64 {
    if index < 8 {
        return index as u64;
    }
    (8 + (index % 8) as u64) << (index / 8 - 1)
}

/// The value below which `fraction` of the timed calls fall.
fn call_percentile(fraction: f64) -> u64 {
    let total: u64 = CALL_HISTOGRAM
        .iter()
        .map(|b| b.load(Ordering::Relaxed))
        .sum();
    let wanted = (total as f64 * fraction) as u64;
    let mut seen = 0;
    for (index, b) in CALL_HISTOGRAM.iter().enumerate() {
        seen += b.load(Ordering::Relaxed);
        if seen > wanted {
            return bucket_floor(index);
        }
    }
    0
}

thread_local! {
    /// Whether this worker thread has made its first access-log call.
    static WARM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Which logger the proxy's access log goes through.
#[derive(Clone, Copy, PartialEq)]
enum Logger {
    /// No access log: the cost of the proxy alone.
    None,
    /// `log::info!` with `env_logger` writing text to a file, one `write` per
    /// record, as with the usual stderr target.
    Log,
    /// The same through a 64 KiB `BufWriter`: formatting and a lock per
    /// record, a `write` only when the buffer is full.
    LogBuffered,
    /// `tracing::info!` with `tracing-subscriber`'s text format and
    /// `tracing-appender`'s non-blocking writer: the record is formatted on
    /// the worker thread and written by a background thread.
    Tracing,
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
        // A thread's first call sets the logger up for that thread (rtlog
        // creates its ring, the text loggers their buffers). It is made
        // through another function and not timed, so that what is counted
        // and timed is the steady state.
        if !WARM.replace(true) {
            access_log_first(self.logger, method, path, status, bytes, peer_port, dur_us);
            return;
        }
        let call = Instant::now();
        access_log(self.logger, method, path, status, bytes, peer_port, dur_us);
        let nanos = call.elapsed().as_nanos() as u64;
        COUNTED_CALLS.fetch_add(1, Ordering::Relaxed);
        if MEASURING.load(Ordering::Relaxed) {
            LOG_NANOS.fetch_add(nanos, Ordering::Relaxed);
            LOG_CALLS.fetch_add(1, Ordering::Relaxed);
            CALL_HISTOGRAM[bucket(nanos)].fetch_add(1, Ordering::Relaxed);
            CALL_MAX_NANOS.fetch_max(nanos, Ordering::Relaxed);
        }
    }
}

/// The access-log call under test: everything the logger does on the worker
/// thread for one request, and nothing else.
#[export_name = "pingora_access_log"]
#[inline(never)]
fn access_log(
    logger: Logger,
    method: &str,
    path: &str,
    status: u16,
    bytes: u64,
    peer_port: u16,
    dur_us: u64,
) {
    emit(logger, method, path, status, bytes, peer_port, dur_us);
}

/// The same call, for a thread's first record (see `logging`).
#[export_name = "pingora_access_log_first"]
#[inline(never)]
fn access_log_first(
    logger: Logger,
    method: &str,
    path: &str,
    status: u16,
    bytes: u64,
    peer_port: u16,
    dur_us: u64,
) {
    emit(logger, method, path, status, bytes, peer_port, dur_us);
}

#[inline(always)]
fn emit(
    logger: Logger,
    method: &str,
    path: &str,
    status: u16,
    bytes: u64,
    peer_port: u16,
    dur_us: u64,
) {
    match logger {
        Logger::Log | Logger::LogBuffered => log::info!(
            "access method={method} path={path} status={status} bytes={bytes} peer_port={peer_port} dur_us={dur_us}"
        ),
        Logger::Tracing => tracing::info!(
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

/// Peak resident set size of this process in KiB (`VmHWM`), 0 where the
/// system does not report it.
fn peak_rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmHWM:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        })
        .unwrap_or(0)
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().collect();
    let usage = "usage: rtlog-reference-pingora [icount] <none|log|log-buf|tracing|rtlog> <out-dir> [seconds] [connections] [proxy-threads]";
    // `icount`: no proxy, only the access-log call, for callgrind (see below).
    let icount = args.get(1).map(String::as_str) == Some("icount");
    if icount {
        args.remove(1);
    }
    let logger = match args.get(1).map(String::as_str) {
        Some("none") => Logger::None,
        Some("log") => Logger::Log,
        Some("log-buf") => Logger::LogBuffered,
        Some("tracing") => Logger::Tracing,
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
    let mut icount_drain = None;
    let mut tracing_guard = None;
    match logger {
        // PINGORA_IDLE_DRAIN=1: no access log, but rtlog's drain thread runs
        // with nothing to do. A control to tell the cost of the log call
        // from the effect of one more thread waking every millisecond.
        Logger::None if std::env::var_os("PINGORA_IDLE_DRAIN").is_some() => {
            let file = std::io::BufWriter::new(std::fs::File::create(&binary_log)?);
            drain = Some(rtlog::spawn_drain(
                rtlog::IoTransport(file),
                Duration::from_millis(1),
            ));
        }
        Logger::None => {}
        Logger::Log => {
            let file = std::fs::File::create(&text_log)?;
            env_logger::Builder::new()
                .filter_level(log::LevelFilter::Info)
                .target(env_logger::Target::Pipe(Box::new(file)))
                .init();
        }
        Logger::LogBuffered => {
            let file =
                std::io::BufWriter::with_capacity(64 * 1024, std::fs::File::create(&text_log)?);
            env_logger::Builder::new()
                .filter_level(log::LevelFilter::Info)
                .target(env_logger::Target::Pipe(Box::new(file)))
                .init();
        }
        Logger::Tracing => {
            let file = std::fs::File::create(&text_log)?;
            let (writer, guard) = tracing_appender::non_blocking(file);
            tracing_guard = Some(guard);
            tracing_subscriber::fmt()
                .with_writer(writer)
                .with_ansi(false)
                .with_max_level(tracing::Level::INFO)
                .init();
        }
        Logger::Rtlog => {
            rtlog::contract::write(out.join("access.contract.jsonl"))?;
            let file = std::io::BufWriter::new(std::fs::File::create(&binary_log)?);
            if icount {
                // Drained by this thread between batches: no second thread.
                icount_drain = Some(rtlog::Drain::new(rtlog::IoTransport(file)));
            } else {
                drain = Some(rtlog::spawn_drain(
                    rtlog::IoTransport(file),
                    Duration::from_millis(1),
                ));
            }
        }
    }

    if icount {
        // The same call with the same fields on every run, from one thread:
        // under callgrind with --toggle-collect=pingora_access_log the
        // instruction count is then the same on every run. The thread's
        // first call sets the logger up and goes through the other function.
        const BATCHES: u32 = 20;
        const CALLS: u32 = 1_000;
        access_log_first(logger, "GET", "/api/v1/items/7", 200, 24, 40_000, 300);
        for batch in 0..BATCHES {
            for i in 0..CALLS {
                access_log(
                    logger,
                    "GET",
                    "/api/v1/items/7",
                    200,
                    24,
                    40_000 + (i % 1_000) as u16,
                    300 + u64::from(batch * 7 + i % 700),
                );
            }
            if let Some(d) = icount_drain.as_mut() {
                d.poll().map_err(|_| "the capture file refused a frame")?;
            }
        }
        drop(tracing_guard);
        log::logger().flush();
        println!(
            "access_log_calls {} dropped {}",
            BATCHES * CALLS,
            rtlog::stats().drops
        );
        return Ok(());
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
        MEASURING.store(measured, Ordering::Relaxed);
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
        MEASURING.store(false, Ordering::Relaxed);
        let nanos = LOG_NANOS.load(Ordering::Relaxed);
        let calls = LOG_CALLS.load(Ordering::Relaxed).max(1);
        println!(
            "| {} | {:.0} | {} | {} | {} | {} | {} | {} | {} | {} | {:.1} | {:.1} | {} |",
            args[1],
            latencies.len() as f64 / elapsed,
            pct(0.50),
            pct(0.99),
            pct(0.999),
            nanos / calls,
            call_percentile(0.50),
            call_percentile(0.99),
            call_percentile(0.999),
            CALL_MAX_NANOS.load(Ordering::Relaxed),
            log_bytes as f64 / n as f64,
            peak_rss_kib() as f64 / 1024.0,
            rtlog::stats().drops,
        );
    }
    if let Some(drain) = drain {
        let mut file = drain.stop();
        rtlog::Transport::flush(&mut file);
    }
    drop(tracing_guard); // writes what the background thread still holds
    log::logger().flush();
    // For the instruction count: calls made through `pingora_access_log`
    // over the whole run, warm-up included.
    println!("access_log_calls {}", COUNTED_CALLS.load(Ordering::Relaxed));
    // The proxy thread never returns; end the process.
    std::process::exit(0);
}
