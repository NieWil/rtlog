//! Read logs as text while keeping deferred, binary log calls:
//!
//! `RTLOG=debug cargo run -p rtlog-examples --example stdout_mode`

fn main() {
    let _log = rtlog_decode::stdout::init();
    rtlog::info!("service start port={} workers={}", 8080u16, 4u8);
    for i in 0..3u32 {
        let _req = rtlog::span!("request");
        rtlog::debug!("request id={} route={}", i, "/api/orders");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    rtlog::warn!("cache miss ratio={:.2}", 0.37f32);
}
