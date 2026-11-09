//! Transport trait and drain building blocks (spec section 8).
//!
//! The drain is the only place that may block, batch or compress. These
//! functions are what both the `std` drain and a hand-rolled `no_std` idle-loop
//! drain are made of.

use crate::ring::{Consumer, FRAME_MAX};
use crate::varint;
use crate::wire::{self, Header};

/// Error returned by a transport that cannot take a whole frame right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Full;

/// A byte sink. Each call carries exactly one whole frame (or the stream
/// header): it must take all of it or none of it.
pub trait Transport {
    /// Write one frame, all or nothing.
    fn write(&mut self, frame: &[u8]) -> Result<(), Full>;
    /// Push buffered bytes out; default does nothing.
    fn flush(&mut self) {}
}

impl<T: Transport + ?Sized> Transport for &mut T {
    fn write(&mut self, frame: &[u8]) -> Result<(), Full> {
        (**self).write(frame)
    }
    fn flush(&mut self) {
        (**self).flush()
    }
}

/// A transport over a fixed byte slice; handy on `no_std` and in tests.
pub struct SliceTransport<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> SliceTransport<'a> {
    /// Wrap `buf`.
    pub fn new(buf: &'a mut [u8]) -> Self {
        SliceTransport { buf, len: 0 }
    }
    /// Bytes written so far.
    pub fn written(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Transport for SliceTransport<'_> {
    fn write(&mut self, frame: &[u8]) -> Result<(), Full> {
        let end = self.len + frame.len();
        if end > self.buf.len() {
            return Err(Full);
        }
        self.buf[self.len..end].copy_from_slice(frame);
        self.len = end;
        Ok(())
    }
}

#[cfg(feature = "std")]
impl Transport for std::vec::Vec<u8> {
    fn write(&mut self, frame: &[u8]) -> Result<(), Full> {
        self.extend_from_slice(frame);
        Ok(())
    }
}

/// Any `std::io::Write` as a transport (file, socket, stdout).
#[cfg(feature = "std")]
pub struct IoTransport<W: std::io::Write>(pub W);

#[cfg(feature = "std")]
impl<W: std::io::Write> Transport for IoTransport<W> {
    fn write(&mut self, frame: &[u8]) -> Result<(), Full> {
        self.0.write_all(frame).map_err(|_| Full)
    }
    fn flush(&mut self) {
        let _ = self.0.flush();
    }
}

/// Write the stream header for this binary's contract and clock.
pub fn write_header(t: &mut impl Transport) -> Result<(), Full> {
    t.write(&Header::new(crate::site::contract_hash(), crate::clock::hz()).encode())
}

/// Write a `resync` control frame: the control ID and a copy of the stream
/// header, in one write. Emit one about every [`wire::RESYNC_EVERY`] bytes (the
/// `std` drain does); a decoder that hit a bad frame skips to the next one.
/// Follow it with [`write_ctx_register`] and [`write_ctx_time`] per context so
/// names and clocks are exact after a gap.
pub fn write_resync(t: &mut impl Transport) -> Result<(), Full> {
    let mut b = [0u8; 4 + wire::HEADER_LEN];
    b[..4].copy_from_slice(&wire::CTRL_RESYNC.to_le_bytes());
    b[4..].copy_from_slice(&Header::new(crate::site::contract_hash(), crate::clock::hz()).encode());
    t.write(&b)
}

/// Write a `ctx_time` control frame: context `ctx`'s clock, in ticks.
pub fn write_ctx_time(t: &mut impl Transport, ctx: u32, ticks: u64) -> Result<(), Full> {
    control(t, wire::CTRL_CTX_TIME, ctx, &ticks.to_le_bytes())
}

/// Ticks a forwarded frame advances its context's clock by: the timestamp delta
/// after the 4-byte site ID. Every frame in a ring is timed.
pub fn frame_delta(frame: &[u8]) -> u64 {
    frame
        .get(4..)
        .and_then(varint::decode)
        .map_or(0, |(d, _)| d)
}

fn control(t: &mut impl Transport, id: u32, ctx: u32, tail: &[u8]) -> Result<(), Full> {
    let mut b = [0u8; 4 + 5 + 1 + crate::arg::DYN_CAP];
    b[..4].copy_from_slice(&id.to_le_bytes());
    let mut n = 4 + varint::encode(u64::from(ctx), &mut b[4..]);
    b[n..n + tail.len()].copy_from_slice(tail);
    n += tail.len();
    t.write(&b[..n])
}

