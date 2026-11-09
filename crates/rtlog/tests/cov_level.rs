//! The runtime level filter. Separate test binary with one test: the filter
//! is process-wide.

use rtlog::Level;

const ALL: [Level; 5] = [
    Level::Trace,
    Level::Debug,
    Level::Info,
    Level::Warn,
    Level::Error,
];

#[test]
fn the_runtime_level_filters_sites_and_reads_back() {
    assert_eq!(
        rtlog::level::level(),
        Some(Level::Trace),
        "everything by default"
    );
    for min in ALL {
        rtlog::set_level(min);
        assert_eq!(rtlog::level::level(), Some(min));
        for l in ALL {
            let compiled_in = !rtlog::level::STATIC_MAX_OFF && l >= rtlog::level::STATIC_MIN;
            assert_eq!(
                rtlog::enabled(l),
                compiled_in && l >= min,
                "{l:?} at {min:?}"
            );
        }
        assert!(rtlog::spans_enabled(), "the level does not stop spans");
    }

    // A producer records only what passes the filter.
    static RING: rtlog::Ring<256> = rtlog::Ring::new();
    let (mut p, mut c) = RING.split(1).unwrap();
    rtlog::set_level(Level::Warn);
    rtlog::info!(to: &mut p, "filtered {}", 1u8);
    rtlog::warn!(to: &mut p, "kept {}", 2u8);
    let mut out = [0u8; rtlog::FRAME_MAX];
    assert!(c.pop(&mut out).is_some());
    assert!(c.pop(&mut out).is_none(), "the info record was filtered");

    rtlog::set_off();
    assert_eq!(rtlog::level::level(), None);
    assert!(ALL.iter().all(|&l| !rtlog::enabled(l)));
    assert!(!rtlog::spans_enabled(), "off stops spans too");
    rtlog::error!(to: &mut p, "off {}", 3u8);
    assert!(c.pop(&mut out).is_none());

    rtlog::set_level(Level::Trace);
    assert!(rtlog::enabled(Level::Trace) && rtlog::spans_enabled());
}
