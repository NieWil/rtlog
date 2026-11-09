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
//! Each dump is a new file, opened with `O_EXCL`, so nothing is ever
//! overwritten: not by a caught panic or a watchdog [`snapshot`] in the same
//! process, not by a restart (even when containers reuse pid 1: the number then
//! just counts on), and not by a forked child (its pid differs). Fatal dumps
//! are always written; panics and snapshots stop after [`MAX_SOFT_DUMPS`] per
//! process, so a service that catches a panic per request cannot fill the disk.
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
//!
//! The fatal dump runs on its own stack of [`SIGNAL_STACK`] bytes with a guard
//! page below it, mapped by [`install`]: the handler claims the dump on the
//! crashing thread's signal stack, then switches to the dump stack (x86-64 and
//! aarch64), so the dump does not depend on which thread crashed. The default
//! signal stack Rust gives a thread is `max(SIGSTKSZ, AT_MINSIGSTKSZ)`, often
//! 8 KiB, and the CPU's saved state takes up to a few KiB of it: too little for
//! a debug build's dump. On other architectures the dump runs on the signal
//! stack, which [`install`] enlarges to [`SIGNAL_STACK`] for the calling thread
//! and every thread that logs after it.

use std::cell::OnceCell;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError};

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

/// Panics and snapshots written per process before further ones are skipped.
/// Fatal signals ignore the cap.
pub const MAX_SOFT_DUMPS: u32 = 16;

/// `<dir>/crash-`; a dump appends `<pid>-<n>.rtlog`, with the pid read when
/// the dump is written (it differs in a forked child).
static PREFIX: OnceLock<CString> = OnceLock::new();
static HEADER: OnceLock<[u8; HEADER_LEN]> = OnceLock::new();
/// Next number to try in a dump's name.
static NEXT: AtomicU32 = AtomicU32::new(1);
/// Dumps written so far by this process.
static WRITTEN: AtomicU32 = AtomicU32::new(0);
/// Panic and snapshot dumps started (for [`MAX_SOFT_DUMPS`]).
static SOFT: AtomicU32 = AtomicU32::new(0);
/// Set by the first fatal dump: a second fatal signal (another thread, or a
/// fault inside the handler) does not dump again, it waits for this one.
static FATAL: AtomicBool = AtomicBool::new(false);
/// Set when the fatal dump is finished.
static DUMP_DONE: AtomicBool = AtomicBool::new(false);

/// Bytes of alternate signal stack the handler runs on (see the module docs).
pub const SIGNAL_STACK: usize = 64 * 1024;

thread_local! {
    /// This thread's own signal stack, if it needed one; unmapped at thread exit.
    static OWN_STACK: OnceCell<Option<SignalStack>> = const { OnceCell::new() };
}

/// Give the calling thread a [`SIGNAL_STACK`]-byte alternate signal stack
/// unless it already has one at least that large. Called by [`install`] and
/// when a thread registers its ring; does nothing before [`install`].
pub(crate) fn prepare_thread() {
    if PREFIX.get().is_none() {
        return;
    }
    let _ = OWN_STACK.try_with(|s| {
        s.get_or_init(SignalStack::new);
    });
}

/// An `mmap`ed alternate signal stack with a guard page below it, so an
/// overflow faults instead of overwriting whatever lies below.
struct SignalStack {
    base: *mut libc::c_void,
    len: usize,
    sp: *mut libc::c_void,
}

