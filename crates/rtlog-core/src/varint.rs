//! LEB128 unsigned varints, used for metadata fields only (timestamps, contexts).

/// Maximum encoded size of a `u64` varint.
pub const MAX_U64: usize = 10;

/// Encode `v` into `out`, returning the number of bytes written.
///
/// `out` must hold at least [`MAX_U64`] bytes.
#[inline]
pub fn encode(mut v: u64, out: &mut [u8]) -> usize {
    let mut i = 0;
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out[i] = byte;
            return i + 1;
        }
        out[i] = byte | 0x80;
        i += 1;
    }
}

/// Decode a varint from the start of `input`.
///
/// Returns the value and the number of bytes consumed, or `None` if the input
/// is truncated or the value does not fit in 64 bits.
pub fn decode(input: &[u8]) -> Option<(u64, usize)> {
    let mut v: u64 = 0;
    for (i, &byte) in input.iter().enumerate().take(MAX_U64) {
        let bits = u64::from(byte & 0x7f);
        if i == MAX_U64 - 1 && bits > 1 {
            return None; // would overflow 64 bits
        }
        v |= bits << (7 * i);
        if byte & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_edges() {
        for v in [
            0,
            1,
            127,
            128,
            300,
            16_383,
            16_384,
            u32::MAX as u64,
            u64::MAX,
        ] {
            let mut buf = [0u8; MAX_U64];
            let n = encode(v, &mut buf);
            assert_eq!(decode(&buf[..n]), Some((v, n)), "value {v}");
        }
    }

    #[test]
    fn sizes() {
        let mut buf = [0u8; MAX_U64];
        assert_eq!(encode(0, &mut buf), 1);
        assert_eq!(encode(127, &mut buf), 1);
        assert_eq!(encode(128, &mut buf), 2);
        assert_eq!(encode(u32::MAX as u64, &mut buf), 5);
        assert_eq!(encode(u64::MAX, &mut buf), 10);
    }

    #[test]
    fn rejects_truncated_and_overflow() {
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[0x80]), None);
        assert_eq!(decode(&[0xff; 10]), None);
        assert_eq!(
            decode(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02]),
            None
        );
    }
}
