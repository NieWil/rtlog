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
/// Payload: full stream header.
pub const CTRL_RESYNC: u32 = CONTROL_BASE;
/// Payload: ctx varint, u32 count of frames dropped since last report.
pub const CTRL_DROPPED: u32 = CONTROL_BASE + 1;
/// Payload: ctx varint, name as a dynamic string (len byte + UTF-8).
pub const CTRL_CTX_REGISTER: u32 = CONTROL_BASE + 2;
/// Payload: ctx varint.
pub const CTRL_CTX_CLOSE: u32 = CONTROL_BASE + 3;

/// Stream header, sent once at the start of a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub wire_version: u8,
    pub scheme: u8,
    pub contract_hash: u32,
    pub clock_hz: u32,
    pub ptr_width: u8,
    pub flags: u8,
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