impl SignalStack {
    fn new() -> Option<SignalStack> {
        // SAFETY: plain sigaltstack/mmap/mprotect calls on memory we own.
        unsafe {
            let mut cur: libc::stack_t = core::mem::zeroed();
            if libc::sigaltstack(core::ptr::null(), &mut cur) != 0
                || cur.ss_flags & libc::SS_ONSTACK != 0
                || (cur.ss_flags & libc::SS_DISABLE == 0 && cur.ss_size >= SIGNAL_STACK)
            {
                return None;
            }
            let page = match libc::sysconf(libc::_SC_PAGESIZE) {
                n if n > 0 => n as usize,
                _ => 4096,
            };
            let len = SIGNAL_STACK + page;
            let base = libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            );
            if base == libc::MAP_FAILED {
                return None;
            }
            let sp = base.cast::<u8>().add(page).cast::<libc::c_void>();
            let st = libc::stack_t {
                ss_sp: sp,
                ss_flags: 0,
                ss_size: SIGNAL_STACK,
            };
            if libc::mprotect(base, page, libc::PROT_NONE) != 0
                || libc::sigaltstack(&st, core::ptr::null_mut()) != 0
            {
                libc::munmap(base, len);
                return None;
            }
            Some(SignalStack { base, len, sp })
        }
    }
}

impl Drop for SignalStack {
    fn drop(&mut self) {
        // SAFETY: as above; the stack is disabled before it is unmapped.
        unsafe {
            let mut cur: libc::stack_t = core::mem::zeroed();
            if libc::sigaltstack(core::ptr::null(), &mut cur) == 0 && cur.ss_sp == self.sp {
                if cur.ss_flags & libc::SS_ONSTACK != 0 {
                    return; // still running on it: leak rather than unmap it
                }
                let off = libc::stack_t {
                    ss_sp: core::ptr::null_mut(),
                    ss_flags: libc::SS_DISABLE,
                    ss_size: 0,
                };
                libc::sigaltstack(&off, core::ptr::null_mut());
            }
            libc::munmap(self.base, self.len);
        }
    }
}

/// Install the crash handler. Dumps go to `dir` (created if missing) as
/// `crash-<pid>-<n>.rtlog`; `n` counts up from 1 and skips names that exist.
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
    prefix.extend_from_slice(b"crash-");
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
    map_dump_stack();
    prepare_thread();

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
            // No SA_RESETHAND: it restores the default action the moment the first
            // handler starts, so a second thread faulting during the dump would be
            // killed on the spot and cut the dump short. The handler resets the
            // default itself, after the dump.
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
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

/// Number of dumps this process has written.
pub fn dumps() -> u32 {
    WRITTEN.load(Ordering::Acquire)
}

/// In a coverage build (`cargo llvm-cov` sets `cfg(coverage)`), save this
/// process's coverage counters now: a process killed by a signal never
/// reaches the exit hook that normally writes them, so the crash tests' code
/// would count as never run. Not async-signal-safe, which is acceptable in a
/// coverage build only; normal builds compile this to nothing.
fn write_coverage_profile() {
    #[cfg(coverage)]
    {
        extern "C" {
            fn __llvm_profile_write_file() -> core::ffi::c_int;
        }
        // SAFETY: provided by the profiler runtime that coverage builds link.
        unsafe { __llvm_profile_write_file() };
    }
}

/// Arguments of the fatal signal dump, passed to [`signal_dump`] through a pointer.
struct SignalDump {
    sig: i32,
    code: i32,
    addr: u64,
    pc: u64,
}

/// The fatal signal dump, called on the dump stack by [`on_dump_stack`].
extern "C" fn signal_dump(arg: *mut libc::c_void) {
    // SAFETY: `on_signal` passes a pointer to a live `SignalDump`.
    let a = unsafe { &*(arg as *const SignalDump) };
    dump_body(
        wire::CRASH_SIGNAL,
        a.sig,
        a.code,
        a.addr,
        a.pc,
        signal_name(a.sig),
        true,
    );
}

/// Top of the dump stack: [`SIGNAL_STACK`] bytes with a guard page below,
/// mapped by [`install`]. Null when it could not be mapped.
static DUMP_STACK_TOP: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());

