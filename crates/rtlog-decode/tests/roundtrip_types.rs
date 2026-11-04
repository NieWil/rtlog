mod common;

use common::*;
use rtlog_decode::{decode, Output};

#[test]
fn every_argument_type_roundtrips_to_text_and_json() {
    rtlog::set_context_name("main");
    let name = String::from("pump-2");
    let payload = [0xde_u8, 0xad, 0xbe, 0xef];
    rtlog::trace!("unsigned {} {} {} {}", 1u8, 2u16, 3u32, 4u64);
    rtlog::debug!("signed {} {} {} {}", -1i8, -2i16, -3i32, -4i64);
    rtlog::info!("float {:.3} {}", 1.23456f32, -0.5f64);
    rtlog::warn!("bool {} char {}", true, 'λ');
    rtlog::error!("str {} owned {}", "static", name);
    rtlog::info!("bytes {:hex} plain {}", &payload[..], &payload[..2]);
    rtlog::info!("hex {:x} {:X} bin {:b}", 255u8, 0xabcu16, 5u8);
    rtlog::info!(
        "option {} {} array {}",
        Some(7u8),
        None::<u8>,
        [1i16, -1, 2]
    );
    rtlog::info!("size {} {}", 42usize, -42isize);
    rtlog::info!("escaped {{braces}} {}", 1u8);
    rtlog::info!("no args");
    let long = "x".repeat(500);
    rtlog::info!("long {}", long.as_str());

    let bytes = drain_all();
    let d = decode_text(&bytes);
    assert_eq!(
        messages(&d),
        [
            "unsigned 1 2 3 4",
            "signed -1 -2 -3 -4",
            "float 1.235 -0.5",
            "bool true char λ",
            "str static owned pump-2",
            "bytes deadbeef plain [dead]",
            "hex ff ABC bin 101",
            "option Some(7) None array [1, -1, 2]",
            "size 42 -42",
            "escaped {braces} 1",
            "no args",
            &format!("long {}…", "x".repeat(rtlog::arg::DYN_CAP)),
        ]
    );
    let first = d.text.lines().next().unwrap();
    assert!(first.contains(" [main] TRACE "), "{first}");
    assert_eq!(d.records, 12);
    assert_eq!(d.dropped, 0);

    let j = decode(&contract(), &bytes, Output::Json, false).unwrap();
    let lines: Vec<serde_json::Value> = j
        .text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 12);
    assert_eq!(lines[2]["level"], "info");
    assert_eq!(lines[2]["message"], "float 1.235 -0.5");
    assert_eq!(lines[1]["args"], serde_json::json!([-1, -2, -3, -4]));
    assert_eq!(
        lines[4]["args"][0],
        serde_json::json!({"s": "static", "truncated": false})
    );
    assert_eq!(lines[0]["ctx_name"], "main");
    assert!(lines[0]["file"]
        .as_str()
        .unwrap()
        .ends_with("roundtrip_types.rs"));
    // Timestamps never go backwards within one context.
    let ts: Vec<u64> = lines.iter().map(|l| l["ts_ns"].as_u64().unwrap()).collect();
    assert!(ts.windows(2).all(|w| w[0] <= w[1]), "{ts:?}");
}
