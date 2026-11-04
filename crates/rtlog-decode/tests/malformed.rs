//! The decoder treats streams and contracts as untrusted input (threats V5, V6).

use rtlog::arg::Tag;
use rtlog::wire::{Header, CTRL_CTX_REGISTER};
use rtlog_decode::{decode, Contract, DecodeError, Decoder, Event, Output};

const CONTRACT: &str = concat!(
    r#"{"id":1,"level":"info","module":"m","file":"f.rs","line":1,"column":1,"fmt":"a={} b={}","nargs":2}"#,
    "\n",
    r#"{"id":2,"level":"warn","module":"m","file":"f.rs","line":2,"column":1,"fmt":"s={}","nargs":1}"#,
    "\n",
);

/// A hand-built conformance stream: fixed clock, two contexts, a drop report.
fn vector(c: &Contract) -> Vec<u8> {
    let mut b = Header::new(c.hash(), 1_000).encode().to_vec(); // 1 kHz clock
    b.extend(CTRL_CTX_REGISTER.to_le_bytes());
    b.extend([1, 5]);
    b.extend(*b"motor");
    // site 1 at t=1500 ticks on ctx 1: a=7u8 b=-2i16
    b.extend(1u32.to_le_bytes());
    b.extend([0xdc, 0x0b, 1]); // varint 1500, ctx 1
    b.extend([Tag::U8 as u8, 7, Tag::I16 as u8, 0xfe, 0xff]);
    // site 2, +250 ticks on ctx 2: s="hé"
    b.extend(2u32.to_le_bytes());
    b.extend([0xfa, 0x01, 2]);
    b.extend([Tag::Str as u8, 3, b'h', 0xc3, 0xa9]);
    // dropped 4 on ctx 1
    b.extend(rtlog::wire::CTRL_DROPPED.to_le_bytes());
    b.extend([1]);
    b.extend(4u32.to_le_bytes());
    b
}

#[test]
fn conformance_vector_decodes_exactly() {
    let c = Contract::from_jsonl(CONTRACT).unwrap();
    let d = decode(&c, &vector(&c), Output::Text, false).unwrap();
    assert_eq!(d.error, None);
    assert_eq!(
        d.text,
        "0.000000 [ctx2] WARN  s=hé\n1.250000 [motor] INFO  a=7 b=-2\n"
    );
    assert_eq!((d.records, d.dropped, d.contexts), (2, 4, 1));

    let j = decode(&c, &vector(&c), Output::Json, false).unwrap();
    let ts: Vec<u64> = j
        .text
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["ts_ns"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(ts, [250_000_000, 1_500_000_000], "JSON keeps absolute time");

    let events: Vec<_> = Decoder::new(&c, &vector(&c), false)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(matches!(&events[0], Event::CtxRegister { ctx: 1, name } if name == "motor"));
    assert!(matches!(&events[3], Event::Dropped { ctx: 1, count: 4 }));
}

#[test]
fn header_and_contract_mismatch() {
    let c = Contract::from_jsonl(CONTRACT).unwrap();
    assert_eq!(
        decode(&c, b"nope", Output::Text, false).unwrap_err(),
        DecodeError::BadHeader
    );
    let mut v = vector(&c);
    v[6] ^= 0xff; // contract hash
    assert!(matches!(
        decode(&c, &v, Output::Text, false),
        Err(DecodeError::ContractMismatch { .. })
    ));
    let forced = decode(&c, &v, Output::Text, true).unwrap();
    assert!(
        forced.text.lines().all(|l| l.contains(" ?")),
        "forced lines are marked: {}",
        forced.text
    );
    let mut v = vector(&c);
    v[4] = 9;
    assert_eq!(
        decode(&c, &v, Output::Text, false).unwrap_err(),
        DecodeError::WireVersion(9)
    );
}

#[test]
fn unknown_site_stops_with_offset_and_keeps_earlier_records() {
    let c = Contract::from_jsonl(CONTRACT).unwrap();
    let mut v = vector(&c);
    v.extend(99u32.to_le_bytes());
    let d = decode(&c, &v, Output::Text, false).unwrap();
    assert_eq!(d.records, 2);
    assert!(matches!(
        d.error,
        Some(DecodeError::UnknownSite { id: 99, .. })
    ));
}

/// Deterministic xorshift so the test needs no extra crates.
fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn truncated_and_corrupted_streams_never_panic() {
    let c = Contract::from_jsonl(CONTRACT).unwrap();
    let v = vector(&c);
    for cut in 0..=v.len() {
        let _ = decode(&c, &v[..cut], Output::Json, true);
    }
    let mut seed = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..20_000 {
        let mut m = v.clone();
        for _ in 0..1 + rng(&mut seed) % 4 {
            let i = (rng(&mut seed) as usize) % m.len();
            m[i] = rng(&mut seed) as u8;
        }
        if rng(&mut seed).is_multiple_of(4) {
            let extra = (rng(&mut seed) % 64) as usize;
            m.extend((0..extra).map(|_| rng(&mut seed) as u8));
        }
        let _ = decode(&c, &m, Output::Text, true);
    }
}
