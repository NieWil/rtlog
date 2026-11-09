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
    if mode == "smallstack" {
        small_signal_stack();
    }
    if mode == "errors" {
        install_errors();
        return;
    }
    rtlog::crash::install(std::env::var(DIR_ENV).unwrap()).unwrap();
    if mode == "details" {
        truncation_and_drops();
        return;
    }
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
        "abort" | "smallstack" => std::process::abort(),
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
        "collide" => {
            // A previous run left crash-<pid>-1.rtlog behind (same pid: a
            // container restart). The dump must not touch it.
            let dir = std::path::PathBuf::from(std::env::var(DIR_ENV).unwrap());
            std::fs::write(
                dir.join(format!("crash-{}-1.rtlog", std::process::id())),
                b"earlier crash",
            )
            .unwrap();
            rtlog::crash::snapshot("second run");
            return;
        }
        "flood" => {
            // A service that catches a panic per request: the disk must not fill.
            for _ in 0..40 {
                rtlog::crash::snapshot("noisy watchdog");
            }
            assert_eq!(rtlog::crash::dumps(), rtlog::crash::MAX_SOFT_DUMPS);
            // A fatal signal is still dumped.
            unsafe { core::ptr::write_volatile(black_box(core::ptr::null_mut::<u32>()), 1) }
        }
        "quiet" => {
            // A thread that never logged faults: it has only the small
            // default signal stack (8 KiB here, what Rust gives a thread on
            // x86-64 CPUs without AMX), never the one rtlog sets up.
            std::thread::spawn(|| {
                small_signal_stack();
                unsafe { core::ptr::write_volatile(black_box(core::ptr::null_mut::<u32>()), 3) }
            })
            .join()
            .unwrap();
        }
        "two" => {
            // Two threads fault at the same moment: the first one's dump must
            // be complete, the second must wait for it, not kill the process.
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

/// Records the "details" child logs; more than its 64 KiB ring holds.
const DETAIL_RECORDS: u32 = 10_000;

/// Child mode "errors": what `install` refuses, and that a refused call
/// leaves the handler installable. Writes one snapshot once installed.
fn install_errors() {
    let dir = std::path::PathBuf::from(std::env::var(DIR_ENV).unwrap());
    rtlog::crash::snapshot("before install");
    assert_eq!(rtlog::crash::dumps(), 0, "no handler, no dump");

    let long = long_dir(&dir).join("x".repeat(250));
    let e = rtlog::crash::install(&long).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
    assert!(e.to_string().contains("too long"), "{e}");

    let e = rtlog::crash::install("bad\0dir").unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput, "{e}");

    rtlog::crash::install(&dir).unwrap();
    let e = rtlog::crash::install(&dir).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(e.to_string().contains("already installed"), "{e}");

    rtlog::crash::snapshot("installed");
    assert_eq!(rtlog::crash::dumps(), 1);
}

/// Where the "errors" child puts its over-long directory: beside the dump
/// directory, so the dump directory holds only dumps.
fn long_dir(dump_dir: &std::path::Path) -> std::path::PathBuf {
    let mut p = dump_dir.as_os_str().to_owned();
    p.push(".long");
    p.into()
}

