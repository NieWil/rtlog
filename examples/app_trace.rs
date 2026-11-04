//! Trace a small simulated application end to end, then export it to text,
//! JSON lines and Perfetto.
//!
//! `cargo run --release -p rtlog-examples --example app_trace [out-dir]`
//! then open `<out-dir>/app.perfetto.json` in <https://ui.perfetto.dev>.
//!
//! What runs (about half a second):
//!
//! - `control`: a 1 kHz control loop. Each tick is a span with nested
//!   `sensor.read`, `pid.update` and `actuator.write` spans; PID values are
//!   logged as named fields, so they become counter tracks. Some ticks get a
//!   deliberate overrun, which the loop reports as a deadline miss.
//! - `acceptor` + `http-0..3`: requests queued to a worker pool, each a
//!   `request` span with `auth`, `db.query` and `render` inside; a few slow
//!   queries are flagged.
//! - `transfer-a` / `transfer-b`: two threads taking two locks in opposite
//!   order, the classic deadlock. Each wait is a `lock.wait` span; a watchdog
//!   in the wait loop reports a suspected deadlock after 2 ms, backs off and
//!   retries, so the run always finishes and the trace shows why it stalled.
//! - `mem-monitor`: samples a counting global allocator every 2 ms, so live
//!   heap bytes and allocation counts become counter tracks.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

// --- memory accounting ------------------------------------------------------

struct Counting;
static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
static ALLOCS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        LIVE_BYTES.fetch_add(l.size() as u64, Ordering::Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE_BYTES.fetch_sub(l.size() as u64, Ordering::Relaxed);
        System.dealloc(p, l)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

// --- helpers ----------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo)
    }
}

/// Busy work for `us` microseconds (stands in for real computation).
fn work(us: u64) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_micros(us) {
        std::hint::spin_loop();
    }
}

fn named(name: &str, f: impl FnOnce() + Send + 'static) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(f)
        .unwrap()
}

// --- the application --------------------------------------------------------

fn control_loop(ticks: u32) {
    let period = Duration::from_millis(1);
    let budget_us = 400u32;
    let mut rng = Rng(0x1234_5678);
    let (setpoint, mut measured, mut integral) = (50.0f32, 20.0f32, 0.0f32);
    let start = Instant::now();
    for tick in 0..ticks {
        let t = Instant::now();
        {
            let _tick = rtlog::span!("control.tick");
            {
                let _s = rtlog::span!("sensor.read");
                work(40 + rng.range(0, 30));
                measured += (rng.range(0, 100) as f32 - 50.0) * 0.004;
            }
            {
                let _p = rtlog::span!("pid.update");
                let err = setpoint - measured;
                integral += err * 0.001;
                let out = (0.8 * err + 2.0 * integral).clamp(-10.0, 10.0);
                measured += out * 0.05;
                // A periodic slow path (e.g. a cache miss or a page fault).
                work(if tick % 97 == 61 { 700 } else { 60 });
                rtlog::debug!(
                    "pid setpoint={:.1} measured={:.3} err={:.3} out={:.3}",
                    setpoint,
                    measured,
                    err,
                    out
                );
            }
            {
                let _a = rtlog::span!("actuator.write");
                work(25);
            }
        }
        let took = t.elapsed().as_micros() as u32;
        if took > budget_us {
            rtlog::warn!(
                "deadline miss tick={} took_us={} budget_us={}",
                tick,
                took,
                budget_us
            );
        }
        let next = start + period * (tick + 1);
        if let Some(d) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
    }
    rtlog::info!("control loop done ticks={}", ticks);
}

struct Request {
    id: u32,
    route: &'static str,
    accepted: Instant,
}

fn acceptor(tx: mpsc::Sender<Request>, n: u32) {
    let routes = ["/api/orders", "/api/stock", "/health", "/api/orders/42"];
    let mut rng = Rng(0xfeed_beef);
    for id in 0..n {
        let route = routes[rng.range(0, routes.len() as u64) as usize];
        rtlog::info!("accept id={} route={}", id, route);
        tx.send(Request {
            id,
            route,
            accepted: Instant::now(),
        })
        .unwrap();
        std::thread::sleep(Duration::from_micros(rng.range(500, 4_000)));
    }
}

