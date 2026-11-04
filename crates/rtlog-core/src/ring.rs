//! Single-producer single-consumer byte ring (spec section 7).
//!
//! Each context owns one ring. The producer is the only writer of `head`, the
//! consumer the only writer of `tail`; there is no CAS and no spin. Inside the
//! ring every frame is prefixed by a `u16` length so the consumer can hand
//! whole frames to the transport; that prefix never goes on the wire.

use core::cell::{Cell, UnsafeCell};
use core::marker::PhantomData;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use crate::varint;

/// Maximum argument bytes in one frame.
pub const ARGS_CAP: usize = 240;
/// Maximum wire frame: site ID + timestamp delta + context + arguments.
pub const FRAME_MAX: usize = 4 + varint::MAX_U64 + 5 + ARGS_CAP;
/// Ring-internal length prefix.
const LEN_PREFIX: usize = 2;

/// A fixed-size SPSC ring of `N` bytes. `N` must be a power of two, ≥ 64.
///
/// Typical use on `no_std`: a `static` ring per task, split once into a
/// [`Producer`] for the task and a [`Consumer`] for the drain.
pub struct Ring<const N: usize> {
    buf: UnsafeCell<[u8; N]>,
    head: AtomicUsize,
    tail: AtomicUsize,
    drops: AtomicU32,
    ctx: AtomicU32,
    split: AtomicBool,
    closed: AtomicBool,
    /// Producer-only: timestamp of the last committed frame.
    last_ts: UnsafeCell<u64>,
}

// SAFETY: the producer side (`head`, `last_ts`, writes to free space) and the
// consumer side (`tail`, reads of committed space) are each reachable from one
// thread only: `split` hands out at most one `Producer` and one `Consumer`, and
// the crate's internal users uphold the same rule. The buffer regions they touch
// are disjoint and handed over with Release/Acquire on `head` and `tail`.
unsafe impl<const N: usize> Sync for Ring<N> {}

impl<const N: usize> Default for Ring<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Ring<N> {
    const CHECK: () = assert!(
        N.is_power_of_two() && N >= 64,
        "ring size must be a power of two >= 64"
    );

    /// An empty ring; `const`, so it can be a `static`.
    pub const fn new() -> Self {
        #[allow(clippy::let_unit_value)]
        let () = Self::CHECK;
        Ring {
            buf: UnsafeCell::new([0; N]),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            drops: AtomicU32::new(0),
            ctx: AtomicU32::new(0),
            split: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            last_ts: UnsafeCell::new(0),
        }
    }

    /// Capacity in bytes.
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Split into the single producer and single consumer, tagging frames with
    /// context `ctx` (0 is reserved for ISRs). Returns `None` after the first call.
    pub fn split(&self, ctx: u32) -> Option<(Producer<'_, N>, Consumer<'_, N>)> {
        if self.split.swap(true, Ordering::AcqRel) {
            return None;
        }
        self.ctx.store(ctx, Ordering::Relaxed);
        Some((
            Producer {
                ring: self,
                span_seq: 0,
                current_span: 0,
                _not_sync: PhantomData,
            },
            Consumer {
                ring: self,
                _not_sync: PhantomData,
            },
        ))
    }

    /// Context ID this ring was split with.
    pub fn ctx(&self) -> u32 {
        self.ctx.load(Ordering::Relaxed)
    }

    /// Bytes currently committed and not yet consumed.
    pub fn pending(&self) -> usize {
        self.head
            .load(Ordering::Acquire)
            .wrapping_sub(self.tail.load(Ordering::Acquire))
    }

    /// Frames dropped since the last [`take_drops`](Self::take_drops).
    pub fn drops(&self) -> u32 {
        self.drops.load(Ordering::Relaxed)
    }

    /// Read and reset the drop counter.
    pub fn take_drops(&self) -> u32 {
        self.drops.swap(0, Ordering::Relaxed)
    }

    pub(crate) fn restore_drops(&self, n: u32) {
        self.drops.fetch_add(n, Ordering::Relaxed);
    }

    #[cfg_attr(not(feature = "std"), allow(dead_code))]
    pub(crate) fn mark_split(&self, ctx: u32) {
        self.split.store(true, Ordering::Relaxed);
        self.ctx.store(ctx, Ordering::Relaxed);
    }

