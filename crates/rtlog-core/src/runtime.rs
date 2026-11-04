//! `std` runtime: one heap ring per thread, created on the thread's first log
//! call, and a drain that serves all of them (spec sections 7 and 8).
//!
//! Locking: producers never take a lock on the log path. A thread's first log
//! call registers its ring through `PENDING`, a lock the drain holds only to
//! move new entries out. `REGISTRY` belongs to the drain alone. A transport may
//! therefore log, or call [`stats`], from inside `write` without deadlocking.

use std::cell::{Cell, OnceCell};
use std::string::String;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;
use std::vec::Vec;

use crate::drain::{self, Full, Transport};
use crate::ring::{Ring, FRAME_MAX};
use crate::wire;

/// Ring size per thread on `std`.
pub const THREAD_RING: usize = 64 * 1024;

pub(crate) type ThreadRing = Ring<THREAD_RING>;

/// State shared between a thread and the drain.
pub(crate) struct Shared {
    pub(crate) ring: ThreadRing,
    pub(crate) name: Mutex<String>,
    /// Bumped on rename so the drain re-announces the context.
    name_gen: AtomicU32,
}

pub(crate) struct Entry {
    pub(crate) shared: Arc<Shared>,
    announced_gen: u32,
}

pub(crate) static PENDING: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
pub(crate) static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
/// Every live ring, for the crash dump. Held only for a push or a removal, so
/// a `try_lock` from a signal handler almost always succeeds; `REGISTRY`, by
/// contrast, is held for a whole drain pass, transport writes included.
pub(crate) static RINGS: Mutex<Vec<Arc<Shared>>> = Mutex::new(Vec::new());
/// Set by the first drain pass. Before it, exited threads' rings are kept only
/// up to [`KEEP_WITHOUT_DRAIN`], so a program that never installs a drain does
/// not keep one 64 KiB ring per thread it ever ran.
static DRAIN_SEEN: AtomicBool = AtomicBool::new(false);
/// How many exited threads' rings are kept while no drain has run: enough for
/// a crash dump to show the last finished workers, bounded at 1 MiB.
pub const KEEP_WITHOUT_DRAIN: usize = 16;
static NEXT_CTX: AtomicU32 = AtomicU32::new(1);
static LIVE_CONTEXTS: AtomicUsize = AtomicUsize::new(0);
static DROPS: AtomicU64 = AtomicU64::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

fn lock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Local {
    shared: Arc<Shared>,
    ctx: u32,
    span_seq: Cell<u64>,
    current_span: Cell<u64>,
}

