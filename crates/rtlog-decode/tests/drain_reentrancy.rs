//! Regression test: a transport that logs or reads stats while the drain is
//! inside `poll` must not deadlock, and threads starting during a poll must
//! register without waiting for it.

mod common;

use std::sync::mpsc;
use std::time::Duration;

struct Chatty(Vec<u8>, u32);

impl rtlog::Transport for Chatty {
    fn write(&mut self, f: &[u8]) -> Result<(), rtlog::Full> {
        self.1 += 1;
        if self.1.is_multiple_of(16) {
            // Logging from inside the transport: the drain thread's own ring.
            rtlog::debug!("transport wrote frames={}", self.1);
            let _ = rtlog::stats();
            // A brand-new thread registering while poll() holds its locks.
            std::thread::spawn(|| rtlog::info!("late thread {}", 1u8))
                .join()
                .unwrap();
        }
        self.0.extend_from_slice(f);
        Ok(())
    }
}

#[test]
fn transport_can_log_and_threads_can_register_during_poll() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let drain = rtlog::spawn_drain(Chatty(Vec::new(), 0), Duration::from_millis(1));
        for i in 0..200u32 {
            rtlog::info!("main {}", i);
        }
        std::thread::sleep(Duration::from_millis(50));
        let out = drain.stop();
        tx.send(out.0).unwrap();
    });
    let bytes = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("drain deadlocked");
    let d = common::decode_text(&bytes);
    assert!(d.records >= 200);
    assert!(d.text.contains("late thread 1"));
    assert!(d.text.contains("transport wrote frames="));
}