/// Child mode "details": a context name and a message longer than a dump
/// keeps, both cut inside a UTF-8 character; a ring that dropped frames; and
/// a snapshot from a thread that never logged.
fn truncation_and_drops() {
    // 1 + 40 × 2 = 81 bytes; the dump keeps 64, which would split an "é".
    rtlog::set_context_name(&format!("a{}", "é".repeat(40)));
    let drops = rtlog::stats().drops;
    for i in 0..DETAIL_RECORDS {
        rtlog::info!("detail {}", i);
    }
    assert!(rtlog::stats().drops > drops, "the ring overflowed");
    // 200 bytes; the record keeps 127, which would split a "ü".
    std::thread::spawn(|| rtlog::crash::snapshot(&"ü".repeat(100)))
        .join()
        .unwrap();
    assert_eq!(rtlog::crash::dumps(), 1);
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

/// What a failing assertion prints about a child run: its exit status, its
/// stderr and the shape of each dump (size, contexts, first and last lines),
/// so a failure seen only in CI shows where the dump stopped.
type Diag = String;

/// Give the calling thread an 8 KiB alternate signal stack with a guard page
/// below it: what Rust's std gives every thread on CPUs whose
/// `AT_MINSIGSTKSZ` is under 8 KiB (most x86-64 without AMX). On such a stack
/// the dump of a debug build used to overflow into the guard page. Where the
/// system minimum is larger (32 KiB on macOS), that minimum: still below
/// `SIGNAL_STACK`, so `install` must replace it.
fn small_signal_stack() {
    let size = libc::MINSIGSTKSZ.max(8192);
    // SAFETY: plain mmap/mprotect/sigaltstack on memory this function owns and
    // leaks for the rest of the (short-lived) child process.
    unsafe {
        let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
        let base = libc::mmap(
            core::ptr::null_mut(),
            size + page,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        );
        assert!(base != libc::MAP_FAILED);
        assert_eq!(libc::mprotect(base, page, libc::PROT_NONE), 0);
        let st = libc::stack_t {
            ss_sp: base.cast::<u8>().add(page).cast(),
            ss_flags: 0,
            ss_size: size,
        };
        assert_eq!(
            libc::sigaltstack(&st, core::ptr::null_mut()),
            0,
            "sigaltstack({size}): {}",
            std::io::Error::last_os_error()
        );
    }
}

/// Run the child in `mode`; returns (exit status, decoded crash file as JSON
/// lines and as text, diagnostics).
fn run_child(
    mode: &str,
) -> (
    std::process::ExitStatus,
    String,
    rtlog_decode::Decoded,
    Diag,
) {
    let (status, mut dumps, diag) = run_child_all(mode);
    assert_eq!(dumps.len(), 1, "one dump for {mode}\n{diag}");
    let (json, text) = dumps.remove(0);
    (status, json, text, diag)
}

/// Run the child in `mode` with its dump directory `dir`; returns its exit
/// status and every file in the directory (name, bytes), sorted by name.
fn run_child_files(
    mode: &str,
    dir: &std::path::Path,
) -> (std::process::ExitStatus, Vec<(String, Vec<u8>)>) {
    let exe = std::env::current_exe().unwrap();
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
        .env(DIR_ENV, dir)
        .env("RUST_BACKTRACE", "0")
        .output()
        .unwrap();
    let mut files: Vec<_> = std::fs::read_dir(dir)
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
    (out.status, files)
}

/// Run the child in `mode`; returns its exit status and every dump it wrote,
/// in order, decoded as JSON lines and as text.
fn run_child_all(
    mode: &str,
) -> (
    std::process::ExitStatus,
    Vec<(String, rtlog_decode::Decoded)>,
    Diag,
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
    let mut diag = format!(
        "child {mode}: {:?}\nchild stderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let dumps = files
        .iter()
        .map(|file| {
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                name.starts_with("crash-") && name.ends_with(".rtlog"),
                "{name}\n{diag}"
            );
            let bytes = std::fs::read(file).unwrap();
            let json = decode(&contract, &bytes, Output::Json, false).unwrap();
            let text = decode(&contract, &bytes, Output::Text, false).unwrap();
            diag.push_str(&dump_summary(&name, &bytes, &json.text, &text.text));
            assert_eq!(
                json.error, None,
                "crash file {name} decodes cleanly\n{diag}"
            );
            (json.text, text)
        })
        .collect();
    std::fs::remove_dir_all(&dir).ok();
    (out.status, dumps, diag)
}

/// Size, records per context and the first and last text lines of one dump.
fn dump_summary(name: &str, bytes: &[u8], json: &str, text: &str) -> String {
    let mut per_ctx: Vec<(String, usize)> = Vec::new();
    for line in json.lines() {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ctx = format!("{}/{}", v["ctx_name"], v["type"]);
        match per_ctx.iter_mut().find(|(c, _)| *c == ctx) {
            Some((_, n)) => *n += 1,
            None => per_ctx.push((ctx, 1)),
        }
    }
    let lines: Vec<&str> = text.lines().collect();
    let head = lines
        .iter()
        .take(8)
        .copied()
        .collect::<Vec<_>>()
        .join("\n  ");
    let tail = lines
        .iter()
        .skip(lines.len().saturating_sub(8).max(8))
        .copied()
        .collect::<Vec<_>>()
        .join("\n  ");
    format!(
        "dump {name}: {} bytes, {} text lines, records by context/type: {per_ctx:?}\n  first:\n  {head}\n  last:\n  {tail}\n",
        bytes.len(),
        lines.len()
    )
}

/// Every record logged before the crash is in the dump, in order, and the
/// crash record comes last.
fn check_dump(json: &str, text: &rtlog_decode::Decoded, diag: &str) {
    check_dump_on(json, text, Some("main"), diag);
}

/// As [`check_dump`]; `crashing` names the context the crash record must be on,
/// or `None` when either of two racing threads may be the one.
fn check_dump_on(json: &str, text: &rtlog_decode::Decoded, crashing: Option<&str>, diag: &str) {
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
        "all main-thread records, in order\n{diag}"
    );
    let worker = lines.iter().filter(|l| l["ctx_name"] == "worker").count();
    assert_eq!(
        worker as u32, WORKER_RECORDS,
        "the finished worker thread's records too\n{diag}"
    );
    assert!(
        lines.iter().any(|l| l["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("about to fail"))),
        "the last record before the crash\n{diag}"
    );
    assert_eq!(
        lines.last().unwrap()["type"],
        "crash",
        "crash record is last\n{diag}"
    );
    if let Some(name) = crashing {
        assert_eq!(
            lines.last().unwrap()["ctx_name"],
            name,
            "on the crashing thread\n{diag}"
        );
    }
    // Times stay monotonic per context although earlier frames went nowhere.
    let ts: Vec<u64> = lines
        .iter()
        .filter(|l| l["ctx_name"] == "main")
        .map(|l| l["ts_ns"].as_u64().unwrap())
        .collect();
    assert!(
        ts.windows(2).all(|w| w[0] <= w[1]),
        "monotonic times\n{diag}"
    );
    let last = text.text.lines().last().unwrap();
    assert!(
        last.contains(" CRASH ") || last.contains(" SNAP "),
        "last text line: {last}\n{diag}"
    );
}

#[test]
fn division_by_zero_panic_is_logged() {
    let (status, json, text, diag) = run_child("div0");
    assert!(!status.success(), "{diag}");
    check_dump(&json, &text, &diag);
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
    let (status, json, text, diag) = run_child("segv");
    assert_eq!(status.signal(), Some(libc::SIGSEGV), "{status:?}\n{diag}");
    check_dump(&json, &text, &diag);
    let crash = text.crash.unwrap();
    assert_eq!(
        (crash.kind, crash.signal, crash.addr),
        (2, libc::SIGSEGV, 0)
    );
}

#[test]
fn hardware_division_by_zero_is_logged() {
    let (status, json, text, diag) = run_child("fpe");
    assert_eq!(status.signal(), Some(libc::SIGFPE), "{status:?}\n{diag}");
    check_dump(&json, &text, &diag);
    assert_eq!(text.crash.unwrap().signal, libc::SIGFPE);
}

#[test]
fn abort_is_logged() {
    let (status, json, text, diag) = run_child("abort");
    assert_eq!(status.signal(), Some(libc::SIGABRT), "{status:?}\n{diag}");
    check_dump(&json, &text, &diag);
    assert_eq!(text.crash.unwrap().signal, libc::SIGABRT);
}

/// A thread whose default signal stack is only 8 KiB (or the system minimum)
/// still gets a complete
/// dump: `install` gives it a bigger one. Without that, a debug build
/// overflowed the small stack, wrote only the stream header and died of
/// SIGSEGV instead of the original signal.
#[test]
fn a_small_default_signal_stack_still_gives_a_complete_dump() {
    let (status, json, text, diag) = run_child("smallstack");
    assert_eq!(status.signal(), Some(libc::SIGABRT), "{status:?}\n{diag}");
    check_dump(&json, &text, &diag);
}

/// A thread that never logged, with only a small default signal stack, faults:
/// the dump runs on rtlog's own dump stack and is complete. Before, a debug
/// build overflowed the small stack and wrote only the stream header.
#[test]
fn a_thread_that_never_logged_still_gives_a_complete_dump() {
    let (status, json, text, diag) = run_child("quiet");
    assert_eq!(status.signal(), Some(libc::SIGSEGV), "{status:?}\n{diag}");
    check_dump_on(&json, &text, None, &diag);
}

/// The realistic case: a drain is running and stuck in a transport write
/// (holding the registry) when the process segfaults. The dump still holds
/// every record, because it does not wait for the drain.
#[test]
fn crash_while_the_drain_is_blocked_in_a_write_still_dumps_everything() {
    let (status, json, text, diag) = run_child("drain");
    assert_eq!(status.signal(), Some(libc::SIGSEGV), "{status:?}\n{diag}");
    check_dump(&json, &text, &diag);
}

/// A file from an earlier run with the same pid is never overwritten.
#[test]
fn an_existing_dump_file_is_never_overwritten() {
    let dir = std::env::temp_dir().join(format!("rtlog-collide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (status, files) = run_child_files("collide", &dir);
    assert!(status.success());
    assert_eq!(files.len(), 2, "{files:?}");
    let old = files.iter().find(|(n, _)| n.ends_with("-1.rtlog")).unwrap();
    assert_eq!(old.1, b"earlier crash", "the earlier file is untouched");
    let new = files.iter().find(|(n, _)| n.ends_with("-2.rtlog")).unwrap();
    assert_eq!(&new.1[..4], b"RTL1", "the new dump went to the next number");
    std::fs::remove_dir_all(&dir).ok();
}

/// Caught panics and snapshots stop after MAX_SOFT_DUMPS files; a fatal signal
/// afterwards is still dumped.
#[test]
fn a_flood_of_soft_dumps_is_capped_but_a_fatal_one_still_lands() {
    let dir = std::env::temp_dir().join(format!("rtlog-flood-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (status, files) = run_child_files("flood", &dir);
    assert_eq!(status.signal(), Some(libc::SIGSEGV), "{status:?}");
    assert_eq!(files.len() as u32, rtlog::crash::MAX_SOFT_DUMPS + 1);
    std::fs::remove_dir_all(&dir).ok();
}

/// Two threads fault together: one complete dump, process killed by the signal.
#[test]
fn two_threads_crashing_at_once_leave_one_complete_dump() {
    let (status, json, text, diag) = run_child("two");
    assert_eq!(status.signal(), Some(libc::SIGSEGV), "{status:?}\n{diag}");
    check_dump_on(&json, &text, None, &diag);
}

/// Each dump is a new numbered file: two caught panics give two files, and
/// the second does not overwrite the first.
#[test]
fn each_dump_is_a_new_file() {
    let (status, dumps, diag) = run_child_all("panics");
    assert!(status.success(), "{diag}");
    assert_eq!(dumps.len(), 2, "{diag}");
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
    let (status, json, text, diag) = run_child("snapshot");
    assert!(status.success(), "{diag}");
    check_dump(&json, &text, &diag);
    let crash = text.crash.unwrap();
    assert_eq!(
        (crash.kind, crash.message.as_str()),
        (3, "watchdog: deadlock suspected")
    );
}

/// `install` rejects an over-long path and a second call; a rejected call
/// does not count as installed, and `snapshot` before `install` does nothing.
#[test]
fn install_rejects_bad_paths_and_a_second_call() {
    let (status, json, text, diag) = run_child("errors");
    let dir = std::env::temp_dir().join(format!("rtlog-crash-{}-errors", std::process::id()));
    std::fs::remove_dir_all(long_dir(&dir)).ok();
    assert!(status.success(), "{diag}");
    let crash = text.crash.unwrap();
    assert_eq!(
        (crash.kind, crash.message.as_str()),
        (3, "installed"),
        "{json}"
    );
}

/// Names and messages are cut at a character boundary, drops are reported,
/// and a snapshot from a thread that never logged lands on context 0.
#[test]
fn a_dump_cuts_long_names_at_a_character_and_reports_drops() {
    let (status, json, text, diag) = run_child("details");
    assert!(status.success(), "{diag}");
    let lines: Vec<serde_json::Value> = json
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let kept: Vec<_> = lines
        .iter()
        .filter(|l| l["type"] == "log" && l["message"].as_str().unwrap().starts_with("detail "))
        .collect();
    assert!(!kept.is_empty(), "{diag}");
    let name = format!("a{}", "é".repeat(31)); // 63 bytes, not 64
    assert!(
        kept.iter().all(|l| l["ctx_name"] == name.as_str()),
        "{diag}"
    );
    assert!(text.dropped > 0, "{diag}");
    assert_eq!(
        kept.len() as u64 + text.dropped,
        u64::from(DETAIL_RECORDS),
        "every record is either in the dump or counted as dropped\n{diag}"
    );
    let crash = text.crash.unwrap();
    assert_eq!(crash.kind, 3);
    assert_eq!(crash.message, "ü".repeat(63), "126 bytes, not 127");
    let record = lines.last().unwrap();
    assert_eq!(
        (&record["type"], &record["ctx"]),
        (&serde_json::json!("crash"), &serde_json::json!(0)),
        "{diag}"
    );
}
