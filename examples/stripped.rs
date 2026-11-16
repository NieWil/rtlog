//! A program built to ship without its format strings (the `strip` feature).
//!
//! It only logs and drains to a file. It does not write its contract: with
//! `strip` the running program cannot read its descriptors, so the contract
//! is read from the linked binary before the descriptor sections are removed.
//! `scripts/strip_e2e.sh` does the whole flow and [docs/strip.md] explains it:
//!
//! ```sh
//! RUSTFLAGS="-C link-arg=-fuse-ld=bfd -C link-arg=-Wl,-T,$PWD/c/rtlog_strip.ld" \
//!   cargo build --release -p rtlog-examples --example stripped --features strip
//! rtlog contract --elf target/release/examples/stripped -o stripped.contract.jsonl
//! objcopy -R rtlog_strs -R linkme_SITES -R linkm2_SITES target/release/examples/stripped shipped
//! rtlog verify --contract stripped.contract.jsonl shipped
//! ./shipped out.rtlog && rtlog decode --contract stripped.contract.jsonl out.rtlog
//! ```
//!
//! [docs/strip.md]: ../docs/strip.md

use std::{fs::File, io::BufWriter, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "stripped.rtlog".into());
    let file = BufWriter::new(File::create(&path)?);
    let drain = rtlog::spawn_drain(rtlog::IoTransport(file), Duration::from_millis(5));

    rtlog::set_context_name("main");
    for (i, rpm) in [1200u32, 1450, 1710].into_iter().enumerate() {
        let _step = rtlog::span!("stripped.control_step");
        rtlog::info!("stripped motor step={} speed={} rpm", i as u8, rpm);
    }
    rtlog::warn!("stripped temperature={} C over limit={}", 81.5f32, 80u8);

    let mut out = drain.stop();
    rtlog::Transport::flush(&mut out);
    Ok(())
}
