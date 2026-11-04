//! Crash log (spec section 8b): when the process panics or dies on a fatal
//! signal, write everything the drain has not sent yet to a crash file, so the
//! last log lines before the failure are never lost.
//!
//! ```no_run
//! rtlog::crash::install("/var/lib/myapp/crashes").unwrap();
//! // ... on a segfault, division by zero, abort or panic, a new file
//! // /var/lib/myapp/crashes/crash-<pid>-<n>.rtlog holds the unread tail of
//! // every thread's ring plus a CRASH record:
//! //   rtlog decode --contract app.contract.jsonl crashes/crash-4242-1.rtlog
//! ```
//!
//! Each dump is a new file, numbered per process, so a caught panic or a
//! watchdog [`snapshot`] never overwrites an earlier dump, and a restart after
//! a crash (new pid) never overwrites the crash.
//!
//! What it covers: panics (including integer division by zero in Rust),
//! `SIGSEGV`, `SIGBUS`, `SIGFPE`, `SIGILL` and `SIGABRT` (so `abort()` and
//! `panic = "abort"` too). It cannot see `SIGKILL` or power loss.
//!
//! How it stays safe inside a signal handler: it allocates nothing, takes no
//! blocking lock (only `try_lock`), formats nothing, reads the rings without
//! consuming them and writes with `open(2)` / `write(2)`. The list of rings
//! has its own lock that is only ever held for a push or a removal, so the
//! dump does not depend on the drain, which may be blocked in a transport
//! write at that moment. A fatal signal first freezes logging; a dump taken
//! while the drain is consuming may then repeat a frame the drain had also
//! sent, never lose one. After the dump the default action runs (core dump,
//! exit status shows the signal). On a panic the previous panic hook runs
//! after the dump, and logging keeps working.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, TryLockError};

use crate::ring::FRAME_MAX;
use crate::runtime::{Shared, FAST, REGISTRY, RINGS};
use crate::varint;
use crate::wire::{self, Header, HEADER_LEN};

/// Signals the handler is installed for.
pub const SIGNALS: [i32; 5] = [
    libc::SIGSEGV,
    libc::SIGBUS,
    libc::SIGFPE,
    libc::SIGILL,
    libc::SIGABRT,
];

/// Crash kind for [`snapshot`]: a dump requested by the program, not a crash.
pub const CRASH_SNAPSHOT: u8 = 3;

/// `<dir>/crash-<pid>-`, so a dump only appends its number and `.rtlog`.
static PREFIX: OnceLock<CString> = OnceLock::new();
static HEADER: OnceLock<[u8; HEADER_LEN]> = OnceLock::new();
/// Dumps written so far by this process.
static SEQ: AtomicU32 = AtomicU32::new(0);
/// Set by the first fatal dump: later signals (an abort after a panic, a fault
/// inside the handler) do not dump again.
static FATAL: AtomicBool = AtomicBool::new(false);

