//! Timing report for the spec's benchmark model (section 10c), as markdown.
//!
//! Per-call latency is measured over batches of calls with the ring drained
//! between batches (outside the timed region), and reported as percentiles of
//! the per-batch mean. A text-formatting baseline shows what the same call
//! costs when it formats on the spot, like `println!` or `env_logger`.
//!
//! `cargo run --release -p rtlog-examples --example timing [-- --out report.md]`

use std::fmt::Write as _;
use std::hint::black_box;
use std::io::Write as _;
use std::time::Instant;

const BATCH: usize = 256;
const BATCHES: usize = 2_000;

struct Stat {
    min: f64,
    p50: f64,
    p99: f64,
    max: f64,
}

fn stats(mut v: Vec<f64>) -> Stat {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
    Stat {
        min: v[0],
        p50: at(0.5),
        p99: at(0.99),
        max: *v.last().unwrap(),
    }
}

/// Time `BATCHES` batches of `BATCH` calls; `after` runs untimed between batches.
fn measure(mut call: impl FnMut(usize), mut after: impl FnMut()) -> Stat {
    for _ in 0..20 {
        for i in 0..BATCH {
            call(i); // warm-up
        }
        after();
    }
    let mut per_call = Vec::with_capacity(BATCHES);
    for _ in 0..BATCHES {
        let t = Instant::now();
        for i in 0..BATCH {
            call(i);
        }
        per_call.push(t.elapsed().as_nanos() as f64 / BATCH as f64);
        after();
    }
    stats(per_call)
}

/// Transport that discards frames, so the report times the logger, not I/O.
struct Discard;

impl rtlog::Transport for Discard {
    fn write(&mut self, _frame: &[u8]) -> Result<(), rtlog::Full> {
        Ok(())
    }
}

fn row(out: &mut String, name: &str, what: &str, s: &Stat) {
    writeln!(
        out,
        "| {name} | {what} | {:.1} | {:.1} | {:.1} | {:.1} |",
        s.min, s.p50, s.p99, s.max
    )
    .unwrap();
}

