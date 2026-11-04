mod common;

use common::*;

#[test]
fn each_thread_gets_a_context_and_order_is_kept_per_thread() {
    const THREADS: u32 = 8;
    const PER: u32 = 500;
    let drain = rtlog::spawn_drain(Vec::new(), std::time::Duration::from_millis(1));
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            std::thread::Builder::new()
                .name(format!("worker-{t}"))
                .spawn(move || {
                    for i in 0..PER {
                        rtlog::info!("thread {} seq {}", t, i);
                    }
                })
                .unwrap()
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let bytes = drain.stop();

    let d = rtlog_decode::decode(&contract(), &bytes, rtlog_decode::Output::Json, false).unwrap();
    assert_eq!(d.error, None);
    assert_eq!(d.dropped, 0, "drain keeps up with 8 x 500 frames");
    assert_eq!(d.records as u32, THREADS * PER);
    assert_eq!(d.contexts as u32, THREADS);

    let mut next = vec![0u64; THREADS as usize];
    for line in d.text.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let t = v["args"][0].as_u64().unwrap() as usize;
        assert_eq!(v["ctx_name"], format!("worker-{t}"));
        assert_eq!(v["args"][1].as_u64().unwrap(), next[t], "per-thread order");
        next[t] += 1;
    }

    let s = rtlog::stats();
    assert!(s.frames >= u64::from(THREADS * PER));
    assert_eq!(
        s.contexts, 0,
        "closed thread rings are released after draining"
    );
}