fn http_worker(rx: Arc<Mutex<mpsc::Receiver<Request>>>, seed: u64) {
    let mut rng = Rng(seed);
    loop {
        let req = {
            let _w = rtlog::span!("queue.wait");
            match rx.lock().unwrap().recv() {
                Ok(r) => r,
                Err(_) => break,
            }
        };
        let _r = rtlog::span!("request");
        rtlog::debug!("start id={} route={}", req.id, req.route);
        {
            let _a = rtlog::span!("auth");
            work(rng.range(20, 90));
        }
        let rows = {
            let _q = rtlog::span!("db.query");
            let slow = rng.range(0, 20) == 0;
            let ms = if slow {
                rng.range(8, 14)
            } else {
                rng.range(1, 3)
            };
            std::thread::sleep(Duration::from_millis(ms));
            if slow {
                rtlog::warn!("slow query id={} query_ms={}", req.id, ms);
            }
            rng.range(0, 200) as u32
        };
        {
            let _x = rtlog::span!("render");
            let body: Vec<u8> = vec![b'x'; rows as usize * 64];
            work(rng.range(30, 120));
            std::hint::black_box(&body);
        }
        let status: u16 = if req.route == "/api/orders/42" {
            404
        } else {
            200
        };
        let latency_us = req.accepted.elapsed().as_micros() as u32;
        rtlog::info!(
            "done id={} status={} rows={} latency_us={}",
            req.id,
            status,
            rows,
            latency_us
        );
    }
}

/// Lock `m`, reporting a suspected deadlock if the wait exceeds `limit`.
/// Returns `None` after reporting, so the caller can back off.
fn lock_with_watchdog<'a, T>(
    m: &'a Mutex<T>,
    holding: &'static str,
    waiting: &'static str,
    limit: Duration,
) -> Option<std::sync::MutexGuard<'a, T>> {
    let _w = rtlog::span!("lock.wait");
    let t = Instant::now();
    loop {
        if let Ok(g) = m.try_lock() {
            return Some(g);
        }
        if t.elapsed() > limit {
            rtlog::error!(
                "deadlock suspected: holding={} waiting_for={} waited_us={}",
                holding,
                waiting,
                t.elapsed().as_micros() as u32
            );
            return None;
        }
        std::thread::yield_now();
    }
}

fn transfers(
    first: Arc<Mutex<i64>>,
    second: Arc<Mutex<i64>>,
    names: (&'static str, &'static str),
    rounds: u32,
    seed: u64,
) {
    let mut rng = Rng(seed);
    let mut done = 0;
    let mut backoffs = 0u32;
    while done < rounds {
        let _t = rtlog::span!("transfer");
        let Some(mut a) = lock_with_watchdog(&first, "none", names.0, Duration::from_millis(50))
        else {
            continue;
        };
        // Hold the first lock while doing some work: widens the deadlock window.
        work(rng.range(100, 400));
        match lock_with_watchdog(&second, names.0, names.1, Duration::from_millis(2)) {
            Some(mut b) => {
                let amount = rng.range(1, 50) as i64;
                *a -= amount;
                *b += amount;
                done += 1;
                rtlog::debug!(
                    "transfer ok amount={} balance_from={} balance_to={}",
                    amount,
                    *a,
                    *b
                );
            }
            None => {
                backoffs += 1;
                drop(a);
                rtlog::warn!("backing off backoffs={}", backoffs);
                std::thread::sleep(Duration::from_micros(rng.range(100, 900)));
            }
        }
        std::thread::sleep(Duration::from_micros(rng.range(200, 1_500)));
    }
    rtlog::info!("transfers done rounds={} backoffs={}", rounds, backoffs);
}

