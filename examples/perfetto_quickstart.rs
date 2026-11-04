//! The shortest path from log calls to a Perfetto timeline.
//!
//! `cargo run --release -p rtlog-examples --example perfetto_quickstart`
//! then drag `target/quickstart.perfetto.json` onto <https://ui.perfetto.dev>.

use std::time::Duration;

fn worker(id: u32) {
    for job in 0..20u32 {
        let _job = rtlog::span!("job"); // a slice on this thread's track
        rtlog::info!("job id={} queue_depth={}", job, 20 - job); // `queue_depth` becomes a counter track
        {
            let _io = rtlog::span!("io");
            std::thread::sleep(Duration::from_micros(200 + u64::from(id * 50 + job * 10)));
        }
        if job % 7 == 6 {
            rtlog::warn!("retry job={} worker={}", job, id); // a marker on the track
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Capture: log from a few threads into memory.
    let drain = rtlog::spawn_drain(Vec::new(), Duration::from_millis(1));
    let threads: Vec<_> = (0..3)
        .map(|id| {
            std::thread::Builder::new()
                .name(format!("worker-{id}"))
                .spawn(move || worker(id))
                .unwrap()
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let capture = drain.stop();

    // 2. Decode on the "host" side: here, the same process.
    let contract = rtlog_decode::Contract::from_jsonl(&rtlog::contract::to_jsonl())?;
    let opts = rtlog_decode::Options {
        output: rtlog_decode::Output::Perfetto,
        ..Default::default()
    };
    let trace = rtlog_decode::decode_with(&contract, &capture, &opts)?;
    // Output path: first argument, or target/quickstart.perfetto.json.
    let out = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "target/quickstart.perfetto.json".into());
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&out, &trace.text)?;
    println!(
        "{} records, {} spans from {} threads -> {}",
        trace.records,
        trace.spans,
        trace.contexts,
        out.display()
    );
    println!("open it in https://ui.perfetto.dev");
    Ok(())
}
