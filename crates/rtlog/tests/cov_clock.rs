//! Clock selection and a custom clock. Separate test binary with one test:
//! the clock is process-wide and the default source is fixed on first use.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rtlog::clock::{self, Source};
use rtlog::wire::Header;

static TICKS: AtomicU64 = AtomicU64::new(1_000);

/// A 32 768 Hz clock that advances 10 ticks per read.
fn fake_ticks() -> u64 {
    TICKS.fetch_add(10, Ordering::Relaxed)
}

fn delta_of(frame: &[u8]) -> u64 {
    rtlog::varint::decode(&frame[4..]).unwrap().0
}

#[test]
fn rtlog_clock_instant_then_a_custom_clock() {
    // Before anything reads the clock: ask for the portable clock.
    std::env::set_var("RTLOG_CLOCK", "instant");
    assert_eq!(clock::source(), Source::Instant);
    assert_eq!(clock::hz(), 1_000_000_000, "nanoseconds");
    let a = clock::now();
    std::thread::sleep(Duration::from_millis(5));
    let b = clock::now();
    assert!(b - a >= 5_000_000, "{} ns for a 5 ms sleep", b - a);
    std::env::remove_var("RTLOG_CLOCK");
    assert_eq!(clock::source(), Source::Instant, "fixed on first use");

    // A custom clock replaces the default for timestamps and the header.
    rtlog::set_clock(fake_ticks, 32_768);
    assert_eq!(clock::hz(), 32_768);
    assert_eq!(clock::now(), 1_000);
    assert_eq!(clock::now(), 1_010);

    static RING: rtlog::Ring<256> = rtlog::Ring::new();
    let (mut p, mut c) = RING.split(1).unwrap();
    assert!(p.push(5, &[])); // stamped 1 020
    assert!(p.push(6, &[])); // stamped 1 030
    let mut out = [0u8; rtlog::FRAME_MAX];
    let n = c.pop(&mut out).unwrap();
    assert_eq!(delta_of(&out[..n]), 1_020, "first frame: ticks since 0");
    let n = c.pop(&mut out).unwrap();
    assert_eq!(delta_of(&out[..n]), 10);

    let mut v = Vec::new();
    rtlog::drain::write_header(&mut v).unwrap();
    assert_eq!(Header::decode(&v).unwrap().clock_hz, 32_768);
}