fn mem_monitor(stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        rtlog::trace!(
            "heap live_bytes={} allocs={}",
            LIVE_BYTES.load(Ordering::Relaxed),
            ALLOCS.load(Ordering::Relaxed)
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn cache_churn(rounds: u32) {
    let mut cache: Vec<Vec<u8>> = Vec::new();
    for round in 0..rounds {
        let _g = rtlog::span!("cache.fill");
        for _ in 0..40 {
            cache.push(vec![0u8; 16 * 1024]);
        }
        rtlog::debug!("cache round={} entries={}", round, cache.len() as u32);
        if cache.len() >= 160 {
            let _e = rtlog::span!("cache.evict");
            cache.clear();
            cache.shrink_to_fit();
            rtlog::info!("cache evicted round={}", round);
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "target/app_trace".into());
    std::fs::create_dir_all(&dir)?;
    let stream_path = dir.join("app.rtlog");
    let contract_path = dir.join("app.contract.jsonl");
    rtlog::contract::write(&contract_path)?;

    let file = std::io::BufWriter::new(std::fs::File::create(&stream_path)?);
    let drain = rtlog::spawn_drain(rtlog::IoTransport(file), Duration::from_millis(1));
    rtlog::set_context_name("main");
    let started = Instant::now();
    rtlog::info!("app start version={} workers={}", 1u8, 4u8);

    let stop = Arc::new(AtomicBool::new(false));
    let mon = {
        let s = stop.clone();
        named("mem-monitor", move || mem_monitor(s))
    };
    let (tx, rx) = mpsc::channel();
    let rx = Arc::new(Mutex::new(rx));
    let mut threads = vec![
        named("control", || control_loop(400)),
        named("acceptor", move || acceptor(tx, 160)),
    ];
    for i in 0..4u64 {
        let rx = rx.clone();
        threads.push(named(&format!("http-{i}"), move || {
            http_worker(rx, 0x9e37 + i)
        }));
    }
    let (acct_a, acct_b) = (
        Arc::new(Mutex::new(1_000i64)),
        Arc::new(Mutex::new(1_000i64)),
    );
    {
        let (a, b) = (acct_a.clone(), acct_b.clone());
        threads.push(named("transfer-a", move || {
            transfers(a, b, ("account_a", "account_b"), 60, 11)
        }));
        let (a, b) = (acct_a.clone(), acct_b.clone());
        threads.push(named("transfer-b", move || {
            transfers(b, a, ("account_b", "account_a"), 60, 29)
        }));
    }
    threads.push(named("cache", || cache_churn(24)));
    for t in threads {
        t.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    mon.join().unwrap();
    let total = *acct_a.lock().unwrap() + *acct_b.lock().unwrap();
    rtlog::info!(
        "app stop total_balance={} elapsed_ms={}",
        total,
        started.elapsed().as_millis() as u32
    );
    let mut t = drain.stop();
    rtlog::Transport::flush(&mut t);
    drop(t);

    // Host side.
    let contract = rtlog_decode::Contract::from_jsonl(&std::fs::read_to_string(&contract_path)?)?;
    let bytes = std::fs::read(&stream_path)?;
    let mut opts = rtlog_decode::Options::default();
    let text = rtlog_decode::decode_with(&contract, &bytes, &opts)?;
    std::fs::write(dir.join("app.txt"), &text.text)?;
    opts.color = true;
    std::fs::write(
        dir.join("app.ansi.txt"),
        rtlog_decode::decode_with(&contract, &bytes, &opts)?.text,
    )?;
    opts.color = false;
    opts.output = rtlog_decode::Output::Json;
    std::fs::write(
        dir.join("app.jsonl"),
        rtlog_decode::decode_with(&contract, &bytes, &opts)?.text,
    )?;
    opts.output = rtlog_decode::Output::Perfetto;
    let perfetto = rtlog_decode::decode_with(&contract, &bytes, &opts)?;
    std::fs::write(dir.join("app.perfetto.json"), &perfetto.text)?;

    let s = rtlog::stats();
    println!(
        "traced {:.0} ms: {} records, {} spans, {} contexts, {} dropped; {} frames, {:.1} KiB on the wire",
        started.elapsed().as_secs_f64() * 1e3,
        text.records,
        text.spans,
        text.contexts,
        text.dropped,
        s.frames,
        bytes.len() as f64 / 1024.0
    );
    println!(
        "deadlock reports: {}",
        text.text.matches("deadlock suspected").count()
    );
    println!(
        "deadline misses:  {}",
        text.text.matches("deadline miss").count()
    );
    println!(
        "slow queries:     {}",
        text.text.matches("slow query").count()
    );
    println!("total balance:    {total} (conserved: {})", total == 2_000);
    println!("files in {}: app.rtlog app.contract.jsonl app.txt app.ansi.txt app.jsonl app.perfetto.json", dir.display());
    println!("open app.perfetto.json in https://ui.perfetto.dev");
    assert_eq!(total, 2_000, "transfers conserve money");
    assert!(text.error.is_none());
    Ok(())
}
