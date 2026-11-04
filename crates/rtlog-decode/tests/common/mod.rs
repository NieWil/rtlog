// Shared helpers; each test file is its own binary with its own contract.
#![allow(dead_code)]

use rtlog_decode::{decode, Contract, Decoded, Output};

pub fn contract() -> Contract {
    Contract::from_jsonl(&rtlog::contract::to_jsonl()).expect("own contract is valid")
}

pub fn drain_all() -> Vec<u8> {
    let mut d = rtlog::Drain::new(Vec::new());
    d.poll().expect("Vec transport never fills");
    d.into_inner()
}

pub fn decode_text(bytes: &[u8]) -> Decoded {
    let d = decode(&contract(), bytes, Output::Text, false).expect("header ok");
    assert_eq!(d.error, None, "stream decoded cleanly");
    d
}

/// Message part of each text line (after the level column).
pub fn messages(d: &Decoded) -> Vec<String> {
    d.text
        .lines()
        .map(|l| {
            l.splitn(4, ' ')
                .nth(3)
                .unwrap_or("")
                .trim_start()
                .to_owned()
        })
        .collect()
}
