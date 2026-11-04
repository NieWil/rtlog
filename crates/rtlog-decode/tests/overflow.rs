mod common;

use common::*;

#[test]
fn burst_beyond_ring_capacity_drops_newest_and_reports_count() {
    // W4 burst: 10 000 calls with no drain running. Each frame is ~16 bytes in
    // the ring, so a 64 KiB thread ring keeps the first few thousand.
    const CALLS: u32 = 10_000;
    for i in 0..CALLS {
        rtlog::info!("burst {} {}", i, 0xabcdu32);
    }
    let pending = rtlog::stats().drops;
    assert!(
        pending > 0,
        "the burst must overflow a {} B ring",
        rtlog::THREAD_RING
    );

    let d = decode_text(&drain_all());
    assert_eq!(d.dropped, pending);
    assert_eq!(d.records as u64 + d.dropped, u64::from(CALLS));
    // DropNewest: what survives is the oldest prefix, in order.
    for (i, m) in messages(&d).iter().enumerate() {
        assert_eq!(m, &format!("burst {i} 43981"));
    }

    // After the drain freed the ring, logging works again.
    rtlog::info!("burst {} {}", 0u32, 0u32);
    assert_eq!(decode_text(&drain_all()).records, 1);
}
