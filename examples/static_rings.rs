//! MCU-style flow, using only the `no_std` API: one static ring per task, an
//! explicit producer per task, an idle-loop drain into a fixed buffer, and a
//! custom clock. Runs on the host so CI can execute it.
//!
//! `cargo run -p rtlog-examples --example static_rings`

use std::sync::atomic::{AtomicU64, Ordering};

use rtlog::drain::{drain_ring, write_ctx_register, write_header};
use rtlog::{Ring, SliceTransport, FRAME_MAX};

static MOTOR_RING: Ring<1024> = Ring::new();
static SENSOR_RING: Ring<512> = Ring::new();
static ISR_RING: Ring<256> = Ring::new();

// Stand-in for a DWT cycle counter at 100 MHz.
static CYCLES: AtomicU64 = AtomicU64::new(0);
fn dwt_now() -> u64 {
    CYCLES.fetch_add(1_250, Ordering::Relaxed)
}

fn main() {
    rtlog::set_clock(dwt_now, 100_000_000);

    let (mut motor, mut motor_c) = MOTOR_RING.split(1).unwrap();
    let (mut sensor, mut sensor_c) = SENSOR_RING.split(2).unwrap();
    let (mut isr, mut isr_c) = ISR_RING.split(0).unwrap();

    // "Tasks" and an "interrupt" log through their own producers.
    for tick in 0..4u16 {
        rtlog::info!(to: &mut motor, "pid out={:.2} err={}", 0.5f32 - f32::from(tick) * 0.1, 4 - tick as i16);
        rtlog::debug!(to: &mut sensor, "adc ch={} raw={:x}", 3u8, 0x0fa0u16 + tick);
        rtlog::trace!(to: &mut isr, "irq {}", tick);
    }
    rtlog::error!(to: &mut sensor, "sensor timeout after {} ms", 25u32);

    // Idle hook: drain every ring into the "UART" buffer.
    let mut uart = [0u8; 4096];
    let mut t = SliceTransport::new(&mut uart);
    let mut scratch = [0u8; FRAME_MAX];
    write_header(&mut t).unwrap();
    write_ctx_register(&mut t, 0, "isr").unwrap();
    write_ctx_register(&mut t, 1, "motor").unwrap();
    write_ctx_register(&mut t, 2, "sensor").unwrap();
    let mut frames = 0;
    frames += drain_ring(&mut motor_c, &mut t, &mut scratch).frames;
    frames += drain_ring(&mut sensor_c, &mut t, &mut scratch).frames;
    frames += drain_ring(&mut isr_c, &mut t, &mut scratch).frames;
    let wire = t.written().to_vec();

    // Host: decode.
    let contract = rtlog_decode::Contract::from_jsonl(&rtlog::contract::to_jsonl()).unwrap();
    let d = rtlog_decode::decode(&contract, &wire, rtlog_decode::Output::Text, false).unwrap();
    print!("{}", d.text);
    println!(
        "\n{frames} frames, {} wire bytes, {:.1} B/frame",
        wire.len(),
        wire.len() as f64 / frames as f64
    );
    assert_eq!(d.records, 13);
}