/// Write a `ctx_register` control frame naming context `ctx`.
pub fn write_ctx_register(t: &mut impl Transport, ctx: u32, name: &str) -> Result<(), Full> {
    let name = &name.as_bytes()[..name.len().min(crate::arg::DYN_CAP)];
    let mut tail = [0u8; 1 + crate::arg::DYN_CAP];
    tail[0] = name.len() as u8;
    tail[1..=name.len()].copy_from_slice(name);
    control(t, wire::CTRL_CTX_REGISTER, ctx, &tail[..=name.len()])
}

/// Write a `ctx_close` control frame.
pub fn write_ctx_close(t: &mut impl Transport, ctx: u32) -> Result<(), Full> {
    control(t, wire::CTRL_CTX_CLOSE, ctx, &[])
}

/// Write a `dropped` control frame.
pub fn write_dropped(t: &mut impl Transport, ctx: u32, count: u32) -> Result<(), Full> {
    control(t, wire::CTRL_DROPPED, ctx, &count.to_le_bytes())
}

/// Counters from one drain pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PassStats {
    /// Log and control frames moved from the ring to the transport.
    pub frames: usize,
    /// Bytes of those frames; the `dropped` report is not counted.
    pub bytes: usize,
    /// Drop count written in a `dropped` frame this pass (0 if none).
    pub dropped_reported: u32,
}

