//! Long-run soak test (ignored by default; CI runs it for minutes):
//!
//! `RTLOG_SOAK_SECS=120 cargo test --release -p rtlog-decode --test soak -- --ignored --nocapture`
//!
//! Workers are spawned and retired continuously, so thread rings are created
//! and freed for the whole run. The transport decodes every frame as it
//! arrives, so the check costs no memory however long the run lasts:
//!
//! - every frame decodes against the contract (no corruption, no torn frames);
//! - per worker, frames received + frames reported dropped = frames sent;
//! - timestamps never go backwards within a context;
//! - resident memory stays flat (Linux) while contexts churn;
//! - the whole run finishes (a watchdog fails it on a deadlock).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use rtlog_decode::{Contract, Event, Session, Value};

#[derive(Default)]
struct Ctx {
    worker: Option<u64>,
    received: u64,
    dropped: u64,
    next_seq: u64,
    gaps: u64,
    last_ts: u64,
}

#[derive(Default)]
struct Check {
    frames: u64,
    bytes: u64,
    ctxs: HashMap<u32, Ctx>,
    errors: Vec<String>,
}

struct Verifier {
    contract: &'static Contract,
    session: Option<Session<'static>>,
    check: Arc<Mutex<Check>>,
}

impl rtlog::Transport for Verifier {
    fn write(&mut self, frame: &[u8]) -> Result<(), rtlog::Full> {
        let mut c = self.check.lock().unwrap();
        c.bytes += frame.len() as u64;
        let Some(session) = self.session.as_mut() else {
            self.session = Some(Session::new(self.contract, frame, false).expect("valid header"));
            return Ok(());
        };
        c.frames += 1;
        match session.decode_frame(frame) {
            Ok(Event::Record(r)) => {
                let ctx = c.ctxs.entry(r.ctx).or_default();
                if r.ts_ns < ctx.last_ts {
                    let msg = format!(
                        "ctx {} time went back {} -> {}",
                        r.ctx, ctx.last_ts, r.ts_ns
                    );
                    c.errors.push(msg);
                    return Ok(());
                }
                ctx.last_ts = r.ts_ns;
                match (r.site.fmt.as_str(), r.args.as_slice()) {
                    ("soak start worker={}", [Value::U(w)]) => ctx.worker = Some(*w),
                    ("soak worker={} seq={} payload={}", [_, Value::U(seq), _]) => {
                        ctx.received += 1;
                        ctx.gaps += seq - ctx.next_seq;
                        ctx.next_seq = seq + 1;
                    }
                    _ => {}
                }
            }
            Ok(Event::Dropped { ctx, count }) => {
                c.ctxs.entry(ctx).or_default().dropped += u64::from(count)
            }
            Ok(_) => {}
            Err(e) => {
                let msg = format!("frame {}: {e}", c.frames);
                c.errors.push(msg);
            }
        }
        Ok(())
    }
}

fn rss_kib() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4)
}

#[test]
#[ignore = "long-running; set RTLOG_SOAK_SECS and pass --ignored"]
fn soak() {
    let secs: u64 = std::env::var("RTLOG_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(run(Duration::from_secs(secs))).unwrap());
    let report = match rx.recv_timeout(Duration::from_secs(secs + 120)) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("soak run did not finish: deadlock or livelock")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("soak run failed; see the panic above"),
    };
    println!("{report}");
    if let Ok(path) = std::env::var("RTLOG_SOAK_REPORT") {
        std::fs::write(path, &report).unwrap();
    }
}