    /// Mark the ring closed: its context is gone and it will get no new frames.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    /// Whether [`close`](Self::close) was called.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    #[inline(always)]
    fn base(&self) -> *mut u8 {
        self.buf.get().cast::<u8>()
    }

    /// Copy `src` into the ring at logical position `pos`, wrapping.
    ///
    /// SAFETY: `[pos, pos + src.len())` must be free space owned by the producer.
    #[inline(always)]
    unsafe fn write_at(&self, pos: usize, src: &[u8]) {
        let idx = pos & (N - 1);
        let first = src.len().min(N - idx);
        ptr::copy_nonoverlapping(src.as_ptr(), self.base().add(idx), first);
        if first < src.len() {
            ptr::copy_nonoverlapping(src.as_ptr().add(first), self.base(), src.len() - first);
        }
    }

    /// Copy from the ring at logical position `pos` into `dst`, wrapping.
    ///
    /// SAFETY: `[pos, pos + dst.len())` must be committed space owned by the consumer.
    #[inline(always)]
    unsafe fn read_at(&self, pos: usize, dst: &mut [u8]) {
        let idx = pos & (N - 1);
        let first = dst.len().min(N - idx);
        ptr::copy_nonoverlapping(self.base().add(idx), dst.as_mut_ptr(), first);
        if first < dst.len() {
            ptr::copy_nonoverlapping(self.base(), dst.as_mut_ptr().add(first), dst.len() - first);
        }
    }

    /// Append one frame. Returns `false` and counts a drop when it does not fit.
    ///
    /// SAFETY: the caller must be the ring's only producer.
    #[inline]
    pub(crate) unsafe fn push_raw(&self, site: u32, ts: u64, args: &[u8]) -> bool {
        let last = *self.last_ts.get();
        let delta = ts.saturating_sub(last);

        let mut meta = [0u8; 4 + varint::MAX_U64 + 5];
        meta[..4].copy_from_slice(&site.to_le_bytes());
        let mut m = 4;
        m += varint::encode(delta, &mut meta[m..]);
        m += varint::encode(u64::from(self.ctx.load(Ordering::Relaxed)), &mut meta[m..]);

        let len = m + args.len();
        let need = LEN_PREFIX + len;
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if args.len() > ARGS_CAP || N - head.wrapping_sub(tail) < need {
            self.drops.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        self.write_at(head, &(len as u16).to_le_bytes());
        self.write_at(head.wrapping_add(LEN_PREFIX), &meta[..m]);
        self.write_at(head.wrapping_add(LEN_PREFIX + m), args);
        *self.last_ts.get() = ts.max(last);
        self.head.store(head.wrapping_add(need), Ordering::Release);
        true
    }

    /// Copy the oldest frame into `out` without consuming it; returns its length.
    ///
    /// SAFETY: the caller must be the ring's only consumer.
    pub(crate) unsafe fn peek_raw(&self, out: &mut [u8; FRAME_MAX]) -> Option<usize> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let mut len = [0u8; LEN_PREFIX];
        self.read_at(tail, &mut len);
        let len = u16::from_le_bytes(len) as usize;
        debug_assert!(len <= FRAME_MAX && LEN_PREFIX + len <= head.wrapping_sub(tail));
        self.read_at(tail.wrapping_add(LEN_PREFIX), &mut out[..len]);
        Some(len)
    }

    /// Release the frame returned by the last [`peek_raw`](Self::peek_raw).
    ///
    /// SAFETY: the caller must be the ring's only consumer, and `len` must come
    /// from the immediately preceding `peek_raw`.
    pub(crate) unsafe fn commit_raw(&self, len: usize) {
        let tail = self.tail.load(Ordering::Relaxed);
        self.tail
            .store(tail.wrapping_add(LEN_PREFIX + len), Ordering::Release);
    }
}

/// The write side of a ring; one per ring, owned by its task or thread.
///
/// `Send` but not `Sync`: it can move to the task that uses it, never be shared.
pub struct Producer<'a, const N: usize> {
    ring: &'a Ring<N>,
    span_seq: u64,
    current_span: u64,
    _not_sync: PhantomData<Cell<()>>,
}

/// An open span on a [`Producer`]; pass it back to [`Producer::span_exit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "close the span with span_exit"]
pub struct SpanId {
    /// Span ID; 0 if the open frame was dropped.
    pub id: u64,
    /// The span that was current when this one opened.
    pub parent: u64,
}

