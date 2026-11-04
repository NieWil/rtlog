//! The contract read from this test binary's ELF descriptors must equal the
//! one the runtime writes: same IDs, strings, levels and kinds.

#[cfg(target_os = "linux")]
#[test]
fn contract_from_own_elf_matches_runtime_contract() {
    // Make sure this binary has log and span sites of its own.
    rtlog::info!("elf probe a={} b={}", 1u8, 2u16);
    drop(rtlog::span!("elf.probe"));

    let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let from_elf = rtlog_decode::elf::contract_from_elf(&exe).expect("descriptors readable");
    assert_eq!(from_elf, rtlog::contract::to_jsonl());
    let c = rtlog_decode::Contract::from_jsonl(&from_elf).unwrap();
    assert_eq!(c.hash(), rtlog::site::contract_hash());
    assert!(from_elf.contains(r#""fmt":"elf probe a={} b={}""#));
    assert!(from_elf.contains(r#""kind":"span""#));
}

#[test]
fn non_elf_input_is_rejected() {
    assert!(rtlog_decode::elf::contract_from_elf(b"not an elf").is_err());
}
