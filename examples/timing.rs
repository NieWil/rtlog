//! Timing report for the spec's benchmark model (section 10c), as markdown.
//!
//! Per-call latency is measured over batches of calls with the ring drained
//! between batches (outside the timed region), and reported as percentiles of
//! the per-batch mean. A text-formatting baseline shows what the same call
//! costs when it formats on the spot, like `println!` or `env_logger`.
//!
//! Costs are reported in nanoseconds and in core cycles. A call's cost is a
//! cycle count; its time is cycles ÷ clock, so cycles compare across machines.
//! The core clock is measured, not read from the OS: a chain of dependent
//! integer adds runs at one add per cycle on every core this targets, so
//! timing a long chain gives cycles per nanosecond.
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

/// Run `n` iterations of 8 dependent adds; returns `Some(8 * n)` adds executed,
/// or `None` on an architecture without the asm loop.
#[allow(unused_variables, unused_mut)]
fn add_chain(n: u64) -> Option<u64> {
    let mut x = 0u64;
    let mut i = n;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        std::arch::asm!(
            "2:",
            "add {x}, 1", "add {x}, 1", "add {x}, 1", "add {x}, 1",
            "add {x}, 1", "add {x}, 1", "add {x}, 1", "add {x}, 1",
            "dec {i}",
            "jnz 2b",
            x = inout(reg) x => _, i = inout(reg) i => _,
            options(nomem, nostack),
        );
        return Some(8 * n);
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        std::arch::asm!(
            "2:",
            "add {x}, {x}, #1", "add {x}, {x}, #1", "add {x}, {x}, #1", "add {x}, {x}, #1",
            "add {x}, {x}, #1", "add {x}, {x}, #1", "add {x}, {x}, #1", "add {x}, {x}, #1",
            "subs {i}, {i}, #1",
            "b.ne 2b",
            x = inout(reg) x => _, i = inout(reg) i => _,
            options(nomem, nostack),
        );
        return Some(8 * n);
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        std::arch::asm!(
            "2:",
            "addi {x}, {x}, 1", "addi {x}, {x}, 1", "addi {x}, {x}, 1", "addi {x}, {x}, 1",
            "addi {x}, {x}, 1", "addi {x}, {x}, 1", "addi {x}, {x}, 1", "addi {x}, {x}, 1",
            "addi {i}, {i}, -1",
            "bnez {i}, 2b",
            x = inout(reg) x => _, i = inout(reg) i => _,
            options(nomem, nostack),
        );
        return Some(8 * n);
    }
    #[allow(unreachable_code)]
    None
}

/// Measured core clock in GHz (cycles per ns): best of several runs.
fn core_ghz() -> Option<f64> {
    let n = 4_000_000u64;
    add_chain(n / 10)?; // warm up the core's clock
    (0..7)
        .map(|_| {
            let t = Instant::now();
            let adds = add_chain(n).unwrap();
            adds as f64 / t.elapsed().as_nanos() as f64
        })
        .reduce(f64::max)
}

fn cpu_model() -> String {
    if let Ok(info) = std::fs::read_to_string("/proc/cpuinfo") {
        for key in ["model name", "Model", "Hardware", "uarch", "isa"] {
            if let Some(v) = info
                .lines()
                .find(|l| l.split(':').next().is_some_and(|k| k.trim() == key))
                .and_then(|l| l.split_once(':'))
            {
                return v.1.trim().to_owned();
            }
        }
    }
    if let Ok(out) = std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !s.is_empty() {
            return s;
        }
    }
    std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".to_owned())
}

/// Transport that discards frames, so the report times the logger, not I/O.
struct Discard;

impl rtlog::Transport for Discard {
    fn write(&mut self, _frame: &[u8]) -> Result<(), rtlog::Full> {
        Ok(())
    }
}

fn row(out: &mut String, ghz: Option<f64>, name: &str, what: &str, s: &Stat) {
    let cycles = ghz.map_or("n/a".to_owned(), |g| format!("{:.0}", s.p50 * g));
    writeln!(
        out,
        "| {name} | {what} | {:.1} | {:.1} | {:.1} | {:.1} | {cycles} |",
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

    let ghz = core_ghz();
    let mut r = String::new();
    writeln!(r, "## rtlog timing report\n").unwrap();
    writeln!(r, "| Platform | |\n| --- | --- |").unwrap();
    writeln!(r, "| CPU | {} |", cpu_model()).unwrap();
    writeln!(
        r,
        "| Measured core clock | {} |",
        ghz.map_or(
            "n/a (no add-chain loop for this architecture)".to_owned(),
            |g| format!("{g:.2} GHz")
        )
    )
    .unwrap();
    writeln!(
        r,
        "| Timestamp counter | {}, {:.1} MHz |",
        match (rtlog::clock::source(), std::env::consts::ARCH) {
            (rtlog::clock::Source::Counter, "x86_64") => "TSC ÷ 2",
            (rtlog::clock::Source::Counter, "aarch64") => "CNTVCT_EL0",
            (rtlog::clock::Source::Counter, _) => "CPU counter",
            (rtlog::clock::Source::Instant, _) => "Instant (ns)",
        },
        f64::from(rtlog::clock::hz()) / 1e6
    )
    .unwrap();
    writeln!(r, "| Target | `{}` |\n", std::env::consts::ARCH).unwrap();
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
    writeln!(
        r,
        "| Workload | Call | min ns | p50 ns | p99 ns | max ns | p50 cycles |"
    )
    .unwrap();
    writeln!(r, "| --- | --- | ---: | ---: | ---: | ---: | ---: |").unwrap();

    let clock = measure(
        |_| {
            black_box(rtlog::clock::now());
        },
        || {},
    );
    row(
        &mut r,
        ghz,
        "Clock read",
        &format!("`clock::now()` ({:?} source)", rtlog::clock::source()),
        &clock,
    );

    let w1 = measure(|i| rtlog::info!("x={} y={}", i as u32, 7u32), &mut drain);
    row(
        &mut r,
        ghz,
        "W1 scalar",
        "`info!(\"x={} y={}\", u32, u32)`",
        &w1,
    );

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
    row(&mut r, ghz, "W2 struct", "6 mixed fields", &w2);

    let s32 = "0123456789abcdef0123456789abcdef";
    let w3 = measure(|_| rtlog::info!("name={}", s32), &mut drain);
    row(&mut r, ghz, "W3 string", "32-byte `&str`", &w3);

    let disabled = {
        rtlog::set_level(rtlog::Level::Warn);
        let s = measure(|i| rtlog::debug!("x={}", i as u32), &mut drain);
        rtlog::set_level(rtlog::Level::Trace);
        s
    };
    row(
        &mut r,
        ghz,
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
        ghz,
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
        ghz,
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
        ghz,
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
    match ghz {
        Some(g) => {
            let cycles = w1.p50 * g;
            writeln!(
                r,
                "\nSpec budget (section 10c): ≤ 50 cycles per W1 call on x86-64 / aarch64. \
                 This run: {cycles:.0} cycles ({:.1} ns at {g:.2} GHz), {}.",
                w1.p50,
                if cycles <= 50.0 { "met" } else { "not met" }
            )
            .unwrap();
        }
        None => writeln!(
            r,
            "\nSpec budget: ≤ 50 cycles per W1 call; core clock not measured on this architecture."
        )
        .unwrap(),
    }

    print!("{r}");
    if let Some(p) = out_path {
        std::fs::write(&p, &r).expect("write report");
    }
}
