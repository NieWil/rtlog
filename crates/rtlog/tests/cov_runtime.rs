//! The `std` runtime when a transport refuses a control frame, when a thread
//! ring is full, and when a thread logs during its own shutdown. Separate test
//! binary: the drain registry, the span switch and the counters are
//! process-wide, so the tests here run one at a time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use rtlog::wire::{CTRL_CTX_CLOSE, CTRL_CTX_REGISTER, CTRL_CTX_TIME, CTRL_DROPPED, CTRL_RESYNC};
use rtlog::{Drain, Full, Transport};

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Keeps every frame except the control frames with ID `refuse`.
#[derive(Default)]
struct Picky {
    out: Vec<u8>,
    refuse: Option<u32>,
}

impl Transport for Picky {
    fn write(&mut self, f: &[u8]) -> Result<(), Full> {
        if self
            .refuse
            .is_some_and(|id| f.starts_with(&id.to_le_bytes()))
        {
            return Err(Full);
        }
        self.out.extend_from_slice(f);
        Ok(())
    }
}

fn drain() -> Drain<Picky> {
    let mut d = Drain::new(Picky::default());
    d.poll().unwrap(); // header, and whatever earlier tests left
    d
}

fn refuse(d: &mut Drain<Picky>, id: Option<u32>) {
    d.transport_mut().refuse = id;
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

fn site(fmt: &str) -> [u8; 4] {
    rtlog::site::sites()
        .iter()
        .find(|s| s.fmt == fmt)
        .unwrap_or_else(|| panic!("no site {fmt:?}"))
        .id
        .to_le_bytes()
}

/// Run `f` on a new thread named `name` and wait for it to end.
fn on_thread(name: &str, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn a_refused_ctx_register_is_retried_on_the_next_poll() {
    let _s = serial();
    let mut d = drain();
    on_thread("announce-me", || {
        for i in 0..3u32 {
            rtlog::info!("announced {}", i);
        }
    });
    refuse(&mut d, Some(CTRL_CTX_REGISTER));
    let before = d.transport().out.len();
    assert_eq!(d.poll(), Err(Full));
    assert_eq!(d.transport().out.len(), before, "no frame before its name");

    refuse(&mut d, None);
    assert!(d.poll().unwrap() >= 3);
    let out = &d.transport().out;
    assert_eq!(count(out, b"announce-me"), 1, "the name, once");
    assert_eq!(count(out, &site("announced {}")), 3);
}

#[test]
fn a_refused_ctx_close_keeps_the_context_until_it_is_written() {
    let _s = serial();
    let mut d = drain();
    let live = rtlog::stats().contexts;
    on_thread("closing", || rtlog::info!("last words {}", 1u8));
    assert_eq!(rtlog::stats().contexts, live + 1);
    refuse(&mut d, Some(CTRL_CTX_CLOSE));
    let before = d.transport().out.len();
    assert_eq!(d.poll(), Err(Full));
    assert_eq!(count(&d.transport().out, &site("last words {}")), 1);
    assert_eq!(rtlog::stats().contexts, live + 1, "not retired yet");

    refuse(&mut d, None);
    d.poll().unwrap();
    let out = &d.transport().out[before..];
    assert_eq!(count(out, &CTRL_CTX_CLOSE.to_le_bytes()), 1);
    assert_eq!(
        rtlog::stats().contexts,
        live,
        "retired after its close frame"
    );
}

#[test]
fn a_refused_drop_report_keeps_the_count_and_the_frames() {
    let _s = serial();
    let mut d = drain();
    const N: u32 = 10_000; // ~150 KB: more than the 64 KiB ring holds
    let drops_before = rtlog::stats().drops;
    on_thread("flood", || {
        for i in 0..N {
            rtlog::info!("flood {}", i);
        }
    });
    let dropped = rtlog::stats().drops - drops_before;
    assert!(dropped > 0, "the ring overflowed");

    refuse(&mut d, Some(CTRL_DROPPED));
    assert_eq!(d.poll(), Err(Full));
    assert_eq!(
        count(&d.transport().out, &site("flood {}")),
        0,
        "frames wait behind the report"
    );

    refuse(&mut d, None);
    d.poll().unwrap();
    let out = &d.transport().out;
    let at = out
        .windows(4)
        .position(|w| w == CTRL_DROPPED.to_le_bytes())
        .expect("a dropped frame");
    let (_, n) = rtlog::varint::decode(&out[at + 4..]).unwrap();
    let reported = u32::from_le_bytes(out[at + 4 + n..at + 8 + n].try_into().unwrap());
    assert_eq!(
        u64::from(reported),
        dropped,
        "the whole count, reported once"
    );
    assert_eq!(count(out, &site("flood {}")) as u64 + dropped, u64::from(N));
}

#[test]
fn a_refused_resync_or_ctx_time_is_retried_on_the_next_poll() {
    let _s = serial();
    let mut d = drain();
    // Push more than RESYNC_EVERY bytes through this drain.
    let start = d.transport().out.len();
    while d.transport().out.len() - start < rtlog::wire::RESYNC_EVERY {
        on_thread("bulk", || {
            for i in 0..2_000u32 {
                rtlog::debug!("bulk {}", i);
            }
        });
        d.poll().unwrap();
    }
    // A context with frames waiting while the resync is refused.
    on_thread("after-gap", || rtlog::info!("after gap {}", 1u8));
    refuse(&mut d, Some(CTRL_RESYNC));
    let before = d.transport().out.len();
    assert_eq!(d.poll(), Err(Full));
    assert_eq!(d.transport().out.len(), before, "nothing else goes out");

    // The resync goes out; the per-context clock after it is refused.
    refuse(&mut d, Some(CTRL_CTX_TIME));
    assert_eq!(d.poll(), Err(Full));
    let out = &d.transport().out[before..];
    assert!(out.starts_with(&rtlog::wire::RESYNC_MARKER), "resync first");
    assert_eq!(count(out, &site("after gap {}")), 0);

    refuse(&mut d, None);
    d.poll().unwrap();
    let out = &d.transport().out[before..];
    assert_eq!(count(out, &CTRL_RESYNC.to_le_bytes()), 1, "one resync");
    assert!(count(out, &CTRL_CTX_TIME.to_le_bytes()) >= 1);
    assert_eq!(count(out, b"after-gap"), 1, "names are announced again");
    assert_eq!(count(out, &site("after gap {}")), 1);
}

#[test]
fn spans_on_a_full_ring_or_with_spans_off_are_inert() {
    let _s = serial();
    let mut d = drain();
    on_thread("spans", || {
        let g = rtlog::span!("recorded");
        assert_ne!(g.id(), 0);
        assert_eq!(g.id() & 0xffff_ffff, 1, "first span on this thread");
        drop(g);

        let drops = rtlog::stats().drops;
        while rtlog::stats().drops == drops {
            rtlog::info!("fill {}", 0u64);
        }
        let drops = rtlog::stats().drops;
        let g = rtlog::span!("no room");
        assert_eq!(g.id(), 0, "its open frame did not fit");
        assert_eq!(rtlog::stats().drops, drops + 1, "and was counted");
        drop(g);
        assert_eq!(
            rtlog::stats().drops,
            drops + 1,
            "an inert guard writes nothing"
        );

        rtlog::set_spans(false);
        let g = rtlog::span!("switched off");
        rtlog::set_spans(true);
        assert_eq!(g.id(), 0);
    });
    let _ = d.poll();
    let sites = rtlog::site::sites();
    let id_of = |name: &str| sites.iter().find(|s| s.fmt == name).unwrap().id;
    let out = &d.transport().out;
    assert_eq!(count(out, &id_of("recorded").to_le_bytes()), 1);
    assert_eq!(count(out, &id_of("no room").to_le_bytes()), 0);
    assert_eq!(count(out, &id_of("switched off").to_le_bytes()), 0);
}

/// Logs from its destructor, which runs after rtlog's own thread-local state
/// is gone (thread-local destructors run in reverse order of registration).
struct LogsOnDrop;

static LATE_RAN: AtomicBool = AtomicBool::new(false);

impl Drop for LogsOnDrop {
    fn drop(&mut self) {
        rtlog::info!("late {}", 1u8);
        LATE_RAN.store(true, Ordering::Release);
    }
}

thread_local! {
    static LATE: LogsOnDrop = const { LogsOnDrop };
}

// The destructor order this relies on is the one std guarantees on Linux.
#[cfg(target_os = "linux")]
#[test]
fn a_log_call_after_the_thread_state_is_gone_is_ignored() {
    let _s = serial();
    let mut d = drain();
    let drops = rtlog::stats().drops;
    on_thread("ending", || {
        LATE.with(|_| {}); // registered first, so destroyed last
        rtlog::info!("early {}", 1u8);
    });
    assert!(LATE_RAN.load(Ordering::Acquire), "the late call returned");
    assert_eq!(
        rtlog::stats().drops,
        drops,
        "ignored, not counted as a drop"
    );
    d.poll().unwrap();
    let out = &d.transport().out;
    assert_eq!(count(out, &site("early {}")), 1);
    assert_eq!(count(out, &site("late {}")), 0);
}
