//! Desktop/Linux flow: log from several threads, drain to a file in the
//! background, write the contract, then decode both into text.
//!
//! `cargo run -p rtlog-examples --example basic [out-dir]`

use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&dir)?;
    let stream_path = dir.join("basic.rtlog");
    let contract_path = dir.join("basic.contract.jsonl");

    // 1. Contract: every site in this binary, known before anything runs.
    rtlog::contract::write(&contract_path)?;

    // 2. Opt-in drain thread writing the binary stream to a file.
    let file = std::io::BufWriter::new(std::fs::File::create(&stream_path)?);
    let drain = rtlog::spawn_drain(rtlog::IoTransport(file), Duration::from_millis(1));

    rtlog::set_context_name("main");
    rtlog::info!("starting {} workers", 3u8);
    let workers: Vec<_> = (0..3u8)
        .map(|id| {
            std::thread::Builder::new()
                .name(format!("motor-{id}"))
                .spawn(move || {
                    let mut err = 10i16 * (i16::from(id) + 1);
                    for step in 0..5u32 {
                        let out = f32::from(err) * 0.042;
                        rtlog::debug!("pid step={} out={:.3} err={}", step, out, err);
                        err -= 3;
                    }
                    if id == 2 {
                        rtlog::warn!("motor {} near limit temp={}", id, 81.5f32);
                    }
                })
                .unwrap()
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    rtlog::info!("done, {} contexts", rtlog::stats().contexts);

    let mut transport = drain.stop();
    rtlog::Transport::flush(&mut transport);
    drop(transport);

    // 3. Host side: join stream and contract.
    let contract = rtlog_decode::Contract::from_jsonl(&std::fs::read_to_string(&contract_path)?)?;
    let bytes = std::fs::read(&stream_path)?;
    let decoded = rtlog_decode::decode(&contract, &bytes, rtlog_decode::Output::Text, false)?;
    print!("{}", decoded.text);
    let text_bytes = decoded.text.len();
    println!(
        "\n{} records, {} contexts, {} dropped; stream {} B vs text {} B ({:.1}x smaller)",
        decoded.records,
        decoded.contexts,
        decoded.dropped,
        bytes.len(),
        text_bytes,
        text_bytes as f64 / bytes.len() as f64
    );
    println!(
        "files: {} and {}",
        stream_path.display(),
        contract_path.display()
    );
    assert_eq!(decoded.records, 18);
    assert!(decoded.error.is_none());
    Ok(())
}