/// Install the crash handler. Dumps go to `dir` (created if missing) as
/// `crash-<pid>-<n>.rtlog`, `n` counting from 1 per process.
///
/// Call it early in `main`, once. It also fixes the clock rate now, so the
/// handler never calibrates it.
pub fn install(dir: impl AsRef<Path>) -> io::Result<()> {
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir)?;
    let mut prefix = dir.as_os_str().as_bytes().to_vec();
    if !prefix.ends_with(b"/") {
        prefix.push(b'/');
    }
    prefix.extend_from_slice(format!("crash-{}-", std::process::id()).as_bytes());
    if prefix.len() > PREFIX_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "crash directory path is too long (200 bytes at most)",
        ));
    }
    let c = CString::new(prefix)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    if PREFIX.set(c).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "rtlog crash handler already installed",
        ));
    }
    HEADER.get_or_init(|| Header::new(crate::site::contract_hash(), crate::clock::hz()).encode());

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // One line: "panicked at src/x.rs:10:5: attempt to divide by zero".
        let msg = info.to_string().replace('\n', " ");
        dump(wire::CRASH_PANIC, 0, 0, 0, 0, &msg, false);
        previous(info);
    }));

    for sig in SIGNALS {
        // SAFETY: a zeroed sigaction is valid; we fill handler, flags and mask.
        unsafe {
            let mut sa: libc::sigaction = core::mem::zeroed();
            sa.sa_sigaction = on_signal as *const () as usize;
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESETHAND;
            libc::sigemptyset(&mut sa.sa_mask);
            if libc::sigaction(sig, &sa, core::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// Write a dump now without crashing, e.g. from a watchdog that detected a
/// deadlock or a missed deadline. Logging keeps working.
pub fn snapshot(reason: &str) {
    dump(CRASH_SNAPSHOT, 0, 0, 0, 0, reason, false);
}

/// Number of dumps this process has written; the last file is
/// `<dir>/crash-<pid>-<dumps()>.rtlog`.
pub fn dumps() -> u32 {
    SEQ.load(Ordering::Acquire)
}

extern "C" fn on_signal(sig: i32, info: *mut libc::siginfo_t, uctx: *mut libc::c_void) {
    // SAFETY: the kernel passes a valid siginfo for SA_SIGINFO handlers.
    let (addr, code) = unsafe {
        if info.is_null() {
            (0, 0)
        } else {
            (fault_addr(info), (*info).si_code)
        }
    };
    dump(
        wire::CRASH_SIGNAL,
        sig,
        code,
        addr,
        pc_of(uctx),
        signal_name(sig),
        true,
    );
    // SA_RESETHAND restored the default action: re-raise to get it (core dump,
    // exit status that names the signal).
    // SAFETY: raising a signal is async-signal-safe.
    unsafe { libc::raise(sig) };
}

#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn fault_addr(info: *mut libc::siginfo_t) -> u64 {
    (*info).si_addr() as u64
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
unsafe fn fault_addr(info: *mut libc::siginfo_t) -> u64 {
    (*info).si_addr as u64
}

#[allow(unused_variables)]
fn pc_of(uctx: *mut libc::c_void) -> u64 {
    if uctx.is_null() {
        return 0;
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    // SAFETY: on Linux the third handler argument is a `ucontext_t`.
    return unsafe {
        (*(uctx as *const libc::ucontext_t)).uc_mcontext.gregs[libc::REG_RIP as usize] as u64
    };
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    // SAFETY: as above.
    return unsafe { (*(uctx as *const libc::ucontext_t)).uc_mcontext.pc };
    #[allow(unreachable_code)]
    0
}

fn signal_name(sig: i32) -> &'static str {
    match sig {
        libc::SIGSEGV => "SIGSEGV: invalid memory access",
        libc::SIGBUS => "SIGBUS: bus error",
        libc::SIGFPE => "SIGFPE: arithmetic error (e.g. integer division by zero)",
        libc::SIGILL => "SIGILL: illegal instruction",
        libc::SIGABRT => "SIGABRT: abort",
        _ => "signal",
    }
}

fn write_all(fd: i32, mut b: &[u8]) {
    while !b.is_empty() {
        // SAFETY: `b` is a valid buffer; write(2) is async-signal-safe.
        let n = unsafe { libc::write(fd, b.as_ptr().cast(), b.len()) };
        if n > 0 {
            b = &b[n as usize..];
        } else if n < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        } else {
            return;
        }
    }
}

/// `try_lock` up to `attempts` times with a spin between: never blocks or
/// sleeps, so it is safe in a signal handler, cheap in a panic hook, and
/// cannot deadlock if the crashing thread already holds the lock.
fn try_lock_for<T>(m: &'static Mutex<T>, attempts: u32) -> Option<MutexGuard<'static, T>> {
    for _ in 0..attempts {
        match m.try_lock() {
            Ok(g) => return Some(g),
            Err(TryLockError::Poisoned(p)) => return Some(p.into_inner()),
            Err(TryLockError::WouldBlock) => core::hint::spin_loop(),
        }
    }
    None
}

/// Longest `<dir>/crash-<pid>-` accepted by [`install`]. The dump path is built
/// on the stack of a signal handler, which may be a small alternate stack.
const PREFIX_MAX: usize = 200;
const PATH_BUF: usize = PREFIX_MAX + 10 + 6 + 1;

/// `<prefix><seq>.rtlog\0` into `buf`, without allocating or formatting.
fn dump_path(buf: &mut [u8; PATH_BUF], prefix: &[u8], seq: u32) -> Option<usize> {
    let mut digits = [0u8; 10];
    let mut n = 0;
    let mut v = seq;
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    let suffix = b".rtlog";
    let total = prefix.len() + n + suffix.len() + 1;
    if total > buf.len() {
        return None;
    }
    buf[..prefix.len()].copy_from_slice(prefix);
    let mut i = prefix.len();
    while n > 0 {
        n -= 1;
        buf[i] = digits[n];
        i += 1;
    }
    buf[i..i + suffix.len()].copy_from_slice(suffix);
    i += suffix.len();
    buf[i] = 0;
    Some(i + 1)
}

/// A control frame: id, ctx varint, payload.
fn control(fd: i32, id: u32, ctx: u32, payload: &[u8]) {
    let mut b = [0u8; 4 + 5 + 1 + 128];
    b[..4].copy_from_slice(&id.to_le_bytes());
    let mut n = 4 + varint::encode(u64::from(ctx), &mut b[4..]);
    let p = payload.len().min(b.len() - n);
    b[n..n + p].copy_from_slice(&payload[..p]);
    n += p;
    write_all(fd, &b[..n]);
}

/// Unread frames of one ring, preceded by its name, drops and time base.
fn dump_ring(fd: i32, shared: &Shared, scratch: &mut [u8; FRAME_MAX]) {
    let ring = &shared.ring;
    let ctx = ring.ctx();
    let mut name = [0u8; 1 + 64];
    let n = match shared.name.try_lock() {
        Ok(s) => {
            let b = &s.as_bytes()[..s.len().min(64)];
            name[1..=b.len()].copy_from_slice(b);
            b.len()
        }
        Err(_) => 0,
    };
    name[0] = n as u8;
    control(fd, wire::CTRL_CTX_REGISTER, ctx, &name[..=n]);
    let drops = ring.drops();
    if drops > 0 {
        control(fd, wire::CTRL_DROPPED, ctx, &drops.to_le_bytes());
    }
    // The first unread frame's absolute time: the newest timestamp minus the
    // deltas of every unread frame.
    let mut deltas: u64 = 0;
    // SAFETY: the registry lock is held, so the drain cannot consume meanwhile.
    unsafe {
        ring.scan(scratch, |f| {
            if let Some((d, _)) = f.get(4..).and_then(varint::decode) {
                deltas = deltas.saturating_add(d);
            }
            true
        })
    };
    let base = ring.last_ts_racy().saturating_sub(deltas);
    control(fd, wire::CTRL_CTX_TIME, ctx, &base.to_le_bytes());
    // SAFETY: as above.
    unsafe {
        ring.scan(scratch, |f| {
            write_all(fd, f);
            true
        })
    };
}

fn dump(kind: u8, sig: i32, code: i32, addr: u64, pc: u64, msg: &str, fatal: bool) {
    if fatal {
        if FATAL.swap(true, Ordering::AcqRel) {
            return;
        }
        crate::level::set_off(); // freeze: later log calls are no-ops
    } else if FATAL.load(Ordering::Acquire) {
        return;
    }
    let (Some(prefix), Some(header)) = (PREFIX.get(), HEADER.get()) else {
        return;
    };
    let seq = SEQ.fetch_add(1, Ordering::AcqRel) + 1;
    let mut path = [0u8; PATH_BUF];
    if dump_path(&mut path, prefix.as_bytes(), seq).is_none() {
        return;
    }
    // SAFETY: open(2) is async-signal-safe; `path` is NUL-terminated.
    let fd = unsafe {
        libc::open(
            path.as_ptr().cast(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
            0o644 as libc::c_uint,
        )
    };
    if fd < 0 {
        return;
    }
    write_all(fd, header);

    let mut scratch = [0u8; FRAME_MAX];
    // Best effort to keep the drain out while we read: if it is mid-pass
    // (blocked in a transport write, say) we go on without it. Fatal dumps
    // have frozen the producers, so a frame can then only be duplicated, not
    // torn; `scan` stops at a frame the drain released meanwhile.
    let _drain_out = try_lock_for(&REGISTRY, 1_000);
    // The ring list is never held for long, so this practically always succeeds.
    if let Some(rings) = try_lock_for(&RINGS, 10_000) {
        for shared in rings.iter() {
            dump_ring(fd, shared, &mut scratch);
        }
    }

    // The crash record, on the crashing thread's context.
    let ring = FAST.try_with(|f| f.get()).unwrap_or(core::ptr::null());
    // SAFETY: FAST points at this thread's live ring or is null.
    let (ctx, last) = match unsafe { ring.as_ref() } {
        Some(r) => (r.ctx(), r.last_ts_racy()),
        None => {
            control(fd, wire::CTRL_CTX_TIME, 0, &0u64.to_le_bytes());
            (0, 0)
        }
    };
    let mut b = [0u8; 4 + 10 + 5 + 1 + 4 + 4 + 8 + 8 + 1 + 127];
    b[..4].copy_from_slice(&wire::CTRL_CRASH.to_le_bytes());
    let mut n = 4;
    n += varint::encode(crate::clock::now().saturating_sub(last), &mut b[n..]);
    n += varint::encode(u64::from(ctx), &mut b[n..]);
    b[n] = kind;
    n += 1;
    for part in [
        &sig.to_le_bytes()[..],
        &code.to_le_bytes()[..],
        &addr.to_le_bytes()[..],
        &pc.to_le_bytes()[..],
    ] {
        b[n..n + part.len()].copy_from_slice(part);
        n += part.len();
    }
    let mut m = msg.len().min(127);
    while !msg.is_char_boundary(m) {
        m -= 1;
    }
    b[n] = m as u8 | if m < msg.len() { 0x80 } else { 0 };
    n += 1;
    b[n..n + m].copy_from_slice(&msg.as_bytes()[..m]);
    n += m;
    write_all(fd, &b[..n]);
    // SAFETY: close(2) is async-signal-safe.
    unsafe { libc::close(fd) };
}
