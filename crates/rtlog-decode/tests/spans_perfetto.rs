mod common;

use common::*;
use rtlog_decode::{decode_with, Options, Output};

fn handle(id: u32) {
    let _req = rtlog::span!("request");
    rtlog::info!("request id={} bytes={}", id, 512u32);
    {
        let _p = rtlog::span!("parse");
        rtlog::debug!("parsed fields={}", 4u8);
    }
    {
        let _db = rtlog::span!("db.query");
        std::thread::sleep(std::time::Duration::from_millis(1));
        rtlog::debug!("rows={} latency_us={}", 3u32, 950u32);
    }
}

#[test]
fn spans_render_as_text_json_and_perfetto_slices() {
    std::thread::Builder::new()
        .name("worker-0".into())
        .spawn(|| {
            handle(1);
            handle(2);
        })
        .unwrap()
        .join()
        .unwrap();
    let bytes = drain_all();
    let c = contract();

    // Text: open/close lines, nesting by indentation, durations on close.
    let text = decode_with(&c, &bytes, &Options::default()).unwrap();
    assert_eq!(text.error, None);
    assert_eq!((text.records, text.spans), (6, 6));
    let lines: Vec<&str> = text.text.lines().collect();
    assert!(lines[0].ends_with("SPAN  ▶ request"), "{}", lines[0]);
    assert!(
        lines[1].contains("INFO    request id=1 bytes=512"),
        "{}",
        lines[1]
    );
    assert!(lines[2].ends_with("SPAN    ▶ parse"), "{}", lines[2]);
    assert!(lines
        .iter()
        .any(|l| l.contains("◀ db.query") && (l.ends_with(" ms") || l.ends_with(" µs"))));

    // JSON: one finished span object per span, parented correctly.
    let json = decode_with(
        &c,
        &bytes,
        &Options {
            output: Output::Json,
            ..Options::default()
        },
    )
    .unwrap();
    let objs: Vec<serde_json::Value> = json
        .text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let spans: Vec<_> = objs.iter().filter(|o| o["type"] == "span").collect();
    assert_eq!(spans.len(), 6);
    let db = spans.iter().find(|s| s["name"] == "db.query").unwrap();
    let req = spans.iter().find(|s| s["name"] == "request").unwrap();
    assert_eq!(db["parent"], req["id"]);
    assert!(
        db["dur_ns"].as_u64().unwrap() >= 1_000_000,
        "sleep(1 ms) inside db.query"
    );

    // Perfetto: thread names, balanced B/E per thread, counters from `key={}`.
    let p = decode_with(
        &c,
        &bytes,
        &Options {
            output: Output::Perfetto,
            ..Options::default()
        },
    )
    .unwrap();
    let trace: serde_json::Value = serde_json::from_str(&p.text).unwrap();
    let ev = trace["traceEvents"].as_array().unwrap();
    assert!(ev
        .iter()
        .any(|e| e["ph"] == "M" && e["args"]["name"] == "worker-0"));
    let b = ev.iter().filter(|e| e["ph"] == "B").count();
    let e = ev.iter().filter(|e| e["ph"] == "E").count();
    assert_eq!((b, e), (6, 6));
    let mut depth = 0i32;
    for x in ev.iter().filter(|e| e["ph"] == "B" || e["ph"] == "E") {
        depth += if x["ph"] == "B" { 1 } else { -1 };
        assert!(depth >= 0, "E before its B");
    }
    let counter = ev
        .iter()
        .find(|e| e["ph"] == "C" && e["name"] == "worker-0/latency_us")
        .unwrap();
    assert_eq!(counter["args"]["latency_us"], 950.0);
    let instant = ev
        .iter()
        .find(|e| e["ph"] == "i" && e["cat"] == "info")
        .unwrap();
    assert_eq!(instant["name"], "request id=1 bytes=512");
    assert_eq!(instant["args"]["id"], 1);
    assert_eq!(trace["otherData"]["source"], "rtlog");
}
