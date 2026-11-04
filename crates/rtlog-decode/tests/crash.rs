//! Crash log: a child process logs, then crashes for real; the parent decodes
//! the crash file and checks that nothing logged before the crash was lost.
#![cfg(unix)]

use std::hint::black_box;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;

use rtlog_decode::{decode, Contract, Output};

const CHILD_ENV: &str = "RTLOG_CRASH_CHILD";
const DIR_ENV: &str = "RTLOG_CRASH_DIR";
const MAIN_RECORDS: u32 = 1_000;
const WORKER_RECORDS: u32 = 300;

/// The child half: runs only when the parent sets `RTLOG_CRASH_CHILD`.
#[test]
fn crash_child() {
    let Ok(mode) = std::env::var(CHILD_ENV) else {
        return;
    };
    rtlog::crash::install(std::env::var(DIR_ENV).unwrap()).unwrap();
    // "drain": a drain is running and stuck inside a transport write, holding
    // the registry, when the crash happens. The dump must not depend on it.
    let _drain = (mode == "drain").then(|| {
        rtlog::info!("drain mode {}", 1u8);
        rtlog::spawn_drain(StuckTransport, std::time::Duration::from_millis(1))
    });
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
        "drain" => {
            // Give the drain time to start its pass and block in `write`.
            std::thread::sleep(std::time::Duration::from_millis(200));
            unsafe { core::ptr::write_volatile(black_box(core::ptr::null_mut::<u32>()), 1) }
        }
        "panics" => {
            // Two caught panics: two numbered dumps, neither overwriting the other.
            for i in 0..2u8 {
                let r = std::panic::catch_unwind(|| {
                    rtlog::info!("request {} starts", i);
                    let z = black_box(0u32);
                    black_box(100 / z);
                });
                assert!(r.is_err());
            }
            assert_eq!(rtlog::crash::dumps(), 2);
            return;
        }
        m => panic!("unknown mode {m}"),
    }
    unreachable!("mode {mode} should have crashed");
}

/// A transport whose first write never returns: the drain holds the registry
/// lock from inside it, and has consumed nothing (frames are committed after
/// the write), so every record must still be in the dump.
struct StuckTransport;

impl rtlog::Transport for StuckTransport {
    fn write(&mut self, _frame: &[u8]) -> Result<(), rtlog::Full> {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }
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
    let (status, mut dumps) = run_child_all(mode);
    assert_eq!(dumps.len(), 1, "one dump for {mode}");
    let (json, text) = dumps.remove(0);
    (status, json, text)
}

/// Run the child in `mode`; returns its exit status and every dump it wrote,
/// in order, decoded as JSON lines and as text.
fn run_child_all(
    mode: &str,
) -> (
    std::process::ExitStatus,
    Vec<(String, rtlog_decode::Decoded)>,
) {
    let dir = std::env::temp_dir().join(format!("rtlog-crash-{}-{mode}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
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
        .env(DIR_ENV, &dir)
        .env("RUST_BACKTRACE", "0")
        .output()
        .unwrap();
    // The child's pid is in the name: crash-<pid>-<n>.rtlog; sort by n.
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| {
            panic!(
                "no crash dir for {mode}: {e}\nstderr:\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no crash file for {mode}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let contract = Contract::from_jsonl(&rtlog::contract::to_jsonl()).unwrap();
    let dumps = files
        .iter()
        .map(|file| {
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                name.starts_with("crash-") && name.ends_with(".rtlog"),
                "{name}"
            );
            let bytes = std::fs::read(file).unwrap();
            let json = decode(&contract, &bytes, Output::Json, false).unwrap();
            let text = decode(&contract, &bytes, Output::Text, false).unwrap();
            assert_eq!(json.error, None, "crash file {name} decodes cleanly");
            (json.text, text)
        })
        .collect();
    std::fs::remove_dir_all(&dir).ok();
    (out.status, dumps)
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

/// The realistic case: a drain is running and stuck in a transport write
/// (holding the registry) when the process segfaults. The dump still holds
/// every record, because it does not wait for the drain.
#[test]
fn crash_while_the_drain_is_blocked_in_a_write_still_dumps_everything() {
    let (status, json, text) = run_child("drain");
    assert_eq!(status.signal(), Some(libc::SIGSEGV), "{status:?}");
    check_dump(&json, &text);
}

/// Each dump is a new numbered file: two caught panics give two files, and
/// the second does not overwrite the first.
#[test]
fn each_dump_is_a_new_file() {
    let (status, dumps) = run_child_all("panics");
    assert!(status.success());
    assert_eq!(dumps.len(), 2);
    for (i, (_, text)) in dumps.iter().enumerate() {
        let crash = text.crash.as_ref().unwrap();
        assert_eq!(crash.kind, 1);
        assert!(
            crash.message.contains("divide by zero"),
            "{}",
            crash.message
        );
        assert!(
            text.text.contains(&format!("request {i} starts")),
            "dump {i} holds its own records:\n{}",
            text.text
        );
    }
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