impl<'a, const N: usize> Producer<'a, N> {
    /// Append a frame for `site` with already encoded `args`, timestamped now.
    #[inline]
    pub fn push(&mut self, site: u32, args: &[u8]) -> bool {
        // SAFETY: `Producer` is unique per ring and not `Sync`.
        unsafe { self.ring.push_raw(site, crate::clock::now(), args) }
    }

    /// Append a frame with an explicit timestamp (ticks of the configured clock).
    #[inline]
    pub fn push_at(&mut self, site: u32, ts: u64, args: &[u8]) -> bool {
        // SAFETY: as above.
        unsafe { self.ring.push_raw(site, ts, args) }
    }

    /// Open a span named by span site `site`, nested in the current one.
    pub fn span_enter(&mut self, site: u32) -> SpanId {
        self.span_seq += 1;
        let id = (u64::from(self.ring.ctx() & 0xffff) << 48) | (self.span_seq & 0xffff_ffff_ffff);
        let parent = self.current_span;
        let payload = crate::wire::span_open_payload(site, id, parent);
        if self.push(crate::wire::CTRL_SPAN_OPEN, &payload) {
            self.current_span = id;
            SpanId { id, parent }
        } else {
            SpanId { id: 0, parent }
        }
    }

    /// Close a span opened by [`span_enter`](Self::span_enter).
    pub fn span_exit(&mut self, span: SpanId) {
        if span.id != 0 {
            self.push(crate::wire::CTRL_SPAN_CLOSE, &span.id.to_le_bytes());
            self.current_span = span.parent;
        }
    }

    /// Count a frame as dropped without writing it.
    #[inline]
    pub fn count_drop(&mut self) {
        self.ring.restore_drops(1);
    }

    /// The ring this producer writes.
    pub fn ring(&self) -> &'a Ring<N> {
        self.ring
    }
}

/// The read side of a ring; one per ring, owned by the drain.
pub struct Consumer<'a, const N: usize> {
    ring: &'a Ring<N>,
    _not_sync: PhantomData<Cell<()>>,
}

impl<'a, const N: usize> Consumer<'a, N> {
    /// Copy the oldest frame into `out` without consuming it.
    pub fn peek(&mut self, out: &mut [u8; FRAME_MAX]) -> Option<usize> {
        // SAFETY: `Consumer` is unique per ring and not `Sync`.
        unsafe { self.ring.peek_raw(out) }
    }

    /// Release the frame returned by the last [`peek`](Self::peek).
    pub fn commit(&mut self, len: usize) {
        // SAFETY: as above; `len` comes from `peek` by contract of this API.
        unsafe { self.ring.commit_raw(len) }
    }

    /// Pop the oldest frame into `out`.
    pub fn pop(&mut self, out: &mut [u8; FRAME_MAX]) -> Option<usize> {
        let n = self.peek(out)?;
        self.commit(n);
        Some(n)
    }

    /// The ring this consumer reads.
    pub fn ring(&self) -> &'a Ring<N> {
        self.ring
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_meta(f: &[u8]) -> (u32, u64, u64, &[u8]) {
        let site = u32::from_le_bytes([f[0], f[1], f[2], f[3]]);
        let (ts, a) = varint::decode(&f[4..]).unwrap();
        let (ctx, b) = varint::decode(&f[4 + a..]).unwrap();
        (site, ts, ctx, &f[4 + a + b..])
    }

    #[test]
    fn push_pop_fifo_with_deltas() {
        let ring = Ring::<256>::new();
        let (mut p, mut c) = ring.split(3).unwrap();
        assert!(p.push_at(10, 100, &[1, 2]));
        assert!(p.push_at(11, 150, &[3]));
        let mut out = [0u8; FRAME_MAX];
        let n = c.pop(&mut out).unwrap();
        assert_eq!(decode_meta(&out[..n]), (10, 100, 3, &[1u8, 2][..]));
        let n = c.pop(&mut out).unwrap();
        assert_eq!(decode_meta(&out[..n]), (11, 50, 3, &[3u8][..]));
        assert!(c.pop(&mut out).is_none());
    }

    #[test]
    fn split_only_once() {
        let ring = Ring::<64>::new();
        assert!(ring.split(1).is_some());
        assert!(ring.split(1).is_none());
    }

