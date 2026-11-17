//! Instructions per log call, for a deterministic regression gate.
//!
//! Each workload is one function that makes `CALLS` log calls. Run under
//! Valgrind's callgrind with `--toggle-collect=<function>`, only the
//! instructions executed inside that function (and what it calls) are
//! counted, so the count does not depend on the machine's load or on program
//! start-up. `scripts/icount.py` runs every workload and divides by the
//! number of calls:
//!
//! ```sh
//! python3 scripts/icount.py            # needs valgrind
//! ```
//!
//! The clock is a counter, so no time source is read, and the rings are
//! emptied between batches outside the measured functions.

use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};

use rtlog::{Level, Producer, Ring, FRAME_MAX};

/// Log calls per batch (one call of a workload function).
const CALLS: u32 = 1_000;
/// Batches per run.
const BATCHES: u32 = 50;
const RING: usize = 1 << 16;

static TICKS: AtomicU64 = AtomicU64::new(0);

/// A clock that only counts: the same instructions on every call.
fn ticks() -> u64 {
    TICKS.fetch_add(1, Ordering::Relaxed)
}

/// Two `u32` arguments through an explicit producer (the `no_std` path).
#[export_name = "icount_w1"]
#[inline(never)]
fn w1(p: &mut Producer<'static, RING>) {
    for i in 0..CALLS {
        rtlog::info!(to: p, "x={} y={}", black_box(i), black_box(i ^ 0x55));
    }
}

/// No argument.
#[export_name = "icount_noargs"]
#[inline(never)]
fn noargs(p: &mut Producer<'static, RING>) {
    for _ in 0..CALLS {
        rtlog::info!(to: p, "tick");
    }
}

/// One seven-byte string.
#[export_name = "icount_str"]
#[inline(never)]
fn string(p: &mut Producer<'static, RING>) {
    for _ in 0..CALLS {
        rtlog::info!(to: p, "state={}", black_box("running"));
    }
}

/// A call below the runtime level: the cost of logging that is switched off.
#[export_name = "icount_disabled"]
#[inline(never)]
fn disabled(p: &mut Producer<'static, RING>) {
    for i in 0..CALLS {
        rtlog::debug!(to: p, "x={} y={}", black_box(i), black_box(i ^ 0x55));
    }
}

/// A span opened and closed.
#[export_name = "icount_span"]
#[inline(never)]
fn span(p: &mut Producer<'static, RING>) {
    for _ in 0..CALLS {
        let s = rtlog::span!(to: p, "step");
        p.span_exit(s);
    }
}

/// Two `u32` arguments into the calling thread's own ring (the `std` path).
#[export_name = "icount_local_w1"]
#[inline(never)]
fn local_w1() {
    for i in 0..CALLS {
        rtlog::info!("x={} y={}", black_box(i), black_box(i ^ 0x55));
    }
}

fn main() {
    static RING_A: Ring<RING> = Ring::new();
    let which = std::env::args().nth(1).unwrap_or_default();
    rtlog::set_clock(ticks, 1_000_000);
    rtlog::set_level(Level::Info);
    let (mut p, mut c) = RING_A.split(1).expect("the ring is split once");
    let mut scratch = [0u8; FRAME_MAX];
    let mut drain = rtlog::Drain::new(rtlog::IoTransport(std::io::sink()));
    // The thread's ring exists before the measured calls.
    rtlog::info!("warm-up {}", 0u8);

    let mut frames = 0u64;
    for _ in 0..BATCHES {
        match which.as_str() {
            "w1" => w1(&mut p),
            "noargs" => noargs(&mut p),
            "str" => string(&mut p),
            "disabled" => disabled(&mut p),
            "span" => span(&mut p),
            "local_w1" => local_w1(),
            _ => {
                eprintln!("usage: icount w1|noargs|str|disabled|span|local_w1");
                std::process::exit(2);
            }
        }
        while c.pop(&mut scratch).is_some() {
            frames += 1;
        }
        drain.poll().expect("the sink accepts everything");
    }
    // Every call was recorded: nothing was dropped for lack of room.
    println!(
        "{which}: {} calls, {frames} frames popped, {} dropped",
        CALLS * BATCHES,
        u64::from(RING_A.drops()) + rtlog::stats().drops
    );
}
