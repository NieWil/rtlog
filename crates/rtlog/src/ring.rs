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

use crate::arg::Writer;
use crate::varint;

/// Write `v` as a varint at `p`; returns the position after it. One or two
/// bytes (deltas under 16 384 ticks) take a branch-light path.
///
/// SAFETY: `p` must be valid for `varint::MAX_U64` bytes of writes.
#[inline(always)]
unsafe fn put_varint(p: *mut u8, v: u64) -> *mut u8 {
    if v < 0x80 {
        *p = v as u8;
        p.add(1)
    } else if v < 0x4000 {
        ptr::write_unaligned(p.cast::<[u8; 2]>(), [(v as u8) | 0x80, (v >> 7) as u8]);
        p.add(2)
    } else {
        let n = varint::encode(v, core::slice::from_raw_parts_mut(p, varint::MAX_U64));
        p.add(n)
    }
}

/// Maximum argument bytes in one frame.
pub const ARGS_CAP: usize = 240;
/// Maximum wire frame: site ID + timestamp delta + context + arguments.
pub const FRAME_MAX: usize = 4 + varint::MAX_U64 + 5 + ARGS_CAP;
/// Ring-internal length prefix.
const LEN_PREFIX: usize = 2;
/// Smallest real frame: a `u32` site ID and at least one byte each of
/// timestamp delta and context.
const MIN_FRAME: usize = 6;

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
    /// The context ID, varint-encoded once at split time: bytes, then length.
    ctx_enc: UnsafeCell<[u8; 8]>,
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
            // All-zero so a `static` ring stays in .bss; set at split time.
            ctx_enc: UnsafeCell::new([0; 8]),
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
        self.set_ctx(ctx);
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

    /// Store the context ID and its varint encoding. Called once, before the
    /// producer exists, so the producer reads both without synchronisation.
    fn set_ctx(&self, ctx: u32) {
        self.ctx.store(ctx, Ordering::Relaxed);
        let mut enc = [0u8; 8];
        let n = varint::encode(u64::from(ctx), &mut enc[..7]);
        enc[7] = n as u8;
        // SAFETY: no producer exists yet (split / mark_split run before it).
        unsafe { *self.ctx_enc.get() = enc };
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
        self.set_ctx(ctx);
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

    /// Append one frame whose arguments `encode` writes straight into the ring.
    ///
    /// Fast path: when a worst-case frame fits contiguously before the wrap
    /// point, the header and arguments are written in place and published with
    /// one release store: no intermediate buffer, no `memcpy` call for scalar
    /// arguments. Otherwise the arguments go through a stack buffer and
    /// [`push_raw`](Self::push_raw), which handles the wrap. Returns `false`
    /// and counts a drop when the frame does not fit.
    ///
    /// Only the argument encoding is generic, so only it is repeated at each
    /// call site; `begin` and `finish` are shared (and out of line on bare
    /// metal, where code size matters more than a call).
    ///
    /// SAFETY: the caller must be the ring's only producer.
    #[inline(always)]
    pub(crate) unsafe fn emit<F: FnOnce(&mut Writer<'_>)>(&self, site: u32, encode: F) -> bool {
        #[cfg(not(target_os = "none"))]
        {
            let mut buf = crate::__private::args_buf();
            let (mut w, slot, ts) = self.begin(site, &mut buf);
            encode(&mut w);
            self.finish(site, ts, &w, slot)
        }
        // Bare metal: code size matters more than one small copy. Encode into
        // a stack buffer here, then one shared, non-generic call stamps and
        // pushes it, so each call site holds only its argument writes.
        #[cfg(target_os = "none")]
        {
            let mut buf = crate::__private::args_buf();
            let mut w = Writer::new_uninit(&mut buf);
            encode(&mut w);
            self.push_encoded(site, &w)
        }
    }

    #[cfg(target_os = "none")]
    #[inline(never)]
    unsafe fn push_encoded(&self, site: u32, w: &Writer<'_>) -> bool {
        if w.overflowed() {
            self.drops.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        self.push_raw(site, crate::clock::now(), w.as_slice())
    }

    /// [`emit`](Self::emit) with an explicit timestamp (tests).
    #[cfg(test)]
    unsafe fn emit_at<F: FnOnce(&mut Writer<'_>)>(&self, site: u32, ts: u64, encode: F) -> bool {
        let mut buf = crate::__private::args_buf();
        let (mut w, slot, _) = self.begin(site, &mut buf);
        // Re-stamp: rewrite the header's delta for the explicit time.
        if slot != usize::MAX {
            let mut p = self.base().add(slot + LEN_PREFIX + 4);
            p = put_varint(p, ts.saturating_sub(*self.last_ts.get()));
            let enc = *self.ctx_enc.get();
            ptr::write_unaligned(p.cast::<[u8; 8]>(), enc);
            w = Writer::from_raw(p.add(usize::from(enc[7])), ARGS_CAP);
        }
        encode(&mut w);
        self.finish(site, ts, &w, slot)
    }

    /// Read the clock, reserve room for a frame and write its header; returns
    /// the argument writer, the slot start (`usize::MAX` when the stack buffer
    /// is used) and the timestamp.
    #[cfg_attr(target_os = "none", allow(dead_code))]
    #[inline(always)]
    unsafe fn begin<'b>(
        &self,
        site: u32,
        buf: &'b mut [core::mem::MaybeUninit<u8>; ARGS_CAP],
    ) -> (Writer<'b>, usize, u64) {
        let ts = crate::clock::now();
        const RESERVE: usize = LEN_PREFIX + 4 + varint::MAX_U64 + 5 + ARGS_CAP;
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let idx = head & (N - 1);
        if N - head.wrapping_sub(tail) >= RESERVE && idx + RESERVE <= N {
            let base = self.base().add(idx);
            let mut p = base.add(LEN_PREFIX);
            ptr::write_unaligned(p.cast::<[u8; 4]>(), site.to_le_bytes());
            p = p.add(4);
            p = put_varint(p, ts.saturating_sub(*self.last_ts.get()));
            let enc = *self.ctx_enc.get();
            ptr::write_unaligned(p.cast::<[u8; 8]>(), enc); // only `enc[7]` bytes count
            p = p.add(usize::from(enc[7]));
            (Writer::from_raw(p, ARGS_CAP), idx, ts)
        } else {
            (Writer::new_uninit(buf), usize::MAX, ts)
        }
    }

    /// Publish the frame started by `begin`, or push it through the slow path.
    #[cfg_attr(target_os = "none", allow(dead_code))]
    #[inline(always)]
    unsafe fn finish(&self, site: u32, ts: u64, w: &Writer<'_>, slot: usize) -> bool {
        if w.overflowed() {
            self.drops.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        if slot == usize::MAX {
            return self.push_slow(site, ts, w.as_slice());
        }
        let base = self.base().add(slot);
        let len = w
            .as_slice()
            .as_ptr()
            .add(w.len())
            .offset_from(base.add(LEN_PREFIX)) as usize;
        ptr::write_unaligned(base.cast::<[u8; 2]>(), (len as u16).to_le_bytes());
        let last = *self.last_ts.get();
        *self.last_ts.get() = ts.max(last);
        let head = self.head.load(Ordering::Relaxed);
        self.head
            .store(head.wrapping_add(LEN_PREFIX + len), Ordering::Release);
        true
    }

    #[cfg_attr(target_os = "none", allow(dead_code))]
    #[cold]
    #[inline(never)]
    unsafe fn push_slow(&self, site: u32, ts: u64, args: &[u8]) -> bool {
        self.push_raw(site, ts, args)
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

    /// Visit every unread frame without consuming any, oldest first, until `f`
    /// returns `false`. Read-only: safe next to a running consumer only if that
    /// consumer cannot move `tail` meanwhile (the crash dump holds the registry
    /// lock the `std` drain tries to take). Allocation-free, for use in a
    /// signal handler.
    ///
    /// SAFETY: no other thread may consume this ring during the call.
    #[cfg_attr(not(all(feature = "std", unix)), allow(dead_code))]
    pub(crate) unsafe fn scan(
        &self,
        scratch: &mut [u8; FRAME_MAX],
        mut f: impl FnMut(&[u8]) -> bool,
    ) {
        // `tail` first, then `head`: both only grow and tail <= head always, so
        // the snapshot [tail, head) is never inverted. (Loading head first lets
        // a running consumer move tail past it, and the loop below would then
        // walk bogus lengths for ages.)
        let mut pos = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);
        while pos != head {
            let mut len = [0u8; LEN_PREFIX];
            self.read_at(pos, &mut len);
            let len = usize::from(u16::from_le_bytes(len));
            // Shorter than the smallest real frame (site ID + two varints) or
            // longer than the largest: torn or not a frame start. Stop.
            if !(MIN_FRAME..=FRAME_MAX).contains(&len) || LEN_PREFIX + len > head.wrapping_sub(pos)
            {
                return;
            }
            self.read_at(pos.wrapping_add(LEN_PREFIX), &mut scratch[..len]);
            // A consumer that moved past this frame meanwhile may have let the
            // producer overwrite it: stop rather than hand out garbage. The
            // fence orders the plain reads above before this reload.
            core::sync::atomic::fence(Ordering::Acquire);
            if (self.tail.load(Ordering::Acquire).wrapping_sub(pos) as isize) > 0 {
                return;
            }
            if !f(&scratch[..len]) {
                return;
            }
            pos = pos.wrapping_add(LEN_PREFIX + len);
        }
    }

    /// Timestamp of the newest frame, read without synchronisation. Used only
    /// to align times in a crash dump, where a stale value is harmless.
    #[cfg_attr(not(all(feature = "std", unix)), allow(dead_code))]
    pub(crate) fn last_ts_racy(&self) -> u64 {
        // SAFETY: a plain aligned u64 read; a torn or stale value only shifts
        // the times shown in a crash dump.
        unsafe { ptr::read_volatile(self.last_ts.get()) }
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

    /// Append a frame whose arguments `encode` writes in place (used by the log
    /// macros; see `Ring::emit`).
    #[inline(always)]
    pub fn emit<F: FnOnce(&mut Writer<'_>)>(&mut self, site: u32, encode: F) -> bool {
        // SAFETY: `Producer` is unique per ring and not `Sync`.
        unsafe { self.ring.emit(site, encode) }
    }

    /// Append a frame with an explicit timestamp (ticks of the configured clock).
    #[inline]
    pub fn push_at(&mut self, site: u32, ts: u64, args: &[u8]) -> bool {
        // SAFETY: as above.
        unsafe { self.ring.push_raw(site, ts, args) }
    }

    /// Open a span named by span site `site`, nested in the current one.
    /// Returns an inert span (`id == 0`) when spans are off ([`crate::set_spans`]).
    pub fn span_enter(&mut self, site: u32) -> SpanId {
        let parent = self.current_span;
        if !crate::level::spans_enabled() {
            return SpanId { id: 0, parent };
        }
        self.span_seq += 1;
        let id = (u64::from(self.ring.ctx()) << 32) | (self.span_seq & 0xffff_ffff);
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
    fn emit_writes_in_place_and_falls_back_at_the_wrap() {
        // 512 B ring: the in-place path needs 261 contiguous bytes, so as the
        // head moves around the ring both paths run, many times.
        let ring = Ring::<512>::new();
        let (mut p, mut c) = ring.split(300).unwrap(); // two-byte ctx varint
        let mut out = [0u8; FRAME_MAX];
        let rounds = if cfg!(miri) { 300u32 } else { 20_000 };
        for i in 0..rounds {
            let n = (i % 37) as usize;
            let payload = [i as u8; 40];
            // A delta of i*50 ticks exercises 1-, 2- and 3-byte varints.
            let ts = u64::from(i) * u64::from(i) * 50;
            assert!(unsafe {
                ring.emit_at(i, ts, |w| {
                    w.put_array(i.to_le_bytes());
                    w.put(&payload[..n]);
                })
            });
            let len = c.pop(&mut out).unwrap();
            let (site, delta, ctx, args) = decode_meta(&out[..len]);
            assert_eq!(site, i);
            let prev = u64::from(i.saturating_sub(1)) * u64::from(i.saturating_sub(1)) * 50;
            assert_eq!(delta, if i == 0 { 0 } else { ts - prev });
            assert_eq!(ctx, 300);
            assert_eq!(&args[..4], &i.to_le_bytes());
            assert_eq!(&args[4..], &payload[..n]);
        }
        let _ = &mut p;
    }

    #[test]
    fn scan_stops_at_a_zero_length_frame_instead_of_walking_it() {
        // A zeroed region that `head` claims is committed (what a stale `head`
        // or a torn write looks like): len = 0 is not a frame.
        let ring = Ring::<64>::new();
        ring.head.store(40, Ordering::Release);
        let mut scratch = [0u8; FRAME_MAX];
        let mut seen = 0;
        // SAFETY: single-threaded test.
        unsafe {
            ring.scan(&mut scratch, |_| {
                seen += 1;
                true
            })
        };
        assert_eq!(seen, 0);
    }

    #[test]
    fn default_ring_is_empty_and_reports_its_capacity() {
        let ring = Ring::<128>::default();
        assert_eq!(ring.capacity(), 128);
        assert_eq!(
            (ring.pending(), ring.drops(), ring.is_closed()),
            (0, 0, false)
        );
        assert!(ring.split(1).is_some());
        assert!(ring.split(1).is_none(), "only one split");
    }

    #[test]
    fn producer_and_consumer_share_the_ring() {
        let ring = Ring::<128>::new();
        let (mut p, c) = ring.split(5).unwrap();
        assert!(core::ptr::eq(p.ring(), &ring));
        assert!(core::ptr::eq(c.ring(), &ring));
        p.count_drop();
        p.count_drop();
        assert_eq!(ring.drops(), 2);
        assert_eq!(ring.take_drops(), 2);
        assert_eq!(ring.drops(), 0);
    }

    #[test]
    fn a_span_whose_open_frame_is_dropped_is_inert() {
        let ring = Ring::<64>::new();
        let (mut p, mut c) = ring.split(2).unwrap();
        let outer = p.span_enter(9);
        assert_ne!(outer.id, 0);
        // Fill the ring; the next open frame cannot fit.
        while p.push_at(1, 0, &[0; 8]) {}
        let drops = ring.drops();
        let inner = p.span_enter(9);
        assert_eq!(
            inner,
            SpanId {
                id: 0,
                parent: outer.id
            }
        );
        assert_eq!(ring.drops(), drops + 1);
        // Closing the inert span writes nothing and keeps `outer` current.
        let mut out = [0u8; FRAME_MAX];
        while c.pop(&mut out).is_some() {}
        p.span_exit(inner);
        assert!(c.pop(&mut out).is_none());
        let next = p.span_enter(9);
        assert_eq!(next.parent, outer.id);
    }

    fn frames_seen_by_scan<const N: usize>(ring: &Ring<N>, max: usize) -> usize {
        let mut scratch = [0u8; FRAME_MAX];
        let mut seen = 0;
        // SAFETY: single-threaded test; nothing consumes during the scan.
        unsafe {
            ring.scan(&mut scratch, |_| {
                seen += 1;
                seen < max
            })
        };
        seen
    }

    #[test]
    fn scan_reads_without_consuming_and_stops_when_asked() {
        let ring = Ring::<256>::new();
        let (mut p, _c) = ring.split(1).unwrap();
        for i in 0..5 {
            assert!(p.push_at(i, u64::from(i), &[i as u8]));
        }
        let pending = ring.pending();
        assert_eq!(frames_seen_by_scan(&ring, usize::MAX), 5);
        assert_eq!(
            frames_seen_by_scan(&ring, 2),
            2,
            "stops when `f` returns false"
        );
        assert_eq!(ring.pending(), pending, "scan consumes nothing");
    }

    #[test]
    fn scan_stops_when_the_consumer_passes_the_frame_it_read() {
        let ring = Ring::<256>::new();
        let (mut p, mut c) = ring.split(1).unwrap();
        for i in 0..3 {
            assert!(p.push_at(i, 0, &[i as u8; 4]));
        }
        let mut scratch = [0u8; FRAME_MAX];
        let mut seen = [0u8; 4];
        let mut n_seen = 0;
        // SAFETY: single-threaded test. The callback plays a drain that
        // consumed the first two frames while the dump read the first.
        unsafe {
            ring.scan(&mut scratch, |f| {
                seen[n_seen] = f[0];
                n_seen += 1;
                if n_seen == 1 {
                    let mut out = [0u8; FRAME_MAX];
                    let a = ring.peek_raw(&mut out).unwrap();
                    ring.commit_raw(a);
                    let b = ring.peek_raw(&mut out).unwrap();
                    ring.commit_raw(b);
                }
                true
            })
        };
        // Frame 0 was read before the consumer moved; frame 1's start is then
        // behind `tail`, so the scan stops instead of handing it out.
        assert_eq!(&seen[..n_seen], &[0]);
        let mut out = [0u8; FRAME_MAX];
        let n = c.pop(&mut out).unwrap();
        assert_eq!(out[..n][0], 2);
    }

    #[test]
    fn emit_overflow_counts_a_drop() {
        let ring = Ring::<1024>::new();
        let (_p, _c) = ring.split(1).unwrap();
        let big = [0u8; ARGS_CAP + 1];
        assert!(!unsafe { ring.emit_at(1, 0, |w| w.put(&big)) });
        assert_eq!(ring.drops(), 1);
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
