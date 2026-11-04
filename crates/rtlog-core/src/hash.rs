//! FNV-1a 32-bit hashing, usable in `const` context.

/// FNV-1a offset basis.
pub const FNV_OFFSET: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;

/// Continue an FNV-1a hash with `bytes`.
pub const fn fnv1a_extend(mut hash: u32, bytes: &[u8]) -> u32 {
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u32;
        hash = hash.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    hash
}

/// FNV-1a 32-bit hash of `bytes`.
pub const fn fnv1a32(bytes: &[u8]) -> u32 {
    fnv1a_extend(FNV_OFFSET, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // Reference values from the FNV specification.
        assert_eq!(fnv1a32(b""), 0x811c_9dc5);
        assert_eq!(fnv1a32(b"a"), 0xe40c_292c);
        assert_eq!(fnv1a32(b"foobar"), 0xbf9c_f968);
    }

    #[test]
    fn extend_is_concatenation() {
        assert_eq!(fnv1a_extend(fnv1a32(b"foo"), b"bar"), fnv1a32(b"foobar"));
    }

    #[test]
    fn usable_in_const() {
        const H: u32 = fnv1a32(b"rtlog");
        assert_eq!(H, fnv1a32(b"rtlog"));
    }
}