/// Drain one ring into `t` until it is empty or `t` is full.
///
/// Reports pending drops first. A frame the transport refuses stays in the ring.
pub fn drain_ring<const N: usize>(
    c: &mut Consumer<'_, N>,
    t: &mut impl Transport,
    scratch: &mut [u8; FRAME_MAX],
) -> PassStats {
    let mut s = PassStats::default();
    let ring = c.ring();
    let drops = ring.take_drops();
    if drops > 0 {
        if write_dropped(t, ring.ctx(), drops).is_err() {
            // Put the count back so it is reported on the next pass.
            ring.restore_drops(drops);
            return s;
        }
        s.dropped_reported = drops;
    }
    while let Some(n) = c.peek(scratch) {
        if t.write(&scratch[..n]).is_err() {
            break;
        }
        c.commit(n);
        s.frames += 1;
        s.bytes += n;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::Ring;

    #[test]
    fn slice_transport_is_all_or_nothing() {
        let mut buf = [0u8; 4];
        let mut t = SliceTransport::new(&mut buf);
        assert_eq!(t.write(&[1, 2, 3]), Ok(()));
        assert_eq!(t.write(&[4, 5]), Err(Full));
        assert_eq!(t.written(), &[1, 2, 3]);
    }

    #[test]
    fn drain_reports_drops_then_frames() {
        let ring = Ring::<64>::new();
        let (mut p, mut c) = ring.split(4).unwrap();
        while p.push_at(7, 1, &[0; 10]) {}
        let mut buf = [0u8; 1024];
        let mut t = SliceTransport::new(&mut buf);
        let mut scratch = [0u8; FRAME_MAX];
        let s = drain_ring(&mut c, &mut t, &mut scratch);
        assert_eq!(s.dropped_reported, 1);
        assert!(s.frames >= 1);
        let w = t.written();
        assert_eq!(&w[..4], &wire::CTRL_DROPPED.to_le_bytes());
        assert_eq!(w[4], 4); // ctx
        assert_eq!(&w[5..9], &1u32.to_le_bytes());
        assert_eq!(ring.pending(), 0);
    }

    #[test]
    fn refused_frame_stays_in_ring_and_drops_are_kept() {
        let ring = Ring::<128>::new();
        let (mut p, mut c) = ring.split(1).unwrap();
        p.push_at(1, 0, &[1; 8]);
        let mut scratch = [0u8; FRAME_MAX];
        let mut tiny = [0u8; 2];
        let mut t = SliceTransport::new(&mut tiny);
        assert_eq!(drain_ring(&mut c, &mut t, &mut scratch).frames, 0);
        assert!(ring.pending() > 0);

        while p.push_at(1, 0, &[1; 8]) {}
        let before = ring.drops();
        assert_eq!(drain_ring(&mut c, &mut t, &mut scratch).dropped_reported, 0);
        assert_eq!(ring.drops(), before);
    }

    // Reads the link-section site table for the contract hash, which Miri cannot.
    #[cfg_attr(miri, ignore)]
    #[test]
    fn resync_frame_starts_with_the_marker_and_holds_the_header() {
        let mut buf = [0u8; 64];
        let mut t = SliceTransport::new(&mut buf);
        write_resync(&mut t).unwrap();
        let w = t.written();
        assert_eq!(w.len(), 4 + wire::HEADER_LEN);
        assert_eq!(&w[..8], &wire::RESYNC_MARKER);
        assert!(Header::decode(&w[4..]).is_some());
    }

    #[test]
    fn frame_delta_reads_the_timestamp() {
        let mut f = [0u8; 8];
        f[..4].copy_from_slice(&7u32.to_le_bytes());
        f[4] = 0x85; // 133 = 0x85 0x01
        f[5] = 0x01;
        assert_eq!(frame_delta(&f), 133);
        assert_eq!(frame_delta(&f[..3]), 0);
    }

    /// Counts flushes so a test can see `flush` reach the inner transport.
    struct Flushes<'a> {
        inner: SliceTransport<'a>,
        flushed: usize,
    }

    impl Transport for Flushes<'_> {
        fn write(&mut self, frame: &[u8]) -> Result<(), Full> {
            self.inner.write(frame)
        }
        fn flush(&mut self) {
            self.flushed += 1;
        }
    }

    #[test]
    fn a_mutable_reference_forwards_writes_and_flushes() {
        let mut buf = [0u8; 8];
        let mut inner = Flushes {
            inner: SliceTransport::new(&mut buf),
            flushed: 0,
        };
        {
            let mut t = &mut inner;
            write_ctx_close(&mut t, 3).unwrap();
            assert_eq!(write_dropped(&mut t, 3, 9), Err(Full), "9 bytes > 3 left");
            Transport::flush(&mut t);
        }
        assert_eq!(inner.flushed, 1);
        let mut want = wire::CTRL_CTX_CLOSE.to_le_bytes().to_vec();
        want.push(3);
        assert_eq!(inner.inner.written(), &want[..]);
    }

    #[test]
    fn vec_and_io_transports_take_every_frame() {
        let mut v: std::vec::Vec<u8> = std::vec::Vec::new();
        write_ctx_time(&mut v, 1, 0x0102).unwrap();
        assert_eq!(&v[..4], &wire::CTRL_CTX_TIME.to_le_bytes());
        assert_eq!(&v[4..], &[1, 2, 1, 0, 0, 0, 0, 0, 0]);

        let mut io = IoTransport(std::vec::Vec::new());
        io.write(&[1, 2, 3]).unwrap();
        io.write(&[4]).unwrap();
        io.flush();
        assert_eq!(io.0, [1, 2, 3, 4]);
    }

    /// A writer that fails every write and flush, like a closed socket.
    struct Broken {
        flushes: usize,
    }

    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }

    #[test]
    fn an_io_error_is_reported_as_full_and_a_failed_flush_is_ignored() {
        let mut t = IoTransport(Broken { flushes: 0 });
        assert_eq!(t.write(&[1]), Err(Full));
        t.flush();
        assert_eq!(t.0.flushes, 1);

        // A drain pass into it keeps the frame in the ring.
        let ring = Ring::<128>::new();
        let (mut p, mut c) = ring.split(1).unwrap();
        assert!(p.push_at(1, 0, &[5]));
        let mut scratch = [0u8; FRAME_MAX];
        assert_eq!(
            drain_ring(&mut c, &mut t, &mut scratch),
            PassStats::default()
        );
        assert!(ring.pending() > 0);
    }

    #[test]
    fn a_long_context_name_is_capped() {
        let mut buf = [0u8; 256];
        let mut t = SliceTransport::new(&mut buf);
        let name = "n".repeat(200);
        write_ctx_register(&mut t, 1, &name).unwrap();
        let w = t.written();
        assert_eq!(w[5] as usize, crate::arg::DYN_CAP);
        assert_eq!(w.len(), 4 + 1 + 1 + crate::arg::DYN_CAP);
    }

    #[test]
    fn ctx_register_frame_layout() {
        let mut buf = [0u8; 64];
        let mut t = SliceTransport::new(&mut buf);
        write_ctx_register(&mut t, 2, "motor").unwrap();
        let w = t.written();
        assert_eq!(&w[..4], &wire::CTRL_CTX_REGISTER.to_le_bytes());
        assert_eq!(&w[4..], &[2, 5, b'm', b'o', b't', b'o', b'r']);
    }
}
