//! Typed argument values parsed from untrusted frame bytes.

use rtlog::arg::{Tag, TRUNCATED};
use serde::Serialize;

/// Deepest `Option`/`Array` nesting accepted from a stream.
pub const MAX_DEPTH: usize = 8;

/// One decoded argument.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Value {
    /// Any unsigned integer (`U8` to `U64`), widened to `u64`.
    U(u64),
    /// Any signed integer (`I8` to `I64`), widened to `i64`.
    I(i64),
    /// An `F32` or `F64`, widened to `f64`.
    F(f64),
    /// A `bool`.
    Bool(bool),
    /// A `char`.
    Char(char),
    /// A string argument.
    Str {
        /// The text that was recorded, at most 127 bytes.
        s: String,
        /// The target cut the string to fit; rendered with a trailing `…`.
        truncated: bool,
    },
    /// A byte slice argument.
    Bytes {
        /// The bytes that were recorded, at most 127.
        bytes: Vec<u8>,
        /// The target cut the slice to fit; rendered with a trailing `…`.
        truncated: bool,
    },
    /// An `Option`: `None`, or the inner value.
    Option(Option<Box<Value>>),
    /// A fixed-size array, element by element.
    Array(Vec<Value>),
}

/// Why argument bytes could not be parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueError {
    /// The input ended inside a value, or an array count exceeds what the
    /// remaining bytes could hold.
    Truncated,
    /// The tag byte is not a known [`Tag`]; holds the byte.
    BadTag(u8),
    /// A `Str` payload is not valid UTF-8.
    BadUtf8,
    /// A `Char` payload is not a Unicode scalar value.
    BadChar,
    /// A `Bool` value or an `Option` presence byte is neither 0 nor 1.
    BadBool,
    /// `Option`/`Array` nesting is deeper than [`MAX_DEPTH`].
    TooDeep,
}

fn take<'a>(input: &mut &'a [u8], n: usize) -> Result<&'a [u8], ValueError> {
    if input.len() < n {
        return Err(ValueError::Truncated);
    }
    let (a, b) = input.split_at(n);
    *input = b;
    Ok(a)
}

fn arr<const N: usize>(input: &mut &[u8]) -> Result<[u8; N], ValueError> {
    let s = take(input, N)?;
    let mut a = [0u8; N];
    a.copy_from_slice(s);
    Ok(a)
}

/// Parse one tagged value from the front of `input`, advancing it.
pub fn parse(input: &mut &[u8]) -> Result<Value, ValueError> {
    parse_depth(input, 0)
}

