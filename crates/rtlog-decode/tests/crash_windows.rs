//! Crash log on Windows: a child process logs, then crashes for real (a
//! panic, an access violation); the parent decodes the crash file and checks
//! that nothing logged before the crash was lost. The Unix tests are in
//! `crash.rs`.
#![cfg(windows)]

use std::hint::black_box;
use std::process::Command;

use rtlog_decode::{decode, Contract, Decoded, Output};

const CHILD_ENV: &str = "RTLOG_CRASH_CHILD";
const DIR_ENV: &str = "RTLOG_CRASH_DIR";
const MAIN_RECORDS: u32 = 1_000;
const WORKER_RECORDS: u32 = 300;
const ACCESS_VIOLATION: u32 = 0xC000_0005;

extern "system" {
    fn SetErrorMode(mode: u32) -> u32;
}

/// The child half: runs only when the parent sets `RTLOG_CRASH_CHILD`.
#[test]
fn crash_child() {
    let Ok(mode) = std::env::var(CHILD_ENV) else {
        return;
    };
    // No error dialog: SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX.
    // SAFETY: only changes how this process reports its own crash.
    unsafe { SetErrorMode(0x0001 | 0x0002) };
    let dir = std::path::PathBuf::from(std::env::var(DIR_ENV).unwrap());
    rtlog::crash::install(&dir).unwrap();
    if mode == "errors" {
        // A second call is refused and changes nothing.
        let e = rtlog::crash::install(&dir).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        return;
    }
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
        "snapshot" => {
            rtlog::crash::snapshot("watchdog: deadlock suspected");
            rtlog::info!("still logging after snapshot {}", 1u8);
            return;
        }
        "collide" => {
            // A previous run left crash-<pid>-1.rtlog behind. The dump must
            // not touch it.
            std::fs::write(
                dir.join(format!("crash-{}-1.rtlog", std::process::id())),
                b"earlier crash",
            )
            .unwrap();
            rtlog::crash::snapshot("second run");
            return;
        }
        "two" => {
            // Two threads fault at the same moment: one complete dump.
            let go = std::sync::Arc::new(std::sync::Barrier::new(2));
            let g = go.clone();
            let t = std::thread::spawn(move || {
                g.wait();
                unsafe { core::ptr::write_volatile(black_box(core::ptr::null_mut::<u32>()), 2) }
            });
            go.wait();
            unsafe { core::ptr::write_volatile(black_box(core::ptr::null_mut::<u32>()), 1) };
            t.join().unwrap();
        }
        m => panic!("unknown mode {m}"),
    }
    unreachable!("mode {mode} should have crashed");
}

/// Run the child in `mode`; returns its exit status and every file it left in
/// its dump directory (name, bytes), sorted by name.
fn run_child(mode: &str) -> (std::process::ExitStatus, Vec<(String, Vec<u8>)>) {
    let dir = std::env::temp_dir().join(format!("rtlog-crash-{}-{mode}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, mode)
        .env(DIR_ENV, &dir)
        .env("RUST_BACKTRACE", "0")
        .output()
        .unwrap();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&p).unwrap(),
            )
        })
        .collect();
    files.sort();
    let _ = std::fs::remove_dir_all(&dir);
    (out.status, files)
}

fn decoded(bytes: &[u8]) -> (Vec<serde_json::Value>, Decoded) {
    let contract = Contract::from_jsonl(&rtlog::contract::to_jsonl()).unwrap();
    let json = decode(&contract, bytes, Output::Json, false).unwrap();
    let text = decode(&contract, bytes, Output::Text, false).unwrap();
    assert!(text.error.is_none(), "{:?}", text.error);
    let lines = json
        .text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    (lines, text)
}

/// Every record logged before the crash is in the dump, in order, and the
/// crash record comes last.
fn check_dump(lines: &[serde_json::Value]) {
    let main: Vec<u64> = lines
        .iter()
        .filter(|l| {
            l["type"] == "log" && l["message"].as_str().unwrap().starts_with("before crash")
        })
        .map(|l| l["args"][0].as_u64().unwrap())
        .collect();
    assert_eq!(main, (0..u64::from(MAIN_RECORDS)).collect::<Vec<_>>());
    let worker = lines.iter().filter(|l| l["ctx_name"] == "worker").count();
    assert_eq!(worker as u32, WORKER_RECORDS);
    assert!(lines.iter().any(|l| l["message"]
        .as_str()
        .is_some_and(|m| m.starts_with("about to fail"))));
    assert_eq!(lines.last().unwrap()["type"], "crash");
}

#[test]
fn division_by_zero_panic_is_logged() {
    let (status, files) = run_child("div0");
    assert!(!status.success());
    assert_eq!(
        files.len(),
        1,
        "{:?}",
        files.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    let (lines, text) = decoded(&files[0].1);
    check_dump(&lines);
    assert_eq!(lines.last().unwrap()["ctx_name"], "main");
    let crash = text.crash.unwrap();
    assert_eq!(crash.kind, 1);
    assert!(
        crash.message.contains("attempt to divide by zero"),
        "{crash:?}"
    );
}

#[test]
fn access_violation_is_logged() {
    let (status, files) = run_child("segv");
    // The process ends with the exception code as its exit status.
    assert_eq!(status.code().map(|c| c as u32), Some(ACCESS_VIOLATION));
    assert_eq!(
        files.len(),
        1,
        "{:?}",
        files.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    let (lines, text) = decoded(&files[0].1);
    check_dump(&lines);
    assert_eq!(lines.last().unwrap()["ctx_name"], "main");
    let crash = text.crash.unwrap();
    assert_eq!((crash.kind, crash.signal), (2, 0));
    assert_eq!(crash.code as u32, ACCESS_VIOLATION);
    assert_eq!(crash.addr, 0, "the write went to address 0");
    assert_ne!(crash.pc, 0);
    let last = text.text.lines().last().unwrap();
    assert!(
        last.contains("CRASH EXCEPTION_ACCESS_VIOLATION: invalid memory access (exception 0xc0000005) addr=0x0 pc=0x"),
        "{last}"
    );
}

#[test]
fn two_threads_crashing_at_once_leave_one_complete_dump() {
    let (status, files) = run_child("two");
    assert_eq!(status.code().map(|c| c as u32), Some(ACCESS_VIOLATION));
    assert_eq!(
        files.len(),
        1,
        "{:?}",
        files.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    let (lines, _) = decoded(&files[0].1);
    check_dump(&lines);
}

#[test]
fn snapshot_writes_a_dump_and_logging_continues() {
    let (status, files) = run_child("snapshot");
    assert!(status.success());
    assert_eq!(files.len(), 1);
    let (lines, text) = decoded(&files[0].1);
    check_dump(&lines);
    let crash = text.crash.unwrap();
    assert_eq!(crash.kind, 3);
    assert_eq!(crash.message, "watchdog: deadlock suspected");
}

#[test]
fn an_existing_dump_file_is_never_overwritten() {
    let (status, files) = run_child("collide");
    assert!(status.success());
    assert_eq!(
        files.len(),
        2,
        "{:?}",
        files.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    assert!(files[0].0.ends_with("-1.rtlog") && files[1].0.ends_with("-2.rtlog"));
    assert_eq!(files[0].1, b"earlier crash");
    let (lines, _) = decoded(&files[1].1);
    check_dump(&lines);
}

#[test]
fn a_second_install_is_refused() {
    let (status, files) = run_child("errors");
    assert!(status.success());
    assert!(files.is_empty());
}
