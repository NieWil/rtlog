//! Argument model: every argument implements [`Log`] and is written as raw bytes.
//!
//! On the wire each argument is one [`Tag`] byte followed by its value, so the
//! host can decode a frame from the contract's argument count alone.

use core::marker::PhantomData;
use core::mem::MaybeUninit;

/// Maximum bytes of a dynamic string or byte slice argument (tier 3).
///
/// The length byte uses bit 7 as the truncation flag, so the cap is 127.
pub const DYN_CAP: usize = 127;

/// Truncation flag in a dynamic length byte.
pub const TRUNCATED: u8 = 0x80;

/// Wire type of one argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Tag {
    /// `u8`, 1 byte.
    U8 = 1,
    /// `u16`, 2 bytes little-endian.
    U16 = 2,
    /// `u32`, 4 bytes little-endian.
    U32 = 3,
    /// `u64`, 8 bytes little-endian.
    U64 = 4,
    /// `i8`, 1 byte.
    I8 = 5,
    /// `i16`, 2 bytes little-endian.
    I16 = 6,
    /// `i32`, 4 bytes little-endian.
    I32 = 7,
    /// `i64`, 8 bytes little-endian.
    I64 = 8,
    /// `f32`, 4 bytes little-endian (IEEE 754 bits).
    F32 = 9,
    /// `f64`, 8 bytes little-endian (IEEE 754 bits).
    F64 = 10,
    /// `bool`, 1 byte: 0 or 1.
    Bool = 11,
    /// `char`, its scalar value as 4 bytes little-endian.
    Char = 12,
    /// `u8` length (bit 7 = truncated) + UTF-8 bytes.
    Str = 13,
    /// `u8` length (bit 7 = truncated) + raw bytes.
    Bytes = 14,
    /// One presence byte (0 or 1), then the inner tagged value when present.
    Option = 15,
    /// `u16` count + that many tagged values.
    Array = 16,
}

impl Tag {
    /// Parse a tag byte.
    pub fn from_u8(v: u8) -> Option<Tag> {
        Some(match v {
            1 => Tag::U8,
            2 => Tag::U16,
            3 => Tag::U32,
            4 => Tag::U64,
            5 => Tag::I8,
            6 => Tag::I16,
            7 => Tag::I32,
            8 => Tag::I64,
            9 => Tag::F32,
            10 => Tag::F64,
            11 => Tag::Bool,
            12 => Tag::Char,
            13 => Tag::Str,
            14 => Tag::Bytes,
            15 => Tag::Option,
            16 => Tag::Array,
            _ => return None,
        })
    }
}

/// Bounded writer over a fixed buffer. Writes past the end are dropped and
/// flagged, so a log call can never overrun its frame buffer.
///
/// It can wrap an uninitialised buffer, so a log call pays no memset.
pub struct Writer<'a> {
    ptr: *mut u8,
    cap: usize,
    pos: usize,
    overflow: bool,
    _buf: PhantomData<&'a mut [u8]>,
}

impl<'a> Writer<'a> {
    /// Wrap an initialised buffer.
    #[inline]
    pub fn new(buf: &'a mut [u8]) -> Self {
        Writer {
            ptr: buf.as_mut_ptr(),
            cap: buf.len(),
            pos: 0,
            overflow: false,
            _buf: PhantomData,
        }
    }

    /// Wrap an uninitialised buffer; only the written prefix is ever read.
    #[inline(always)]
    pub fn new_uninit(buf: &'a mut [MaybeUninit<u8>]) -> Self {
        Writer {
            ptr: buf.as_mut_ptr().cast(),
            cap: buf.len(),
            pos: 0,
            overflow: false,
            _buf: PhantomData,
        }
    }

    /// A writer over `cap` bytes at `ptr` (ring memory on the fast path).
    ///
    /// SAFETY: `[ptr, ptr + cap)` must be valid for writes and exclusively owned
    /// for `'a`.
    #[inline(always)]
    pub(crate) unsafe fn from_raw(ptr: *mut u8, cap: usize) -> Self {
        Writer {
            ptr,
            cap,
            pos: 0,
            overflow: false,
            _buf: PhantomData,
        }
    }

    /// Bytes written so far.
    #[inline]
    pub fn len(&self) -> usize {
        self.pos
    }