    #[test]
    fn drop_newest_when_full_and_count() {
        let ring = Ring::<64>::new();
        let (mut p, mut c) = ring.split(1).unwrap();
        let args = [0u8; 20];
        let mut ok = 0;
        while p.push_at(1, 0, &args) {
            ok += 1;
        }
        assert!(ok >= 1);
        assert!(!p.push_at(1, 0, &args));
        assert_eq!(ring.drops(), 2);
        assert_eq!(ring.take_drops(), 2);
        assert_eq!(ring.drops(), 0);
        let mut out = [0u8; FRAME_MAX];
        let mut got = 0;
        while c.pop(&mut out).is_some() {
            got += 1;
        }
        assert_eq!(got, ok);
    }

    #[test]
    fn oversize_args_dropped() {
        let ring = Ring::<1024>::new();
        let (mut p, _c) = ring.split(1).unwrap();
        assert!(!p.push_at(1, 0, &[0u8; ARGS_CAP + 1]));
        assert_eq!(ring.drops(), 1);
    }

    #[test]
    fn wraps_around_many_times() {
        let ring = Ring::<128>::new();
        let (mut p, mut c) = ring.split(2).unwrap();
        let mut out = [0u8; FRAME_MAX];
        for i in 0..if cfg!(miri) { 600u32 } else { 10_000 } {
            let args = i.to_le_bytes();
            assert!(p.push_at(i, u64::from(i), &args[..(i as usize % 4) + 1]));
            let n = c.pop(&mut out).unwrap();
            let (site, delta, ctx, a) = decode_meta(&out[..n]);
            assert_eq!(site, i);
            assert_eq!(delta, if i == 0 { 0 } else { 1 });
            assert_eq!(ctx, 2);
            assert_eq!(a, &args[..(i as usize % 4) + 1]);
        }
        assert_eq!(ring.pending(), 0);
    }

    #[test]
    fn non_monotonic_clock_gives_zero_delta() {
        let ring = Ring::<128>::new();
        let (mut p, mut c) = ring.split(1).unwrap();
        p.push_at(1, 100, &[]);
        p.push_at(1, 90, &[]);
        p.push_at(1, 120, &[]);
        let mut out = [0u8; FRAME_MAX];
        let deltas: [u64; 3] = core::array::from_fn(|_| {
            let n = c.pop(&mut out).unwrap();
            decode_meta(&out[..n]).1
        });
        assert_eq!(deltas, [100, 0, 20]);
    }

    #[test]
    fn peek_does_not_consume() {
        let ring = Ring::<128>::new();
        let (mut p, mut c) = ring.split(1).unwrap();
        p.push_at(5, 0, &[9]);
        let mut out = [0u8; FRAME_MAX];
        let n = c.peek(&mut out).unwrap();
        assert_eq!(c.peek(&mut out), Some(n));
        c.commit(n);
        assert!(c.peek(&mut out).is_none());
    }

    #[cfg(feature = "std")]
    #[test]
    fn spsc_threads_preserve_order() {
        use std::sync::Arc;
        let ring: Arc<Ring<1024>> = Arc::new(Ring::new());
        ring.mark_split(1);
        const COUNT: u32 = if cfg!(miri) { 2_000 } else { 200_000 };
        let r = ring.clone();
        let producer = std::thread::spawn(move || {
            let mut sent = 0;
            let mut i = 0u32;
            while i < COUNT {
                // SAFETY: this thread is the only producer.
                if unsafe { r.push_raw(i, u64::from(i), &i.to_le_bytes()) } {
                    sent += 1;
                    i += 1;
                } else {
                    r.take_drops();
                    std::hint::spin_loop();
                }
            }
            sent
        });
        let mut out = [0u8; FRAME_MAX];
        let mut next = 0u32;
        while next < COUNT {
            // SAFETY: this thread is the only consumer.
            if let Some(n) = unsafe { ring.peek_raw(&mut out) } {
                let (site, _, _, a) = decode_meta(&out[..n]);
                assert_eq!(site, next);
                assert_eq!(a, next.to_le_bytes());
                unsafe { ring.commit_raw(n) };
                next += 1;
            } else {
                std::hint::spin_loop();
            }
        }
        assert_eq!(producer.join().unwrap(), COUNT);
    }
}