fn run(duration: Duration) -> String {
    const WORKERS: u64 = 4;
    const PER_GENERATION: u64 = 20_000;
    let contract: &'static Contract = Box::leak(Box::new(
        Contract::from_jsonl(&rtlog::contract::to_jsonl()).unwrap(),
    ));
    let check = Arc::new(Mutex::new(Check::default()));
    let drain = rtlog::spawn_drain(
        Verifier {
            contract,
            session: None,
            check: check.clone(),
        },
        Duration::from_millis(1),
    );

    let stop = Arc::new(AtomicBool::new(false));
    let start = Instant::now();
    let rss_start = rss_kib();
    let mut rss_mid = None;
    let mut sent: HashMap<u64, u64> = HashMap::new();
    let mut next_worker = 0u64;
    let mut live: Vec<std::thread::JoinHandle<(u64, u64)>> = Vec::new();
    while start.elapsed() < duration {
        while live.len() < WORKERS as usize {
            let w = next_worker;
            next_worker += 1;
            let stop = stop.clone();
            live.push(std::thread::spawn(move || {
                // The first frame of a fresh ring always fits, so the worker's
                // context is always identified.
                rtlog::info!("soak start worker={}", w);
                let mut seq = 0u64;
                while seq < PER_GENERATION && !stop.load(Ordering::Relaxed) {
                    rtlog::debug!("soak worker={} seq={} payload={}", w, seq, 0x5a5a_5a5au32);
                    seq += 1;
                    // Alternate bursts (drops expected) with calm periods.
                    if seq.is_multiple_of(2_000) && (seq / 2_000).is_multiple_of(2) {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
                (w, seq)
            }));
        }
        let (done, running): (Vec<_>, Vec<_>) = live.into_iter().partition(|h| h.is_finished());
        live = running;
        for h in done {
            let (w, n) = h.join().unwrap();
            sent.insert(w, n);
        }
        if rss_mid.is_none() && start.elapsed() > duration / 4 {
            rss_mid = rss_kib();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    stop.store(true, Ordering::Relaxed);
    for h in live {
        let (w, n) = h.join().unwrap();
        sent.insert(w, n);
    }
    // Let the drain retire every closed ring before stopping it.
    std::thread::sleep(Duration::from_millis(50));
    drop(drain.stop());
    let elapsed = start.elapsed();
    let rss_end = rss_kib();

    let c = check.lock().unwrap();
    assert!(
        c.errors.is_empty(),
        "decode errors: {:?}",
        &c.errors[..c.errors.len().min(5)]
    );
    let mut received = 0;
    let mut dropped = 0;
    for ctx in c.ctxs.values() {
        let Some(w) = ctx.worker else { continue };
        let n = sent[&w];
        assert_eq!(
            ctx.received + ctx.dropped,
            n,
            "worker {w}: received + dropped != sent"
        );
        assert!(
            ctx.gaps <= ctx.dropped,
            "worker {w}: sequence gaps exceed reported drops"
        );
        received += ctx.received;
        dropped += ctx.dropped;
    }
    let total_sent: u64 = sent.values().sum();
    assert_eq!(received + dropped, total_sent);
    assert_eq!(sent.len() as u64, next_worker);
    let workers_seen = c.ctxs.values().filter(|x| x.worker.is_some()).count() as u64;
    assert_eq!(
        workers_seen, next_worker,
        "every worker context was announced"
    );
    assert_eq!(
        rtlog::stats().contexts,
        0,
        "every retired worker's ring was freed"
    );
    if let (Some(mid), Some(end)) = (rss_mid, rss_end) {
        assert!(
            end < mid + 16 * 1024,
            "RSS grew from {mid} KiB to {end} KiB while contexts churned"
        );
    }

    let rate = c.frames as f64 / elapsed.as_secs_f64();
    let kib = |v: Option<u64>| {
        v.map_or("n/a".to_owned(), |k| {
            format!("{:.1} MiB", k as f64 / 1024.0)
        })
    };
    format!(
        "## rtlog soak test\n\n\
         | Metric | Value |\n| --- | ---: |\n\
         | Duration | {:.1} s |\n\
         | Worker threads started and retired | {} |\n\
         | Frames sent | {} |\n\
         | Frames received and verified | {} |\n\
         | Frames dropped (all reported, all accounted for) | {} ({:.2}%) |\n\
         | Verified frame rate | {:.0} frames/s |\n\
         | Bytes through the transport | {:.1} MiB |\n\
         | RSS at start / after warm-up / at end | {} / {} / {} |\n\
         | Decode errors, time reversals, deadlocks | 0 |\n",
        elapsed.as_secs_f64(),
        next_worker,
        total_sent,
        received,
        dropped,
        100.0 * dropped as f64 / total_sent.max(1) as f64,
        rate,
        c.bytes as f64 / (1024.0 * 1024.0),
        kib(rss_start),
        kib(rss_mid),
        kib(rss_end),
    )
}