    /// Whether nothing was written.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.pos == 0
    }

    /// Whether a write did not fit.
    #[inline]
    pub fn overflowed(&self) -> bool {
        self.overflow
    }

    /// The bytes written.
    #[inline(always)]
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `[0, pos)` lies inside the buffer and has been written.
        unsafe { core::slice::from_raw_parts(self.ptr, self.pos) }
    }

    /// Append `bytes`, or mark overflow if they do not fit.
    #[inline(always)]
    pub fn put(&mut self, bytes: &[u8]) {
        if bytes.len() <= self.cap - self.pos {
            // SAFETY: bounds checked above; `bytes` cannot alias our exclusive buffer.
            unsafe {
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr.add(self.pos), bytes.len())
            };
            self.pos += bytes.len();
        } else {
            self.overflow = true;
        }
    }

    /// Append a fixed-size array. The size is a constant, so this compiles to
    /// plain stores instead of a `memcpy` call.
    #[inline(always)]
    pub fn put_array<const N: usize>(&mut self, bytes: [u8; N]) {
        if N <= self.cap - self.pos {
            // SAFETY: bounds checked above; unaligned store of a byte array.
            unsafe { core::ptr::write_unaligned(self.ptr.add(self.pos).cast::<[u8; N]>(), bytes) };
            self.pos += N;
        } else {
            self.overflow = true;
        }
    }

    /// Append one byte.
    #[inline(always)]
    pub fn put_u8(&mut self, b: u8) {
        self.put_array([b]);
    }

    /// Append a dynamic (tier 3) payload: length byte with truncation flag, then bytes.
    #[inline]
    pub fn put_dyn(&mut self, bytes: &[u8]) {
        if bytes.len() > DYN_CAP {
            self.put_u8(DYN_CAP as u8 | TRUNCATED);
            self.put(&bytes[..DYN_CAP]);
        } else {
            self.put_u8(bytes.len() as u8);
            self.put(bytes);
        }
    }
}