/// Map the dump stack once. A fatal dump runs on it whatever stack the
/// crashing thread has: a thread that never logged still has the small
/// default signal stack, too small for a debug build's dump.
fn map_dump_stack() {
    if !DUMP_STACK_TOP.load(Ordering::Acquire).is_null() {
        return;
    }
    // SAFETY: plain mmap/mprotect on memory this function owns; it is never unmapped.
    unsafe {
        let page = match libc::sysconf(libc::_SC_PAGESIZE) {
            n if n > 0 => n as usize,
            _ => 4096,
        };
        let len = SIGNAL_STACK + page;
        let base = libc::mmap(
            core::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        );
        if base == libc::MAP_FAILED {
            return;
        }
        if libc::mprotect(base, page, libc::PROT_NONE) != 0 {
            libc::munmap(base, len);
            return;
        }
        // mmap returns page-aligned memory, so the top is 16-byte aligned.
        DUMP_STACK_TOP.store(base.cast::<u8>().add(len), Ordering::Release);
    }
}

/// Call `f(arg)` on the dump stack, or on the current stack where it is not
/// mapped or the architecture has no stack switch here (then the per-thread
/// signal stack from [`prepare_thread`] is what the dump runs on).
///
/// # Safety
///
/// One caller at a time (the thread that won [`claim_fatal`]); `f` must not
/// unwind.
unsafe fn on_dump_stack(f: extern "C" fn(*mut libc::c_void), arg: *mut libc::c_void) {
    let top = DUMP_STACK_TOP.load(Ordering::Acquire);
    if top.is_null() {
        f(arg);
        return;
    }
    #[cfg(target_arch = "x86_64")]
    // SAFETY: `top` is the 16-byte-aligned end of a mapped, unused stack; the
    // old stack pointer is kept in callee-saved r12 across the call.
    core::arch::asm!(
        "mov r12, rsp",
        "mov rsp, {top}",
        "call {f}",
        "mov rsp, r12",
        top = in(reg) top,
        f = in(reg) f,
        in("rdi") arg,
        out("r12") _,
        clobber_abi("C"),
    );
    #[cfg(target_arch = "aarch64")]
    // SAFETY: as above; the old stack pointer is kept in callee-saved x20.
    core::arch::asm!(
        "mov x20, sp",
        "mov sp, {top}",
        "blr {f}",
        "mov sp, x20",
        top = in(reg) top,
        f = in(reg) f,
        in("x0") arg,
        out("x20") _,
        clobber_abi("C"),
    );
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    f(arg);
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
    if claim_fatal() {
        let mut args = SignalDump {
            sig,
            code,
            addr,
            pc: pc_of(uctx),
        };
        // SAFETY: only the thread that claimed the fatal dump gets here, so the
        // dump stack has one user; `args` outlives the call.
        unsafe { on_dump_stack(signal_dump, (&mut args as *mut SignalDump).cast()) };
        write_coverage_profile();
    }
    // Restore the default action and re-raise to get it (core dump, exit status
    // that names the signal). The signal is blocked while this handler runs, so
    // it stays pending and takes effect when the handler returns.
    // SAFETY: sigaction(2) and raise(3) are async-signal-safe.
    unsafe {
        let mut dfl: libc::sigaction = core::mem::zeroed();
        dfl.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut dfl.sa_mask);
        libc::sigaction(sig, &dfl, core::ptr::null_mut());
        libc::raise(sig);
    }
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

/// Longest `<dir>/crash-` accepted by [`install`]. The dump path is built on
/// the stack of a signal handler, which may be a small alternate stack.
const PREFIX_MAX: usize = 200;
/// Prefix, `<pid>-<n>` (two u32s), ".rtlog", NUL.
const PATH_BUF: usize = PREFIX_MAX + 10 + 1 + 10 + 6 + 1;

/// Append `v` in decimal at `buf[i..]`; returns the new length.
fn put_dec(buf: &mut [u8], mut i: usize, mut v: u32) -> usize {
    let mut digits = [0u8; 10];
    let mut n = 0;
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    while n > 0 {
        n -= 1;
        buf[i] = digits[n];
        i += 1;
    }
    i
}

/// `<prefix><pid>-<n>.rtlog\0` into `buf`, without allocating or formatting.
fn dump_path(buf: &mut [u8; PATH_BUF], prefix: &[u8], pid: u32, n: u32) -> usize {
    buf[..prefix.len()].copy_from_slice(prefix);
    let mut i = put_dec(buf, prefix.len(), pid);
    buf[i] = b'-';
    i = put_dec(buf, i + 1, n);
    let suffix = b".rtlog";
    buf[i..i + suffix.len()].copy_from_slice(suffix);
    i += suffix.len();
    buf[i] = 0;
    i + 1
}

