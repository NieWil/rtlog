//! A program that never installs a drain must not keep a 64 KiB ring per
//! thread it ever ran (spec section 0: a library crate is "silent if none").
//! Separate test binary: no other test here may start a drain.

use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn rings_of_exited_threads_are_released_without_a_drain() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let before = rtlog::stats().contexts;
    for round in 0..50u32 {
        let handles: Vec<_> = (0..8u8)
            .map(|i| {
                std::thread::spawn(move || {
                    rtlog::info!("round={} worker={}", round, i);
                    CALLS.fetch_add(1, Ordering::Relaxed);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
    assert_eq!(CALLS.load(Ordering::Relaxed), 400);
    // Only the last few finished threads' rings are kept (for a crash dump);
    // the rest were released as the threads ended.
    assert!(
        rtlog::stats().contexts <= before + rtlog::KEEP_WITHOUT_DRAIN,
        "{} live contexts",
        rtlog::stats().contexts
    );
}

#[test]
fn spans_follow_their_own_switch_not_the_level() {
    static RING: rtlog::Ring<1024> = rtlog::Ring::new();
    let (mut p, mut c) = RING.split(7).unwrap();
    let mut out = [0u8; rtlog::FRAME_MAX];

    rtlog::set_level(rtlog::Level::Error);
    let s = rtlog::span!(to: &mut p, "kept at warn level");
    assert_ne!(s.id, 0, "spans are recorded whatever the level");
    assert_eq!(s.id >> 32, 7, "the full 32-bit context is in the span id");
    p.span_exit(s);
    assert!(c.pop(&mut out).is_some());
    assert!(c.pop(&mut out).is_some());

    rtlog::set_spans(false);
    let s = rtlog::span!(to: &mut p, "removed by set_spans");
    assert_eq!(s.id, 0);
    p.span_exit(s);
    assert!(c.pop(&mut out).is_none());

    rtlog::set_spans(true);
    rtlog::set_off();
    let s = rtlog::span!(to: &mut p, "off stops spans too");
    assert_eq!(s.id, 0);
    rtlog::set_level(rtlog::Level::Trace);
}

/// Without a drain the most recently *finished* rings are kept, whatever the
/// order the threads were created in: a long-lived worker that exits last is
/// the one a crash dump most wants to show.
#[test]
fn the_last_finished_threads_are_the_ones_kept() {
    use std::sync::mpsc;
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    // Created first, finishes last.
    let long_lived = std::thread::Builder::new()
        .name("long-lived".into())
        .spawn(move || {
            rtlog::info!("long-lived starts {}", 1u8);
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        })
        .unwrap();
    started_rx.recv().unwrap();
    for i in 0..(rtlog::KEEP_WITHOUT_DRAIN + 8) {
        std::thread::Builder::new()
            .name(format!("short-{i}"))
            .spawn(move || rtlog::info!("short lived {}", i as u32))
            .unwrap()
            .join()
            .unwrap();
    }
    release_tx.send(()).unwrap();
    long_lived.join().unwrap();

    let names = rtlog::__context_names();
    assert!(names.iter().any(|n| n == "long-lived"), "kept: {names:?}");
    assert!(
        !names.iter().any(|n| n == "short-0"),
        "the oldest finished is evicted: {names:?}"
    );
    assert!(names
        .iter()
        .any(|n| n == &format!("short-{}", rtlog::KEEP_WITHOUT_DRAIN + 7)));
}