fn parse_depth(input: &mut &[u8], depth: usize) -> Result<Value, ValueError> {
    if depth > MAX_DEPTH {
        return Err(ValueError::TooDeep);
    }
    let tag_byte = take(input, 1)?[0];
    let tag = Tag::from_u8(tag_byte).ok_or(ValueError::BadTag(tag_byte))?;
    Ok(match tag {
        Tag::U8 => Value::U(u64::from(arr::<1>(input)?[0])),
        Tag::U16 => Value::U(u64::from(u16::from_le_bytes(arr(input)?))),
        Tag::U32 => Value::U(u64::from(u32::from_le_bytes(arr(input)?))),
        Tag::U64 => Value::U(u64::from_le_bytes(arr(input)?)),
        Tag::I8 => Value::I(i64::from(i8::from_le_bytes(arr(input)?))),
        Tag::I16 => Value::I(i64::from(i16::from_le_bytes(arr(input)?))),
        Tag::I32 => Value::I(i64::from(i32::from_le_bytes(arr(input)?))),
        Tag::I64 => Value::I(i64::from_le_bytes(arr(input)?)),
        Tag::F32 => Value::F(f64::from(f32::from_le_bytes(arr(input)?))),
        Tag::F64 => Value::F(f64::from_le_bytes(arr(input)?)),
        Tag::Bool => match arr::<1>(input)?[0] {
            0 => Value::Bool(false),
            1 => Value::Bool(true),
            _ => return Err(ValueError::BadBool),
        },
        Tag::Char => {
            Value::Char(char::from_u32(u32::from_le_bytes(arr(input)?)).ok_or(ValueError::BadChar)?)
        }
        Tag::Str | Tag::Bytes => {
            let len = arr::<1>(input)?[0];
            let truncated = len & TRUNCATED != 0;
            let body = take(input, usize::from(len & !TRUNCATED))?;
            if tag == Tag::Str {
                let s = std::str::from_utf8(body).map_err(|_| ValueError::BadUtf8)?;
                Value::Str {
                    s: s.to_owned(),
                    truncated,
                }
            } else {
                Value::Bytes {
                    bytes: body.to_vec(),
                    truncated,
                }
            }
        }
        Tag::Option => match arr::<1>(input)?[0] {
            0 => Value::Option(None),
            1 => Value::Option(Some(Box::new(parse_depth(input, depth + 1)?))),
            _ => return Err(ValueError::BadBool),
        },
        Tag::Array => {
            let n = usize::from(u16::from_le_bytes(arr(input)?));
            // Every element takes at least two bytes, so a huge count cannot
            // make us allocate more than the input justifies.
            if n > input.len() / 2 {
                return Err(ValueError::Truncated);
            }
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(parse_depth(input, depth + 1)?);
            }
            Value::Array(v)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtlog::{Log, Writer};

    fn roundtrip<T: Log + ?Sized>(v: &T) -> Value {
        let mut buf = [0u8; 512];
        let mut w = Writer::new(&mut buf);
        v.write(&mut w);
        let n = w.len();
        let mut s = &buf[..n];
        let out = parse(&mut s).unwrap();
        assert!(s.is_empty());
        out
    }

    #[test]
    fn all_types_roundtrip() {
        assert_eq!(roundtrip(&200u8), Value::U(200));
        assert_eq!(roundtrip(&65_000u16), Value::U(65_000));
        assert_eq!(roundtrip(&u32::MAX), Value::U(u32::MAX as u64));
        assert_eq!(roundtrip(&u64::MAX), Value::U(u64::MAX));
        assert_eq!(roundtrip(&-5i8), Value::I(-5));
        assert_eq!(roundtrip(&-500i16), Value::I(-500));
        assert_eq!(roundtrip(&i32::MIN), Value::I(i32::MIN as i64));
        assert_eq!(roundtrip(&i64::MIN), Value::I(i64::MIN));
        assert_eq!(roundtrip(&0.5f32), Value::F(0.5));
        assert_eq!(roundtrip(&-2.25f64), Value::F(-2.25));
        assert_eq!(roundtrip(&true), Value::Bool(true));
        assert_eq!(roundtrip(&'ß'), Value::Char('ß'));
        assert_eq!(roundtrip(&7usize), Value::U(7));
        assert_eq!(roundtrip(&-7isize), Value::I(-7));
        assert_eq!(
            roundtrip("héllo"),
            Value::Str {
                s: "héllo".into(),
                truncated: false
            }
        );
        assert_eq!(
            roundtrip(&[1u8, 2][..]),
            Value::Bytes {
                bytes: vec![1, 2],
                truncated: false
            }
        );
        assert_eq!(
            roundtrip(&Some(3u8)),
            Value::Option(Some(Box::new(Value::U(3))))
        );
        assert_eq!(roundtrip(&None::<u8>), Value::Option(None));
        assert_eq!(
            roundtrip(&[1i16, -1]),
            Value::Array(vec![Value::I(1), Value::I(-1)])
        );
        assert_eq!(
            roundtrip(&String::from("s")),
            Value::Str {
                s: "s".into(),
                truncated: false
            }
        );
    }

    #[test]
    fn bool_false_parses() {
        let mut input = &[Tag::Bool as u8, 0, 0xee][..];
        assert_eq!(parse(&mut input), Ok(Value::Bool(false)));
        assert_eq!(input, [0xee], "only the value's two bytes are consumed");
    }

    #[test]
    fn truncated_flag_survives() {
        let long = "z".repeat(300);
        match roundtrip(long.as_str()) {
            Value::Str { s, truncated } => {
                assert!(truncated);
                assert_eq!(s.len(), rtlog::arg::DYN_CAP);
            }
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(parse(&mut &[][..]), Err(ValueError::Truncated));
        assert_eq!(parse(&mut &[0u8][..]), Err(ValueError::BadTag(0)));
        assert_eq!(
            parse(&mut &[Tag::U32 as u8, 1, 2][..]),
            Err(ValueError::Truncated)
        );
        assert_eq!(
            parse(&mut &[Tag::Bool as u8, 2][..]),
            Err(ValueError::BadBool)
        );
        assert_eq!(
            parse(&mut &[Tag::Char as u8, 0, 0xd8, 0, 0][..]),
            Err(ValueError::BadChar)
        );
        assert_eq!(
            parse(&mut &[Tag::Str as u8, 1, 0xff][..]),
            Err(ValueError::BadUtf8)
        );
        assert_eq!(
            parse(&mut &[Tag::Array as u8, 0xff, 0xff, 1][..]),
            Err(ValueError::Truncated)
        );
        let deep: Vec<u8> = std::iter::repeat_n([Tag::Option as u8, 1], 20)
            .flatten()
            .chain([Tag::U8 as u8, 0])
            .collect();
        assert_eq!(parse(&mut &deep[..]), Err(ValueError::TooDeep));
    }
}