/// Create the next dump file: `O_EXCL`, so an existing file (an earlier run
/// that shared this pid, as in containers) is never touched; the number moves
/// on instead. Returns the descriptor.
fn open_dump(prefix: &[u8]) -> Option<i32> {
    // SAFETY: getpid(2) is async-signal-safe.
    let pid = unsafe { libc::getpid() } as u32;
    let mut path = [0u8; PATH_BUF];
    for _ in 0..1_000 {
        let n = NEXT.fetch_add(1, Ordering::AcqRel);
        dump_path(&mut path, prefix, pid, n);
        // SAFETY: open(2) is async-signal-safe; `path` is NUL-terminated.
        let fd = unsafe {
            libc::open(
                path.as_ptr().cast(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                0o644 as libc::c_uint,
            )
        };
        if fd >= 0 {
            return Some(fd);
        }
        if io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
            return None;
        }
    }
    None
}

/// Block (without sleeping in a lock) until the fatal dump in progress is done
/// or about two seconds pass: a second crashing thread must not race the first
/// one to the default action, which ends the process mid-write.
fn wait_for_dump() {
    let ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 1_000_000,
    };
    for _ in 0..2_000 {
        if DUMP_DONE.load(Ordering::Acquire) {
            return;
        }
        // SAFETY: nanosleep(2) is async-signal-safe.
        unsafe { libc::nanosleep(&ts, core::ptr::null_mut()) };
    }
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
            let mut k = s.len().min(64);
            while !s.is_char_boundary(k) {
                k -= 1;
            }
            name[1..=k].copy_from_slice(&s.as_bytes()[..k]);
            k
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
    // SAFETY: `scan` only reads; if the drain consumes meanwhile it notices
    // (it re-checks `tail`) and stops rather than hand out overwritten bytes.
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
        if !claim_fatal() {
            return;
        }
    } else if FATAL.load(Ordering::Acquire) || SOFT.fetch_add(1, Ordering::AcqRel) >= MAX_SOFT_DUMPS
    {
        return;
    }
    dump_body(kind, sig, code, addr, pc, msg, fatal);
}

/// Claim the one fatal dump of the process. `false` when another thread (or
/// this one, inside its own handler) already has it: this waits for that dump
/// to finish first, so the default action does not end the process under it.
fn claim_fatal() -> bool {
    if FATAL.swap(true, Ordering::AcqRel) {
        wait_for_dump();
        return false;
    }
    crate::level::set_off(); // freeze: later log calls are no-ops
    true
}