/// A value that can be logged as raw bytes.
pub trait Log {
    /// Wire tag of this type.
    const TAG: Tag;
    /// Upper bound on wire bytes, tag included.
    const MAX: usize;
    /// Write the tag and value.
    fn write(&self, w: &mut Writer<'_>);
}

macro_rules! impl_num {
    ($($t:ty => $tag:ident),* $(,)?) => {$(
        impl Log for $t {
            const TAG: Tag = Tag::$tag;
            const MAX: usize = 1 + core::mem::size_of::<$t>();
            #[inline(always)]
            fn write(&self, w: &mut Writer<'_>) {
                w.put_u8(Tag::$tag as u8);
                w.put_array(self.to_le_bytes());
            }
        }
    )*};
}

impl_num!(
    u8 => U8, u16 => U16, u32 => U32, u64 => U64,
    i8 => I8, i16 => I16, i32 => I32, i64 => I64,
    f32 => F32, f64 => F64,
);

// usize/isize are recorded at the target's pointer width, as `u32`/`u64`.
impl Log for usize {
    const TAG: Tag = if core::mem::size_of::<usize>() == 8 {
        Tag::U64
    } else {
        Tag::U32
    };
    const MAX: usize = 1 + core::mem::size_of::<usize>();
    #[inline(always)]
    fn write(&self, w: &mut Writer<'_>) {
        w.put_u8(Self::TAG as u8);
        w.put_array(self.to_le_bytes());
    }
}

impl Log for isize {
    const TAG: Tag = if core::mem::size_of::<isize>() == 8 {
        Tag::I64
    } else {
        Tag::I32
    };
    const MAX: usize = 1 + core::mem::size_of::<isize>();
    #[inline(always)]
    fn write(&self, w: &mut Writer<'_>) {
        w.put_u8(Self::TAG as u8);
        w.put_array(self.to_le_bytes());
    }
}

impl Log for bool {
    const TAG: Tag = Tag::Bool;
    const MAX: usize = 2;
    #[inline(always)]
    fn write(&self, w: &mut Writer<'_>) {
        w.put_array([Tag::Bool as u8, *self as u8]);
    }
}

impl Log for char {
    const TAG: Tag = Tag::Char;
    const MAX: usize = 5;
    #[inline(always)]
    fn write(&self, w: &mut Writer<'_>) {
        w.put_u8(Tag::Char as u8);
        w.put_array((*self as u32).to_le_bytes());
    }
}

impl Log for str {
    const TAG: Tag = Tag::Str;
    const MAX: usize = 2 + DYN_CAP;
    #[inline]
    fn write(&self, w: &mut Writer<'_>) {
        w.put_u8(Tag::Str as u8);
        // Truncate on a char boundary so the host always gets valid UTF-8.
        let mut end = self.len().min(DYN_CAP);
        while !self.is_char_boundary(end) {
            end -= 1;
        }
        if end < self.len() {
            w.put_u8(end as u8 | TRUNCATED);
            w.put(&self.as_bytes()[..end]);
        } else {
            w.put_dyn(self.as_bytes());
        }
    }
}

impl Log for [u8] {
    const TAG: Tag = Tag::Bytes;
    const MAX: usize = 2 + DYN_CAP;
    #[inline]
    fn write(&self, w: &mut Writer<'_>) {
        w.put_u8(Tag::Bytes as u8);
        w.put_dyn(self);
    }
}

impl<T: Log + ?Sized> Log for &T {
    const TAG: Tag = T::TAG;
    const MAX: usize = T::MAX;
    #[inline(always)]
    fn write(&self, w: &mut Writer<'_>) {
        (**self).write(w)
    }
}

impl<T: Log> Log for Option<T> {
    const TAG: Tag = Tag::Option;
    const MAX: usize = 2 + T::MAX;
    #[inline]
    fn write(&self, w: &mut Writer<'_>) {
        w.put_u8(Tag::Option as u8);
        match self {
            Some(v) => {
                w.put_u8(1);
                v.write(w);
            }
            None => w.put_u8(0),
        }
    }
}

impl<T: Log, const N: usize> Log for [T; N] {
    const TAG: Tag = Tag::Array;
    const MAX: usize = 3 + N * T::MAX;
    #[inline]
    fn write(&self, w: &mut Writer<'_>) {
        w.put_u8(Tag::Array as u8);
        w.put_array((N as u16).to_le_bytes());
        for v in self {
            v.write(w);
        }
    }
}

#[cfg(feature = "std")]
impl Log for std::string::String {
    const TAG: Tag = Tag::Str;
    const MAX: usize = <str as Log>::MAX;
    #[inline]
    fn write(&self, w: &mut Writer<'_>) {
        self.as_str().write(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc<T: Log + ?Sized>(v: &T) -> ([u8; 512], usize) {
        let mut buf = [0u8; 512];
        let mut w = Writer::new(&mut buf);
        v.write(&mut w);
        let n = w.len();
        assert!(!w.overflowed());
        assert!(n <= T::MAX, "{} > MAX {}", n, T::MAX);
        (buf, n)
    }

    #[test]
    fn ints_are_tagged_little_endian() {
        let (b, n) = enc(&0x1234u16);
        assert_eq!(&b[..n], &[Tag::U16 as u8, 0x34, 0x12]);
        let (b, n) = enc(&-1i32);
        assert_eq!(&b[..n], &[Tag::I32 as u8, 0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn floats_bool_char() {
        let (b, n) = enc(&1.5f32);
        assert_eq!(&b[..n], &[Tag::F32 as u8, 0, 0, 0xc0, 0x3f]);
        let (b, n) = enc(&true);
        assert_eq!(&b[..n], &[Tag::Bool as u8, 1]);
        let (b, n) = enc(&'é');
        assert_eq!(&b[..n], &[Tag::Char as u8, 0xe9, 0, 0, 0]);
    }

    #[test]
    fn strings_and_truncation() {
        let (b, n) = enc("hi");
        assert_eq!(&b[..n], &[Tag::Str as u8, 2, b'h', b'i']);
        let long = "x".repeat(200);
        let (b, n) = enc(long.as_str());
        assert_eq!(b[1], DYN_CAP as u8 | TRUNCATED);
        assert_eq!(n, 2 + DYN_CAP);
    }

    #[test]
    fn truncation_keeps_utf8() {
        // 126 ASCII bytes then a 2-byte char straddling the cap.
        let s = format!("{}é", "a".repeat(126));
        let (b, n) = enc(s.as_str());
        assert_eq!(b[1], 126 | TRUNCATED);
        assert!(core::str::from_utf8(&b[2..n]).is_ok());
    }

    #[test]
    fn bytes_option_array() {
        let (b, n) = enc(&[1u8, 2, 3][..]);
        assert_eq!(&b[..n], &[Tag::Bytes as u8, 3, 1, 2, 3]);
        let (b, n) = enc(&Some(7u8));
        assert_eq!(&b[..n], &[Tag::Option as u8, 1, Tag::U8 as u8, 7]);
        let (b, n) = enc(&None::<u8>);
        assert_eq!(&b[..n], &[Tag::Option as u8, 0]);
        let (b, n) = enc(&[1u8, 2]);
        assert_eq!(
            &b[..n],
            &[Tag::Array as u8, 2, 0, Tag::U8 as u8, 1, Tag::U8 as u8, 2]
        );
    }

    #[test]
    fn writer_overflow_is_flagged_not_written() {
        let mut buf = [0u8; 3];
        let mut w = Writer::new(&mut buf);
        0xdead_beefu32.write(&mut w);
        assert!(w.overflowed());
        assert_eq!(w.len(), 1); // tag fit, value did not
    }

    #[test]
    fn tag_roundtrip() {
        for v in 1..=16u8 {
            assert_eq!(Tag::from_u8(v).map(|t| t as u8), Some(v));
        }
        assert_eq!(Tag::from_u8(0), None);
        assert_eq!(Tag::from_u8(17), None);
    }
}