impl Drop for Local {
    fn drop(&mut self) {
        // Detach the fast path first: a log call from a later TLS destructor
        // takes the slow path and is dropped instead of touching a released ring.
        let _ = FAST.try_with(|f| f.set(core::ptr::null()));
        self.shared.ring.close();
        if !DRAIN_SEEN.load(Ordering::Acquire) {
            // No drain so far: nothing retires rings, so keep only the last
            // `KEEP_WITHOUT_DRAIN` closed ones (a crash dump still shows the
            // threads that just finished) and release the oldest. Frames in a
            // released ring are lost, as they would be at exit. A drain that
            // starts in between finds the entries in `REGISTRY` and retires
            // them itself.
            let mut pending = lock(&PENDING);
            let closed = pending.iter().filter(|e| e.shared.ring.is_closed()).count();
            if closed > KEEP_WITHOUT_DRAIN {
                let mut released = Vec::new();
                let mut excess = closed - KEEP_WITHOUT_DRAIN;
                pending.retain(|e| {
                    if excess > 0 && e.shared.ring.is_closed() {
                        excess -= 1;
                        released.push(e.shared.clone());
                        return false;
                    }
                    true
                });
                drop(pending);
                let mut rings = lock(&RINGS);
                for r in &released {
                    rings.retain(|s| !Arc::ptr_eq(s, r));
                    LIVE_CONTEXTS.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
    }
}

thread_local! {
    static LOCAL: OnceCell<Local> = const { OnceCell::new() };
    /// The calling thread's ring, for the log fast path. Const-initialised and
    /// without a destructor, so reading it is a plain thread-pointer load with
    /// no lazy-registration check.
    pub(crate) static FAST: Cell<*const ThreadRing> = const { Cell::new(core::ptr::null()) };
}

fn new_local() -> Local {
    let ctx = NEXT_CTX.fetch_add(1, Ordering::Relaxed);
    let name = std::thread::current()
        .name()
        .map(String::from)
        .unwrap_or_else(|| std::format!("thread-{ctx}"));
    let shared = Arc::new(Shared {
        ring: Ring::new(),
        name: Mutex::new(name),
        name_gen: AtomicU32::new(1),
    });
    shared.ring.mark_split(ctx);
    LIVE_CONTEXTS.fetch_add(1, Ordering::Relaxed);
    lock(&RINGS).push(shared.clone());
    lock(&PENDING).push(Entry {
        shared: shared.clone(),
        announced_gen: 0,
    });
    Local {
        shared,
        ctx,
        span_seq: Cell::new(0),
        current_span: Cell::new(0),
    }
}

#[inline(always)]
fn push(l: &Local, site: u32, now: u64, payload: &[u8]) -> bool {
    // SAFETY: only this thread ever pushes into its own thread-local ring.
    let ok = unsafe { l.shared.ring.push_raw(site, now, payload) };
    if !ok {
        DROPS.fetch_add(1, Ordering::Relaxed);
    }
    ok
}

/// Emit a frame into the calling thread's ring. Used by the log macros.
#[doc(hidden)]
#[inline(always)]
pub fn __local_emit<F: FnOnce(&mut crate::Writer<'_>)>(site: u32, encode: F) {
    let ring = FAST.with(Cell::get);
    let ring = if ring.is_null() { attach() } else { ring };
    if ring.is_null() {
        return; // thread is shutting down
    }
    // SAFETY: `FAST` points at this thread's own ring, kept alive by `LOCAL`
    // (and by the registry until the drain retires it after the thread exits);
    // only this thread pushes into it.
    if !unsafe { (*ring).emit(site, encode) } {
        DROPS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Slow path of the first log call on a thread: create and register its ring.
#[cold]
#[inline(never)]
fn attach() -> *const ThreadRing {
    LOCAL
        .try_with(|l| {
            let ring: *const ThreadRing = &l.get_or_init(new_local).shared.ring;
            FAST.with(|f| f.set(ring));
            ring
        })
        .unwrap_or(core::ptr::null())
}

/// Name the calling thread's context. A rename after the first log line is
/// announced again, so the host shows the latest name.
pub fn set_context_name(name: &str) {
    let _ = LOCAL.try_with(|l| {
        let l = l.get_or_init(new_local);
        *l.shared.name.lock().unwrap_or_else(|e| e.into_inner()) = String::from(name);
        l.shared.name_gen.fetch_add(1, Ordering::Release);
    });
}

/// Guard for a span on the calling thread; closes the span when dropped.
///
/// Created by [`span!`](crate::span). Not `Send`: a span opens and closes on
/// the same thread, so the host sees properly nested slices per context.
#[must_use = "the span closes when this guard is dropped"]
pub struct SpanGuard {
    id: u64,
    parent: u64,
    _not_send: core::marker::PhantomData<*const ()>,
}

impl SpanGuard {
    const INERT: SpanGuard = SpanGuard {
        id: 0,
        parent: 0,
        _not_send: core::marker::PhantomData,
    };

    /// Span ID (0 when the span was filtered out); high 16 bits are the context.
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        if self.id == 0 {
            return;
        }
        let now = crate::clock::now();
        let (id, parent) = (self.id, self.parent);
        let _ = LOCAL.try_with(|l| {
            if let Some(l) = l.get() {
                push(l, wire::CTRL_SPAN_CLOSE, now, &id.to_le_bytes());
                l.current_span.set(parent);
            }
        });
    }
}

/// Open a span on the calling thread. Used by [`span!`](crate::span).
#[doc(hidden)]
#[inline]
pub fn __span_enter(site: u32) -> SpanGuard {
    if !crate::level::spans_enabled() {
        return SpanGuard::INERT;
    }
    let now = crate::clock::now();
    LOCAL
        .try_with(|l| {
            let l = l.get_or_init(new_local);
            let seq = l.span_seq.get() + 1;
            l.span_seq.set(seq);
            let id = (u64::from(l.ctx) << 32) | (seq & 0xffff_ffff);
            let parent = l.current_span.get();
            let payload = wire::span_open_payload(site, id, parent);
            if push(l, wire::CTRL_SPAN_OPEN, now, &payload) {
                l.current_span.set(id);
                SpanGuard {
                    id,
                    parent,
                    _not_send: core::marker::PhantomData,
                }
            } else {
                SpanGuard::INERT
            }
        })
        .unwrap_or(SpanGuard::INERT)
}

/// Runtime counters (spec section 0, `stats()`). Lock-free to read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Thread contexts whose rings are still alive.
    pub contexts: usize,
    /// Frames handed to a transport.
    pub frames: u64,
    /// Bytes handed to a transport.
    pub bytes: u64,
    /// Frames dropped on thread rings since start.
    pub drops: u64,
}

/// Current counters.
pub fn stats() -> Stats {
    Stats {
        contexts: LIVE_CONTEXTS.load(Ordering::Relaxed),
        frames: FRAMES.load(Ordering::Relaxed),
        bytes: BYTES.load(Ordering::Relaxed),
        drops: DROPS.load(Ordering::Relaxed),
    }
}

/// The drain: the single consumer of every thread ring.
pub struct Drain<T: Transport> {
    transport: T,
    header_sent: bool,
    scratch: std::boxed::Box<[u8; FRAME_MAX]>,
}

impl<T: Transport> Drain<T> {
    /// A drain writing to `transport`. The stream header goes out on the first poll.
    pub fn new(transport: T) -> Self {
        Drain {
            transport,
            header_sent: false,
            scratch: std::boxed::Box::new([0; FRAME_MAX]),
        }
    }

    /// Move every pending frame to the transport. Returns frames written.
    pub fn poll(&mut self) -> Result<usize, Full> {
        if !self.header_sent {
            drain::write_header(&mut self.transport)?;
            self.header_sent = true;
        }
        // Holding REGISTRY makes this the only consumer for the pass; producers
        // never take it, so the transport may log while we hold it.
        let mut reg = lock(&REGISTRY);
        DRAIN_SEEN.store(true, Ordering::Release);
        {
            let mut pending = lock(&PENDING);
            reg.append(&mut pending);
        }
        let mut frames = 0;
        let mut full = false;
        let t = &mut self.transport;
        let scratch = &mut self.scratch;
        reg.retain_mut(|e| {
            if full {
                return true;
            }
            let ring = &e.shared.ring;
            let ctx = ring.ctx();
            let gen = e.shared.name_gen.load(Ordering::Acquire);
            if e.announced_gen != gen {
                let name = e
                    .shared
                    .name
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                if drain::write_ctx_register(t, ctx, &name).is_err() {
                    full = true;
                    return true;
                }
                e.announced_gen = gen;
            }
            let drops = ring.take_drops();
            if drops > 0 && drain::write_dropped(t, ctx, drops).is_err() {
                ring.restore_drops(drops);
                full = true;
                return true;
            }
            // SAFETY: REGISTRY is held, so this is the ring's only consumer.
            while let Some(n) = unsafe { ring.peek_raw(scratch) } {
                if t.write(&scratch[..n]).is_err() {
                    full = true;
                    return true;
                }
                unsafe { ring.commit_raw(n) };
                frames += 1;
                FRAMES.fetch_add(1, Ordering::Relaxed);
                BYTES.fetch_add(n as u64, Ordering::Relaxed);
            }
            if ring.is_closed() && ring.pending() == 0 && ring.drops() == 0 {
                if drain::write_ctx_close(t, ctx).is_err() {
                    full = true;
                    return true;
                }
                LIVE_CONTEXTS.fetch_sub(1, Ordering::Relaxed);
                lock(&RINGS).retain(|s| !Arc::ptr_eq(s, &e.shared));
                return false;
            }
            true
        });
        drop(reg);
        self.transport.flush();
        if full {
            Err(Full)
        } else {
            Ok(frames)
        }
    }

    /// The transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// The transport, mutably.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Stop draining and return the transport.
    pub fn into_inner(self) -> T {
        self.transport
    }
}

/// Handle to a background drain thread from [`spawn_drain`].
pub struct DrainHandle<T: Transport + Send + 'static> {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Drain<T>>>,
}

impl<T: Transport + Send + 'static> DrainHandle<T> {
    /// Drain everything left, stop the thread and return the transport.
    pub fn stop(mut self) -> T {
        self.stop.store(true, Ordering::Release);
        let t = self.thread.take().expect("drain thread");
        t.thread().unpark();
        let mut d = t.join().expect("drain thread panicked");
        let _ = d.poll();
        d.into_inner()
    }
}

/// Start a low-priority drain thread polling every `period` (opt-in; spec section 0).
pub fn spawn_drain<T: Transport + Send + 'static>(
    transport: T,
    period: Duration,
) -> DrainHandle<T> {
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let thread = std::thread::Builder::new()
        .name(String::from("rtlog-drain"))
        .spawn(move || {
            let mut d = Drain::new(transport);
            while !s.load(Ordering::Acquire) {
                let _ = d.poll();
                std::thread::park_timeout(period);
            }
            d
        })
        .expect("spawn rtlog drain");
    DrainHandle {
        stop,
        thread: Some(thread),
    }
}