/// Write one dump. Fatal dumps call this only after [`claim_fatal`].
fn dump_body(kind: u8, sig: i32, code: i32, addr: u64, pc: u64, msg: &str, fatal: bool) {
    let (Some(prefix), Some(header)) = (PREFIX.get(), HEADER.get()) else {
        return;
    };
    let Some(fd) = open_dump(prefix.as_bytes()) else {
        if fatal {
            DUMP_DONE.store(true, Ordering::Release);
        }
        return;
    };
    write_all(fd, header);

    let mut scratch = [0u8; FRAME_MAX];
    // Best effort to keep the drain out while we read: if it is mid-pass
    // (blocked in a transport write, say) we go on without it. Fatal dumps
    // have frozen the producers, so a frame can then only be duplicated, not
    // torn; `scan` stops at a frame the drain released meanwhile.
    let _drain_out = try_lock_for(&REGISTRY, 1_000);
    // The ring list is held only for a push or a removal, so this practically
    // always succeeds.
    if let Some(rings) = try_lock_for(&RINGS, 10_000) {
        if fatal {
            // In a signal handler: no allocation. Hold the list while writing;
            // the process is ending anyway.
            for shared in rings.iter() {
                dump_ring(fd, shared, &mut scratch);
            }
        } else {
            // Ordinary thread context (panic hook, snapshot): copy the handles
            // out and release the list, so a slow disk never stalls a thread
            // that is starting, exiting or being retired.
            let owned: Vec<Arc<Shared>> = rings.iter().cloned().collect();
            drop(rings);
            for shared in &owned {
                dump_ring(fd, shared, &mut scratch);
            }
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
    WRITTEN.fetch_add(1, Ordering::AcqRel);
    if fatal {
        DUMP_DONE.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[test]
    fn every_handled_signal_has_a_name() {
        assert_eq!(signal_name(libc::SIGSEGV), "SIGSEGV: invalid memory access");
        assert_eq!(signal_name(libc::SIGBUS), "SIGBUS: bus error");
        assert!(signal_name(libc::SIGFPE).starts_with("SIGFPE: "));
        assert_eq!(signal_name(libc::SIGILL), "SIGILL: illegal instruction");
        assert_eq!(signal_name(libc::SIGABRT), "SIGABRT: abort");
        assert_eq!(signal_name(libc::SIGUSR1), "signal");
    }

    #[test]
    fn no_context_means_no_pc() {
        assert_eq!(pc_of(core::ptr::null_mut()), 0);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn pc_is_read_from_the_saved_context() {
        // SAFETY: a zeroed ucontext_t is a valid value.
        let mut uc: libc::ucontext_t = unsafe { core::mem::zeroed() };
        uc.uc_mcontext.gregs[libc::REG_RIP as usize] = 0x1234_5678;
        assert_eq!(
            pc_of((&mut uc as *mut libc::ucontext_t).cast()),
            0x1234_5678
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    #[test]
    fn pc_is_read_from_the_saved_context() {
        // SAFETY: a zeroed ucontext_t is a valid value.
        let mut uc: libc::ucontext_t = unsafe { core::mem::zeroed() };
        uc.uc_mcontext.pc = 0x1234_5678;
        assert_eq!(
            pc_of((&mut uc as *mut libc::ucontext_t).cast()),
            0x1234_5678
        );
    }

    #[test]
    fn try_lock_for_gives_up_on_a_held_lock_and_takes_a_poisoned_one() {
        static M: Mutex<u32> = Mutex::new(7);
        assert_eq!(try_lock_for(&M, 1).map(|g| *g), Some(7));
        let held = M.lock().unwrap();
        assert!(try_lock_for(&M, 100).is_none());
        drop(held);
        let _ = std::thread::spawn(|| {
            let _g = M.lock().unwrap();
            panic!("poison the lock");
        })
        .join();
        assert!(M.is_poisoned());
        assert_eq!(try_lock_for(&M, 1).map(|g| *g), Some(7));
    }

    #[test]
    fn write_all_writes_everything_and_stops_on_an_error() {
        let mut fds = [0i32; 2];
        // SAFETY: pipe(2) fills the two descriptors.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        write_all(fds[1], b"hello");
        let mut buf = [0u8; 8];
        // SAFETY: `buf` is valid for its length.
        let n = unsafe { libc::read(fds[0], buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(&buf[..n as usize], b"hello");
        // SAFETY: closing the descriptors pipe(2) gave us.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        // An invalid descriptor: it returns instead of retrying forever.
        write_all(-1, b"lost");
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
    }

    #[test]
    fn a_thread_gets_one_signal_stack_and_loses_it_on_drop() {
        std::thread::spawn(|| {
            // Start from no signal stack on every OS: Rust gives a thread 8 KiB
            // on most Linux x86-64 CPUs but 128 KiB on macOS, which is large
            // enough that `new` would rightly keep it.
            let off = libc::stack_t {
                ss_sp: core::ptr::null_mut(),
                ss_flags: libc::SS_DISABLE,
                ss_size: 0,
            };
            // SAFETY: disables this thread's alternate signal stack.
            assert_eq!(unsafe { libc::sigaltstack(&off, core::ptr::null_mut()) }, 0);
            let s = SignalStack::new().expect("a thread without a signal stack gets one");
            // SAFETY: sigaltstack(2) queries into a zeroed stack_t.
            let mut cur: libc::stack_t = unsafe { core::mem::zeroed() };
            assert_eq!(unsafe { libc::sigaltstack(core::ptr::null(), &mut cur) }, 0);
            assert_eq!((cur.ss_sp, cur.ss_size), (s.sp, SIGNAL_STACK));
            assert!(SignalStack::new().is_none(), "a large enough stack is kept");
            drop(s);
            assert_eq!(unsafe { libc::sigaltstack(core::ptr::null(), &mut cur) }, 0);
            assert_ne!(
                cur.ss_flags & libc::SS_DISABLE,
                0,
                "disabled before unmapping"
            );
        })
        .join()
        .unwrap();
    }

    extern "C" fn record_stack_address(arg: *mut libc::c_void) {
        let local = 0u8;
        // SAFETY: the caller passes a pointer to a live usize.
        unsafe { *(arg as *mut usize) = core::hint::black_box(&local) as *const u8 as usize };
    }

    #[test]
    fn the_dump_runs_on_its_own_stack_once_it_is_mapped() {
        // Nothing else in this test binary maps the dump stack (`install`
        // runs only in child processes).
        assert!(DUMP_STACK_TOP.load(Ordering::Acquire).is_null());
        let marker = core::hint::black_box(0u8);
        let here = core::hint::black_box(&marker) as *const u8 as usize;
        let mut seen = 0usize;
        // SAFETY: one caller; `record_stack_address` does not unwind.
        unsafe { on_dump_stack(record_stack_address, (&mut seen as *mut usize).cast()) };
        assert!(
            here.abs_diff(seen) < 64 * 1024,
            "unmapped: runs on this stack"
        );

        map_dump_stack();
        let top = DUMP_STACK_TOP.load(Ordering::Acquire);
        assert!(!top.is_null());
        map_dump_stack();
        assert_eq!(DUMP_STACK_TOP.load(Ordering::Acquire), top, "mapped once");
        // SAFETY: as above.
        unsafe { on_dump_stack(record_stack_address, (&mut seen as *mut usize).cast()) };
        let top = top as usize;
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        assert!(
            (top - SIGNAL_STACK..top).contains(&seen),
            "{seen:#x} not in the dump stack below {top:#x}"
        );
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        assert!(here.abs_diff(seen) < 64 * 1024, "no stack switch here");
    }

    // Child processes: `install` and the fatal path are process-wide.

    const CHILD_ENV: &str = "RTLOG_CRASH_UNIT_CHILD";
    const DIR_ENV: &str = "RTLOG_CRASH_UNIT_DIR";

    /// The crash record at the end of a dump.
    #[derive(Debug, PartialEq)]
    struct Record {
        ctx: u64,
        kind: u8,
        sig: i32,
        code: i32,
        addr: u64,
        pc: u64,
        msg: Vec<u8>,
    }

    fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).rposition(|w| w == needle)
    }

    fn count(hay: &[u8], needle: &[u8]) -> usize {
        hay.windows(needle.len()).filter(|w| *w == needle).count()
    }

    fn crash_record(b: &[u8]) -> Record {
        let at = find(b, &wire::CTRL_CRASH.to_le_bytes()).expect("a crash record");
        let mut p = at + 4;
        let (_, n) = varint::decode(&b[p..]).unwrap();
        p += n;
        let (ctx, n) = varint::decode(&b[p..]).unwrap();
        p += n;
        let le4 = |p: usize| i32::from_le_bytes(b[p..p + 4].try_into().unwrap());
        let le8 = |p: usize| u64::from_le_bytes(b[p..p + 8].try_into().unwrap());
        let len = b[p + 25];
        let r = Record {
            ctx,
            kind: b[p],
            sig: le4(p + 1),
            code: le4(p + 5),
            addr: le8(p + 9),
            pc: le8(p + 17),
            msg: b[p + 26..].to_vec(),
        };
        assert_eq!(
            r.msg.len(),
            usize::from(len & 0x7f),
            "the record ends the file"
        );
        r
    }

    /// Dump files in `dir`, lowest number first.
    fn dumps_in(dir: &Path) -> Vec<Vec<u8>> {
        let mut files: Vec<(u32, PathBuf)> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                let name = p.file_stem().unwrap().to_string_lossy().into_owned();
                (name.rsplit('-').next().unwrap().parse().unwrap(), p)
            })
            .collect();
        files.sort();
        files
            .iter()
            .map(|(_, p)| std::fs::read(p).unwrap())
            .collect()
    }

    fn site_id_of(fmt: &str) -> [u8; 4] {
        let s = crate::site::sites().iter().find(|s| s.fmt == fmt).unwrap();
        s.id.to_le_bytes()
    }

    /// The child half: runs only when a parent test below sets `CHILD_ENV`.
    #[test]
    fn unit_child() {
        let Ok(mode) = std::env::var(CHILD_ENV) else {
            return;
        };
        let dir = PathBuf::from(std::env::var(DIR_ENV).unwrap());
        match mode.as_str() {
            "fatal" => child_fatal(&dir),
            "nodir" => child_nodir(&dir),
            "exhausted" => child_exhausted(&dir),
            "locks" => child_locks(&dir),
            m => panic!("unknown mode {m}"),
        }
    }

    /// The fatal path of `on_signal` without the signal: claim, dump on the
    /// dump stack, and a second thread that waits for the dump to finish.
    fn child_fatal(dir: &Path) {
        install(dir).unwrap();
        for i in 0..10u32 {
            crate::info!("fatal child i={}", i);
        }
        assert!(claim_fatal());
        assert_eq!(crate::level::level(), None, "logging is frozen");
        static STARTED: AtomicBool = AtomicBool::new(false);
        let waiter = std::thread::spawn(|| {
            STARTED.store(true, Ordering::Release);
            let t = Instant::now();
            (claim_fatal(), t.elapsed())
        });
        while !STARTED.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            !waiter.is_finished(),
            "the second claimant waits for the dump"
        );
        let mut args = SignalDump {
            sig: libc::SIGSEGV,
            code: 1,
            addr: 0xdead,
            pc: 0xbeef,
        };
        // SAFETY: this thread claimed the fatal dump; `args` outlives the call.
        unsafe { on_dump_stack(signal_dump, (&mut args as *mut SignalDump).cast()) };
        let (second, waited) = waiter.join().unwrap();
        assert!(!second, "the second claimant does not dump");
        assert!(
            waited < Duration::from_secs(2),
            "released by the dump, not the timeout"
        );
        assert!(DUMP_DONE.load(Ordering::Acquire));
        assert_eq!(dumps(), 1);

        // Later fatal and soft dumps are skipped.
        dump(wire::CRASH_SIGNAL, libc::SIGBUS, 0, 0, 0, "again", true);
        snapshot("after the fatal dump");
        assert_eq!(dumps(), 1);

        let files = dumps_in(dir);
        assert_eq!(files.len(), 1);
        let b = &files[0];
        assert_eq!(&b[..HEADER_LEN], HEADER.get().unwrap());
        assert_eq!(count(b, &site_id_of("fatal child i={}")), 10, "every frame");
        let msg = signal_name(libc::SIGSEGV).as_bytes();
        let r = crash_record(b);
        assert_eq!(
            (r.kind, r.sig, r.code, r.addr, r.pc, r.msg.as_slice()),
            (wire::CRASH_SIGNAL, libc::SIGSEGV, 1, 0xdead, 0xbeef, msg)
        );
        assert_ne!(r.ctx, 0, "on the crashing thread's context");
    }

    /// The dump directory disappeared: no dump, and no hang.
    fn child_nodir(dir: &Path) {
        install(dir).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        snapshot("nowhere to go");
        assert_eq!(dumps(), 0);
        assert!(claim_fatal());
        let mut args = SignalDump {
            sig: libc::SIGABRT,
            code: 0,
            addr: 0,
            pc: 0,
        };
        signal_dump((&mut args as *mut SignalDump).cast());
        assert!(DUMP_DONE.load(Ordering::Acquire), "waiters are released");
        assert_eq!(dumps(), 0);
        assert!(!dir.exists());
    }

    /// 1 000 names in a row are taken: give up rather than loop, and use the
    /// next free number afterwards.
    fn child_exhausted(dir: &Path) {
        install(dir).unwrap();
        let pid = std::process::id();
        let first = NEXT.load(Ordering::Acquire);
        for n in first..first + 1_000 {
            std::fs::write(dir.join(std::format!("crash-{pid}-{n}.rtlog")), b"taken").unwrap();
        }
        snapshot("no free name");
        assert_eq!(dumps(), 0);
        assert_eq!(NEXT.load(Ordering::Acquire), first + 1_000);
        snapshot("a free name again");
        assert_eq!(dumps(), 1);
        let new = dir.join(std::format!("crash-{pid}-{}.rtlog", first + 1_000));
        let r = crash_record(&std::fs::read(new).unwrap());
        assert_eq!(
            (r.kind, r.msg.as_slice()),
            (CRASH_SNAPSHOT, &b"a free name again"[..])
        );
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 1_001);
    }

    /// A busy name lock gives an empty name; a busy ring list gives a dump
    /// with only the header and the crash record.
    fn child_locks(dir: &Path) {
        install(dir).unwrap();
        std::thread::Builder::new()
            .name("held".into())
            .spawn(|| crate::info!("from held {}", 1u8))
            .unwrap()
            .join()
            .unwrap();
        crate::info!("from main {}", 2u8);
        let held: Arc<Shared> = RINGS
            .lock()
            .unwrap()
            .iter()
            .find(|s| *s.name.lock().unwrap() == "held")
            .unwrap()
            .clone();
        {
            let _name = held.name.lock().unwrap();
            snapshot("name busy");
        }
        {
            let _rings = RINGS.lock().unwrap();
            snapshot("list busy");
        }
        assert_eq!(dumps(), 2);
        let files = dumps_in(dir);

        let mut empty_name = wire::CTRL_CTX_REGISTER.to_le_bytes().to_vec();
        let mut ctx = [0u8; 5];
        let n = varint::encode(u64::from(held.ring.ctx()), &mut ctx);
        empty_name.extend_from_slice(&ctx[..n]);
        empty_name.push(0);
        assert!(
            find(&files[0], &empty_name).is_some(),
            "announced without a name"
        );
        assert!(find(&files[0], b"held").is_none());
        assert_eq!(count(&files[0], &site_id_of("from held {}")), 1);

        let b = &files[1];
        assert_eq!(
            &b[HEADER_LEN..HEADER_LEN + 4],
            &wire::CTRL_CRASH.to_le_bytes()
        );
        assert_eq!(crash_record(b).msg, b"list busy");
    }

    /// Run `unit_child` in `mode` in a new process; returns its dump directory.
    fn run_child(mode: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(std::format!(
            "rtlog-crash-unit-{}-{mode}",
            std::process::id()
        ));
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
                let mut c = std::process::Command::new(parts.next().unwrap());
                c.args(parts).arg(&exe);
                c
            }
            None => std::process::Command::new(&exe),
        };
        let out = cmd
            .args([
                "--exact",
                "crash::tests::unit_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, mode)
            .env(DIR_ENV, &dir)
            .env("RUST_BACKTRACE", "0")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "child {mode}: {:?}\nstdout:\n{stdout}\nstderr:\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(stdout.contains("1 passed"), "the child test ran:\n{stdout}");
        dir
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn a_fatal_dump_runs_once_and_a_second_claimant_waits_for_it() {
        let dir = run_child("fatal");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn a_missing_dump_directory_writes_nothing_and_does_not_hang() {
        let dir = run_child("nodir");
        assert!(!dir.exists());
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn dump_names_are_tried_a_bounded_number_of_times() {
        let dir = run_child("exhausted");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn busy_locks_shorten_the_dump_instead_of_blocking_it() {
        let dir = run_child("locks");
        std::fs::remove_dir_all(dir).ok();
    }
}
