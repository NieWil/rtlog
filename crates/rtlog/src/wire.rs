//! Stream header and control frame constants (spec section 5).

/// Stream magic.
pub const MAGIC: [u8; 4] = *b"RTL1";
/// Frame grammar version.
pub const WIRE_VERSION: u8 = 1;
/// Site ID scheme: 32-bit content hash.
pub const SCHEME_HASH32: u8 = 0;
/// Encoded header size in bytes.
pub const HEADER_LEN: usize = 4 + 1 + 1 + 4 + 4 + 1 + 1 + 1;

/// First reserved site ID; IDs at or above it are control frames.
pub const CONTROL_BASE: u32 = 0xFFFF_FF00;
/// Payload: a copy of the 17-byte stream header. The drain writes one every
/// [`RESYNC_EVERY`] bytes, followed by a `ctx_register` and a `ctx_time` frame
/// per live context, so a decoder that lost sync can pick the stream up again.
pub const CTRL_RESYNC: u32 = CONTROL_BASE;
/// The first bytes of every resync frame: its ID (little-endian) then the
/// header magic. A decoder searches for these after a bad frame.
pub const RESYNC_MARKER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, b'R', b'T', b'L', b'1'];
/// Bytes of frames between two resync frames.
pub const RESYNC_EVERY: usize = 64 * 1024;
/// Payload: ctx varint, u32 count of frames dropped since last report.
pub const CTRL_DROPPED: u32 = CONTROL_BASE + 1;
/// Payload: ctx varint, name as a dynamic string (len byte + UTF-8).
pub const CTRL_CTX_REGISTER: u32 = CONTROL_BASE + 2;
/// Payload: ctx varint.
pub const CTRL_CTX_CLOSE: u32 = CONTROL_BASE + 3;
/// Timed like a log frame (ts delta, ctx), then: u32 span site ID, u64 span ID,
/// u64 parent span ID (0 = root).
pub const CTRL_SPAN_OPEN: u32 = CONTROL_BASE + 4;
/// Timed like a log frame (ts delta, ctx), then: u64 span ID.
pub const CTRL_SPAN_CLOSE: u32 = CONTROL_BASE + 5;

/// Timed like a log frame (ts delta, ctx), then: u8 kind (1 = panic,
/// 2 = signal), i32 signal, i32 signal code, u64 fault address, u64 program
/// counter, message as a dynamic string (len byte + UTF-8). Written by the
/// crash handler as the last frame of a crash dump (spec section 8b).
pub const CTRL_CRASH: u32 = CONTROL_BASE + 7;
/// Payload: ctx varint, u64 absolute ticks. Sets the context's clock, so the
/// deltas that follow decode to absolute times even when earlier frames of
/// that context are in another stream (a crash dump holds only the tail).
pub const CTRL_CTX_TIME: u32 = CONTROL_BASE + 9;

/// Crash kinds in a `crash` frame.
pub const CRASH_PANIC: u8 = 1;
/// See [`CRASH_PANIC`].
pub const CRASH_SIGNAL: u8 = 2;

/// Encode a `span_open` payload.
pub fn span_open_payload(site: u32, id: u64, parent: u64) -> [u8; 20] {
    let mut b = [0u8; 20];
    b[..4].copy_from_slice(&site.to_le_bytes());
    b[4..12].copy_from_slice(&id.to_le_bytes());
    b[12..].copy_from_slice(&parent.to_le_bytes());
    b
}

/// Stream header, sent once at the start of a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Frame grammar version; [`WIRE_VERSION`] for this crate.
    pub wire_version: u8,
    /// Site ID scheme; [`SCHEME_HASH32`] (1 is reserved).
    pub scheme: u8,
    /// Hash of the contract's site IDs; see [`crate::site::contract_hash`].
    pub contract_hash: u32,
    /// Timestamp rate in ticks per second.
    pub clock_hz: u32,
    /// Size of `usize` on the target in bytes (4 or 8).
    pub ptr_width: u8,
    /// 0; bits reserved for drain compression and authenticated or encrypted blocks.
    pub flags: u8,
    /// 0; reserved.
    pub key_id: u8,
}

impl Header {
    /// Header for this target with the given contract hash and clock rate.
    pub const fn new(contract_hash: u32, clock_hz: u32) -> Self {
        Header {
            wire_version: WIRE_VERSION,
            scheme: SCHEME_HASH32,
            contract_hash,
            clock_hz,
            ptr_width: core::mem::size_of::<usize>() as u8,
            flags: 0,
            key_id: 0,
        }
    }

    /// Encode to bytes.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..4].copy_from_slice(&MAGIC);
        b[4] = self.wire_version;
        b[5] = self.scheme;
        b[6..10].copy_from_slice(&self.contract_hash.to_le_bytes());
        b[10..14].copy_from_slice(&self.clock_hz.to_le_bytes());
        b[14] = self.ptr_width;
        b[15] = self.flags;
        b[16] = self.key_id;
        b
    }

    /// Decode from the start of `b`; `None` if truncated or the magic is wrong.
    pub fn decode(b: &[u8]) -> Option<Header> {
        if b.len() < HEADER_LEN || b[0..4] != MAGIC {
            return None;
        }
        Some(Header {
            wire_version: b[4],
            scheme: b[5],
            contract_hash: u32::from_le_bytes([b[6], b[7], b[8], b[9]]),
            clock_hz: u32::from_le_bytes([b[10], b[11], b[12], b[13]]),
            ptr_width: b[14],
            flags: b[15],
            key_id: b[16],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = Header::new(0xdead_beef, 1_000_000_000);
        let b = h.encode();
        assert_eq!(&b[..4], b"RTL1");
        assert_eq!(Header::decode(&b), Some(h));
    }

    #[test]
    fn header_rejects_bad_magic_and_truncation() {
        let mut b = Header::new(1, 1).encode();
        assert_eq!(Header::decode(&b[..HEADER_LEN - 1]), None);
        b[0] = b'X';
        assert_eq!(Header::decode(&b), None);
    }
}
