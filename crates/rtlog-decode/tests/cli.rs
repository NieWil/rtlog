mod common;

use std::process::Command;

/// The CLI binary, launched through the target runner (QEMU) when the tests
/// themselves run under one, as in cross-architecture CI.
fn rtlog_cmd() -> Command {
    let bin = env!("CARGO_BIN_EXE_rtlog");
    let runner = std::env::vars()
        .find(|(k, v)| {
            k.starts_with("CARGO_TARGET_") && k.ends_with("_RUNNER") && !v.trim().is_empty()
        })
        .map(|(_, v)| v);
    match runner {
        Some(r) => {
            let mut parts = r.split_whitespace();
            let mut c = Command::new(parts.next().unwrap());
            c.args(parts).arg(bin);
            c
        }
        None => Command::new(bin),
    }
}

#[test]
fn cli_decodes_files_and_checks_contracts() {
    rtlog::info!("cli speed={} ok={}", 1200u16, true);
    let bytes = common::drain_all();

    let dir = std::env::temp_dir().join(format!("rtlog-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let contract = dir.join("app.contract.jsonl");
    let stream = dir.join("app.rtlog");
    rtlog::contract::write(&contract).unwrap();
    std::fs::write(&stream, &bytes).unwrap();

    let out = rtlog_cmd()
        .arg("decode")
        .arg("--contract")
        .arg(&contract)
        .arg(&stream)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("INFO  cli speed=1200 ok=true"), "{text}");

    let out = rtlog_cmd()
        .args(["decode", "--json", "--contract"])
        .arg(&contract)
        .arg(&stream)
        .output()
        .unwrap();
    let v: serde_json::Value =
        serde_json::from_slice(out.stdout.split(|&b| b == b'\n').next().unwrap()).unwrap();
    assert_eq!(v["message"], "cli speed=1200 ok=true");

    let out = rtlog_cmd().arg("check").arg(&contract).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("contract hash"));

    let out = rtlog_cmd().arg("bogus").output().unwrap();
    assert!(!out.status.success());
    let out = rtlog_cmd()
        .args(["decode", "--contract"])
        .arg(&contract)
        .arg(dir.join("missing"))
        .output()
        .unwrap();
    assert!(!out.status.success());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn every_subcommand_prints_usage_on_help() {
    for args in [
        &["--help"][..],
        &["decode", "--help"],
        &["check", "--help"],
        &["contract", "-h"],
    ] {
        let out = rtlog_cmd().args(args).output().unwrap();
        assert!(out.status.success(), "rtlog {args:?}: {out:?}");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("rtlog decode --contract"),
            "rtlog {args:?}: {text}"
        );
    }
}
