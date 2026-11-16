//! The contract read from this test binary's own descriptors (ELF on Linux,
//! Mach-O on macOS, PE on Windows) must equal the one the runtime writes:
//! same IDs, strings, levels and kinds.

#[test]
fn contract_from_own_binary_matches_runtime_contract() {
    // Make sure this binary has log and span sites of its own.
    rtlog::info!("elf probe a={} b={}", 1u8, 2u16);
    drop(rtlog::span!("elf.probe"));

    let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let from_binary = rtlog_decode::elf::contract_from_binary(&exe).expect("descriptors readable");
    assert_eq!(from_binary, rtlog::contract::to_jsonl());
    let c = rtlog_decode::Contract::from_jsonl(&from_binary).unwrap();
    assert_eq!(c.hash(), rtlog::site::contract_hash());
    assert!(from_binary.contains(r#""fmt":"elf probe a={} b={}""#));
    assert!(from_binary.contains(r#""kind":"span""#));
    // The ELF-only entry point reads the same on ELF and refuses the rest.
    #[cfg(target_os = "linux")]
    assert_eq!(
        rtlog_decode::elf::contract_from_elf(&exe).unwrap(),
        from_binary
    );
    #[cfg(any(target_os = "macos", windows))]
    assert!(rtlog_decode::elf::contract_from_elf(&exe).is_err());
}

#[test]
fn non_binary_input_is_rejected() {
    assert!(rtlog_decode::elf::contract_from_elf(b"not an elf").is_err());
    assert!(rtlog_decode::elf::contract_from_binary(b"not an elf").is_err());
}
