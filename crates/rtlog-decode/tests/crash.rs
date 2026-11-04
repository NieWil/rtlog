//! Crash log: a child process logs, then crashes for real; the parent decodes
//! the crash file and checks that nothing logged before the crash was lost.
#![cfg(unix)]

use std::hint::black_box;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;

use rtlog_decode::{decode, Contract, Output};

const CHILD_ENV: &str = "RTLOG_CRASH_CHILD";
const FILE_ENV: &str = "RTLOG_CRASH_FILE";
const MAIN_RECORDS: u32 = 1_000;
const WORKER_RECORDS: u32 = 300;

/// The child half: runs only when the parent sets `RTLOG_CRASH_CHILD`.
#[test]
fn crash_child() {
    let Ok(mode) = std::env::var(CHILD_ENV) else {
        return;
    };
    rtlog::crash::install(std::env::var(FILE_ENV).unwrap()).unwrap();
    std::thread::Builder::new()
        .name("worker".into())
        .spawn(|| {
            for i in 0..WORKER_RECORDS {
                rtlog::info!("worker step={}", i);
            }
        })
        .unwrap()
        .join()
        .unwrap();
    rtlog::set_context_name("main");
    // No drain runs: every record below exists only in the rings.
    for i in 0..MAIN_RECORDS {
        rtlog::debug!("before crash i={} sensor={}", i, 2_000u32 + i);
    }
    rtlog::warn!("about to fail mode={}", mode.as_str());
    match mode.as_str() {
        "div0" => {
            let z = black_box(0u32);
            black_box(100 / z); // Rust panics: attempt to divide by zero
        }
        "segv" => unsafe { core::ptr::write_volatile(black_box(core::ptr::null_mut::<u32>()), 1) },
        "fpe" => hardware_divide_by_zero(),
        "abort" => std::process::abort(),
        "snapshot" => {
            rtlog::crash::snapshot("watchdog: deadlock suspected");
            rtlog::info!("still logging after snapshot {}", 1u8);
            return;
        }
        m => panic!("unknown mode {m}"),
    }
    unreachable!("mode {mode} should have crashed");
}

/// A real integer division by zero in machine code: SIGFPE on x86. Other
/// architectures do not trap on it, so the signal is raised directly there.
fn hardware_divide_by_zero() {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    unsafe {
        core::arch::asm!("div {d:e}", d = in(reg) black_box(0u32), inout("eax") 1u32 => _, inout("edx") 0u32 => _);
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
    unsafe {
        libc::raise(libc::SIGFPE);
    }
}

/// Run the child in `mode`; returns (exit status, decoded crash file as JSON lines).
fn run_child(mode: &str) -> (std::process::ExitStatus, String, rtlog_decode::Decoded) {
    let dir = std::env::temp_dir().join(format!("rtlog-crash-{}-{mode}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("crash.rtlog");
    let exe = std::env::current_exe().unwrap();
    // Under a cross-architecture runner (QEMU), launch the child through it.
    let runner = std::env::vars()
        .find(|(k, v)| {
            k.starts_with("CARGO_TARGET_") && k.ends_with("_RUNNER") && !v.trim().is_empty()
        })
        .map(|(_, v)| v);
    let mut cmd = match &runner {
        Some(r) => {
            let mut parts = r.split_whitespace();
            let mut c = Command::new(parts.next().unwrap());
            c.args(parts).arg(&exe);
            c
        }
        None => Command::new(&exe),
    };
    let out = cmd
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, mode)
        .env(FILE_ENV, &file)
        .env("RUST_BACKTRACE", "0")
        .output()
        .unwrap();
    let bytes = std::fs::read(&file).unwrap_or_else(|e| {
        panic!(
            "no crash file for {mode}: {e}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let contract = Contract::from_jsonl(&rtlog::contract::to_jsonl()).unwrap();
    let json = decode(&contract, &bytes, Output::Json, false).unwrap();
    let text = decode(&contract, &bytes, Output::Text, false).unwrap();
    assert_eq!(json.error, None, "crash file decodes cleanly");
    std::fs::remove_dir_all(&dir).ok();
    (out.status, json.text, text)
}

/// Every record logged before the crash is in the dump, in order, and the
/// crash record comes last.
fn check_dump(json: &str, text: &rtlog_decode::Decoded) {
    let lines: Vec<serde_json::Value> = json
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let main: Vec<u64> = lines
        .iter()
        .filter(|l| {
            l["type"] == "log" && l["message"].as_str().unwrap().starts_with("before crash")
        })
        .map(|l| l["args"][0].as_u64().unwrap())
        .collect();
    assert_eq!(
        main,
        (0..u64::from(MAIN_RECORDS)).collect::<Vec<_>>(),
        "all main-thread records, in order"
    );
    let worker = lines.iter().filter(|l| l["ctx_name"] == "worker").count();
    assert_eq!(
        worker as u32, WORKER_RECORDS,
        "the finished worker thread's records too"
    );
    assert!(lines.iter().any(|l| l["message"]
        .as_str()
        .is_some_and(|m| m.starts_with("about to fail"))));
    assert_eq!(
        lines.last().unwrap()["type"],
        "crash",
        "crash record is last"
    );
    assert_eq!(
        lines.last().unwrap()["ctx_name"],
        "main",
        "on the crashing thread"
    );
    // Times stay monotonic per context although earlier frames went nowhere.
    let ts: Vec<u64> = lines
        .iter()
        .filter(|l| l["ctx_name"] == "main")
        .map(|l| l["ts_ns"].as_u64().unwrap())
        .collect();
    assert!(ts.windows(2).all(|w| w[0] <= w[1]));
    let last = text.text.lines().last().unwrap();
    assert!(
        last.contains(" CRASH ") || last.contains(" SNAP "),
        "last text line: {last}"
    );
}

#[test]
fn division_by_zero_panic_is_logged() {
    let (status, json, text) = run_child("div0");
    assert!(!status.success());
    check_dump(&json, &text);
    let crash = text.crash.unwrap();
    assert_eq!(crash.kind, 1);
    assert!(
        crash.message.contains("attempt to divide by zero"),
        "{}",
        crash.message
    );
}

#[test]
fn segfault_is_logged() {
    let (status, json, text) = run_child("segv");
    assert_eq!(status.signal(), Some(libc::SIGSEGV), "{status:?}");
    check_dump(&json, &text);
    let crash = text.crash.unwrap();
    assert_eq!(
        (crash.kind, crash.signal, crash.addr),
        (2, libc::SIGSEGV, 0)
    );
}

#[test]
fn hardware_division_by_zero_is_logged() {
    let (status, json, text) = run_child("fpe");
    assert_eq!(status.signal(), Some(libc::SIGFPE), "{status:?}");
    check_dump(&json, &text);
    assert_eq!(text.crash.unwrap().signal, libc::SIGFPE);
}

#[test]
fn abort_is_logged() {
    let (status, json, text) = run_child("abort");
    assert_eq!(status.signal(), Some(libc::SIGABRT), "{status:?}");
    check_dump(&json, &text);
    assert_eq!(text.crash.unwrap().signal, libc::SIGABRT);
}

#[test]
fn snapshot_writes_a_dump_and_logging_continues() {
    let (status, json, text) = run_child("snapshot");
    assert!(status.success());
    check_dump(&json, &text);
    let crash = text.crash.unwrap();
    assert_eq!(
        (crash.kind, crash.message.as_str()),
        (3, "watchdog: deadlock suspected")
    );
}