fn main() {
    let out_path = std::env::args().skip_while(|a| a != "--out").nth(1);
    let drops_before = rtlog::stats().drops;
    let mut d = rtlog::Drain::new(Discard);
    let mut drain = || {
        d.poll().unwrap();
    };

    let mut r = String::new();
    writeln!(r, "## rtlog timing report\n").unwrap();
    writeln!(
        r,
        "{} batches × {} calls, per-call mean of each batch in ns; ring drained between batches. \
         Build: `{}` profile, target `{}`.\n",
        BATCHES,
        BATCH,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        std::env::consts::ARCH
    )
    .unwrap();
    writeln!(r, "| Workload | Call | min | p50 | p99 | max |").unwrap();
    writeln!(r, "| --- | --- | ---: | ---: | ---: | ---: |").unwrap();

    let clock = measure(
        |_| {
            black_box(rtlog::clock::now());
        },
        || {},
    );
    row(
        &mut r,
        "Clock read",
        &format!("`clock::now()` ({:?} source)", rtlog::clock::source()),
        &clock,
    );

    let w1 = measure(|i| rtlog::info!("x={} y={}", i as u32, 7u32), &mut drain);
    row(&mut r, "W1 scalar", "`info!(\"x={} y={}\", u32, u32)`", &w1);

    let w2 = measure(
        |i| {
            rtlog::info!(
                "s a={} b={} c={} d={} e={} f={}",
                i as u32,
                2u16,
                -3i32,
                0.5f32,
                true,
                9u64
            )
        },
        &mut drain,
    );
    row(&mut r, "W2 struct", "6 mixed fields", &w2);

    let s32 = "0123456789abcdef0123456789abcdef";
    let w3 = measure(|_| rtlog::info!("name={}", s32), &mut drain);
    row(&mut r, "W3 string", "32-byte `&str`", &w3);

    let disabled = {
        rtlog::set_level(rtlog::Level::Warn);
        let s = measure(|i| rtlog::debug!("x={}", i as u32), &mut drain);
        rtlog::set_level(rtlog::Level::Trace);
        s
    };
    row(
        &mut r,
        "Disabled",
        "`debug!` below runtime level",
        &disabled,
    );

    let mut buf = String::with_capacity(256);
    let text = measure(
        |i| {
            buf.clear();
            write!(buf, "x={} y={}", black_box(i as u32), black_box(7u32)).unwrap();
            black_box(&buf);
        },
        || {},
    );
    row(
        &mut r,
        "Baseline",
        "`write!` same W1 text into a `String`",
        &text,
    );

    let start = Instant::now();
    let mut line = Vec::with_capacity(BATCH * 64);
    let fmt_io = measure(
        |i| {
            if i == 0 {
                black_box(&line);
                line.clear();
            }
            let ts = start.elapsed();
            writeln!(
                line,
                "{}.{:06} INFO x={} y={}",
                ts.as_secs(),
                ts.subsec_micros(),
                black_box(i as u32),
                7u32
            )
            .unwrap();
        },
        || {},
    );
    row(
        &mut r,
        "Baseline",
        "`writeln!` timestamped W1 line into a buffer",
        &fmt_io,
    );

    // W5 contended: 8 threads logging at once, each with its own ring.
    let threads = 8;
    let start = std::sync::Arc::new(std::sync::Barrier::new(threads));
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                let t = Instant::now();
                for i in 0..2_000u32 {
                    rtlog::info!("x={} y={}", i, 7u32);
                }
                t.elapsed().as_nanos() as f64 / 2_000.0
            })
        })
        .collect();
    let per_thread: Vec<f64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let w5 = stats(per_thread);
    row(
        &mut r,
        "W5 contended",
        "8 threads × 2 000 W1 calls, per-thread mean",
        &w5,
    );
    drain();

    // Throughput: frames through ring + drain per second, single thread.
    let n = 200_000u32;
    let t = Instant::now();
    let mut done = 0;
    while done < n {
        for i in 0..1_000 {
            rtlog::info!("x={} y={}", done + i, 7u32);
        }
        done += 1_000;
        drain();
    }
    let secs = t.elapsed().as_secs_f64();

    // Wire size of one W1 frame.
    let mut probe = rtlog::Drain::new(Vec::new());
    probe.poll().unwrap();
    let base = probe.transport().len();
    rtlog::info!("x={} y={}", 1u32, 7u32);
    probe.poll().unwrap();
    let w1_bytes = probe.transport().len() - base;

    // Decode throughput on a captured W1 stream.
    let mut cap = rtlog::Drain::new(Vec::new());
    cap.poll().unwrap();
    for chunk in 0..50u32 {
        for i in 0..1_000u32 {
            rtlog::info!("x={} y={}", chunk * 1_000 + i, 7u32);
        }
        cap.poll().unwrap();
    }
    let stream = cap.into_inner();
    let contract = rtlog_decode::Contract::from_jsonl(&rtlog::contract::to_jsonl()).unwrap();
    let t = Instant::now();
    let dec = rtlog_decode::decode(&contract, &stream, rtlog_decode::Output::Text, false).unwrap();
    let dsecs = t.elapsed().as_secs_f64().max(1e-9);

    writeln!(r, "\n| Metric | Value |").unwrap();
    writeln!(r, "| --- | ---: |").unwrap();
    writeln!(
        r,
        "| W1 speed-up vs timestamped `writeln!` baseline (p50) | {:.1}× |",
        fmt_io.p50 / w1.p50
    )
    .unwrap();
    writeln!(
        r,
        "| Ring + drain throughput, 1 thread | {:.1} M frames/s |",
        f64::from(n) / secs / 1e6
    )
    .unwrap();
    writeln!(r, "| W1 wire bytes per frame | {w1_bytes} B |").unwrap();
    writeln!(
        r,
        "| W1 rendered text per frame | {} B |",
        dec.text.len() / dec.records.max(1)
    )
    .unwrap();
    writeln!(
        r,
        "| Decode to text | {:.1} M records/s |",
        dec.records as f64 / dsecs / 1e6
    )
    .unwrap();
    writeln!(
        r,
        "| Frames dropped during the run | {} |",
        rtlog::stats().drops - drops_before
    )
    .unwrap();
    writeln!(
        r,
        "\nSpec target (section 11): ≤ 15 ns per W1 call on x86-64; this run: p50 {:.1} ns ({}).",
        w1.p50,
        if w1.p50 <= 15.0 {
            "met"
        } else {
            "not met on this machine"
        }
    )
    .unwrap();

    print!("{r}");
    if let Some(p) = out_path {
        std::fs::write(&p, &r).expect("write report");
    }
}
