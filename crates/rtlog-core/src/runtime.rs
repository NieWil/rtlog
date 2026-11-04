//! `std` runtime: one heap ring per thread, created on the thread's first log
//! call, and a drain that serves all of them (spec sections 7 and 8).

use std::cell::OnceCell;
use std::string::String;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;
use std::vec::Vec;

use crate::drain::{self, Full, Transport};
use crate::ring::{Ring, FRAME_MAX};

/// Ring size per thread on `std`.
pub const THREAD_RING: usize = 64 * 1024;

type ThreadRing = Ring<THREAD_RING>;

struct Entry {
    ring: Arc<ThreadRing>,
    name: String,
    announced: bool,
}

static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static NEXT_CTX: AtomicU32 = AtomicU32::new(1);
static REPORTED_DROPS: AtomicU64 = AtomicU64::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

fn registry() -> MutexGuard<'static, Vec<Entry>> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}

struct Local(Arc<ThreadRing>);

impl Drop for Local {
    fn drop(&mut self) {
        self.0.close();
    }
}

thread_local! {
    static LOCAL: OnceCell<Local> = const { OnceCell::new() };
}

fn new_local() -> Local {
    let ctx = NEXT_CTX.fetch_add(1, Ordering::Relaxed);
    let ring: Arc<ThreadRing> = Arc::new(Ring::new());
    ring.mark_split(ctx);
    let name = std::thread::current()
        .name()
        .map(String::from)
        .unwrap_or_else(|| std::format!("thread-{ctx}"));
    registry().push(Entry {
        ring: ring.clone(),
        name,
        announced: false,
    });
    Local(ring)
}

/// Push a frame into the calling thread's ring. Used by the log macros.
#[doc(hidden)]
#[inline]
pub fn __local_push(site: u32, args: &[u8]) {
    let now = crate::clock::now();
    let _ = LOCAL.try_with(|l| {
        let ring = &l.get_or_init(new_local).0;
        // SAFETY: only this thread ever pushes into its own thread-local ring.
        unsafe { ring.push_raw(site, now, args) };
    });
}

/// Name the calling thread's context; call before its first log line.
pub fn set_context_name(name: &str) {
    let _ = LOCAL.try_with(|l| {
        let ring = &l.get_or_init(new_local).0;
        if let Some(e) = registry().iter_mut().find(|e| Arc::ptr_eq(&e.ring, ring)) {
            e.name = String::from(name);
        }
    });
}

/// Runtime counters (spec section 0, `stats()`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Live contexts.
    pub contexts: usize,
    /// Frames handed to a transport.
    pub frames: u64,
    /// Bytes handed to a transport.
    pub bytes: u64,
    /// Frames dropped, reported or still pending.
    pub drops: u64,
}

/// Current counters.
pub fn stats() -> Stats {
    let reg = registry();
    let pending: u64 = reg.iter().map(|e| u64::from(e.ring.drops())).sum();
    Stats {
        contexts: reg.len(),
        frames: FRAMES.load(Ordering::Relaxed),
        bytes: BYTES.load(Ordering::Relaxed),
        drops: REPORTED_DROPS.load(Ordering::Relaxed) + pending,
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
        // Holding the registry lock makes this the only consumer for the pass.
        let mut reg = registry();
        let mut frames = 0;
        let mut full = false;
        reg.retain_mut(|e| {
            if full {
                return true;
            }
            let ctx = e.ring.ctx();
            if !e.announced {
                if drain::write_ctx_register(&mut self.transport, ctx, &e.name).is_err() {
                    full = true;
                    return true;
                }
                e.announced = true;
            }
            let drops = e.ring.take_drops();
            if drops > 0 {
                if drain::write_dropped(&mut self.transport, ctx, drops).is_err() {
                    e.ring.restore_drops(drops);
                    full = true;
                    return true;
                }
                REPORTED_DROPS.fetch_add(u64::from(drops), Ordering::Relaxed);
            }
            // SAFETY: registry lock held, so this is the ring's only consumer.
            while let Some(n) = unsafe { e.ring.peek_raw(&mut self.scratch) } {
                if self.transport.write(&self.scratch[..n]).is_err() {
                    full = true;
                    return true;
                }
                unsafe { e.ring.commit_raw(n) };
                frames += 1;
                FRAMES.fetch_add(1, Ordering::Relaxed);
                BYTES.fetch_add(n as u64, Ordering::Relaxed);
            }
            if e.ring.is_closed() && e.ring.pending() == 0 && e.ring.drops() == 0 {
                return drain::write_ctx_close(&mut self.transport, ctx).is_err();
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
    stop: Arc<std::sync::atomic::AtomicBool>,
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
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
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
