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

type ThreadRing = Ring<THREAD_RING>;

/// State shared between a thread and the drain.
struct Shared {
    ring: ThreadRing,
    name: Mutex<String>,
    /// Bumped on rename so the drain re-announces the context.
    name_gen: AtomicU32,
}

struct Entry {
    shared: Arc<Shared>,
    announced_gen: u32,
}

static PENDING: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
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
        self.shared.ring.close();
    }
}

thread_local! {
    static LOCAL: OnceCell<Local> = const { OnceCell::new() };
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

/// Push a frame into the calling thread's ring. Used by the log macros.
#[doc(hidden)]
#[inline]
pub fn __local_push(site: u32, args: &[u8]) {
    let now = crate::clock::now();
    let _ = LOCAL.try_with(|l| push(l.get_or_init(new_local), site, now, args));
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
    if !crate::enabled(crate::Level::Info) {
        return SpanGuard::INERT;
    }
    let now = crate::clock::now();
    LOCAL
        .try_with(|l| {
            let l = l.get_or_init(new_local);
            let seq = l.span_seq.get() + 1;
            l.span_seq.set(seq);
            let id = (u64::from(l.ctx & 0xffff) << 48) | (seq & 0xffff_ffff_ffff);
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
