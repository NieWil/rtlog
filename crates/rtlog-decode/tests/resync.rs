//! A corrupted stretch of a stream costs only the frames inside it: the drain
//! writes resync frames, and the decoder skips to the next one and carries on
//! with exact clocks. The tests share the process-wide runtime (every drain
//! consumes every thread's ring), so they run one at a time.

use rtlog_decode::{decode, Contract, Output};

static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn contract() -> Contract {
    Contract::from_jsonl(&rtlog::contract::to_jsonl()).unwrap()
}

/// A capture with several resync frames in it: enough rounds of logging and
/// draining to pass `RESYNC_EVERY` bytes more than once.
fn capture() -> Vec<u8> {
    rtlog::set_context_name("resync-test");
    let mut drain = rtlog::Drain::new(Vec::new());
    let mut n = 0u32;
    for _ in 0..9 {
        for _ in 0..1_800 {
            rtlog::info!("sample n={} v={}", n, n.wrapping_mul(2_654_435_761));
            n += 1;
        }
        drain.poll().unwrap();
    }
    rtlog::info!("last record {}", n);
    drain.poll().unwrap();
    drain.into_inner()
}

fn markers(bytes: &[u8]) -> Vec<usize> {
    bytes
        .windows(8)
        .enumerate()
        .filter(|(_, w)| *w == rtlog::wire::RESYNC_MARKER)
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn a_corrupt_stretch_costs_only_the_frames_inside_it() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let bytes = capture();
    let m = markers(&bytes);
    assert!(m.len() >= 2, "the drain wrote {} resync frames", m.len());
    let c = contract();

    let clean = decode(&c, &bytes, Output::Json, false).unwrap();
    assert_eq!(clean.error, None);
    assert_eq!((clean.gaps, clean.skipped_bytes), (0, 0));
    let last = |text: &str| -> serde_json::Value {
        serde_json::from_str(text.lines().last().unwrap()).unwrap()
    };
    let clean_last = last(&clean.text);
    assert_eq!(clean_last["message"], "last record 16200");

    // Trash 80 bytes between the first two resync frames.
    let mut bad = bytes.clone();
    let from = (m[0] + m[1]) / 2;
    bad[from..from + 80].fill(0xFF);

    let d = decode(&c, &bad, Output::Json, false).unwrap();
    assert_eq!(d.error, None, "decoding went on past the damage");
    assert_eq!(d.gaps, 1);
    assert!(d.skipped_bytes > 0 && d.skipped_bytes < m[1] - m[0] + 80);
    assert!(
        d.records < clean.records && d.records > clean.records / 2,
        "{} of {} records survive",
        d.records,
        clean.records
    );
    // The end of the stream decodes, and at the exact same time as without the
    // damage: each resync restores the context's clock.
    let bad_last = last(&d.text);
    assert_eq!(bad_last["message"], "last record 16200");
    assert_eq!(bad_last["ts_ns"], clean_last["ts_ns"], "clock restored");
    // The gap is in the output, in every format.
    assert!(d.text.lines().any(|l| l.contains("\"type\":\"gap\"")));
    let text = decode(&c, &bad, Output::Text, false).unwrap();
    assert!(text.text.contains("GAP") && text.text.contains("[stream]"));
    let trace = decode(&c, &bad, Output::Perfetto, false).unwrap();
    assert!(trace.text.contains("stream gap"));
    serde_json::from_str::<serde_json::Value>(&trace.text).unwrap();
}

#[test]
fn without_a_resync_frame_after_the_damage_decoding_still_ends_there() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let bytes = capture();
    let m = markers(&bytes);
    let c = contract();
    // Damage with no resync frame after it (the stream ends before the second
    // one): nothing to recover at, so decoding ends there, as it always did.
    let mut bad = bytes[..m[1]].to_vec();
    let at = (m[0] + m[1]) / 2;
    bad[at..at + 80].fill(0xFF);
    let d = decode(&c, &bad, Output::Text, false).unwrap();
    assert!(d.error.is_some(), "stopped at the damage");
    assert_eq!(d.gaps, 0);

    // And recovery can be switched off: the first bad frame ends decoding.
    let mut worse = bytes.clone();
    let from = (m[0] + m[1]) / 2;
    worse[from..from + 80].fill(0xFF);
    let mut dec = rtlog_decode::Decoder::new(&c, &worse, false)
        .unwrap()
        .recovering(false);
    let errors = dec.by_ref().filter(Result::is_err).count();
    assert_eq!(errors, 1);
    assert_eq!(dec.last_skip, None);
}
