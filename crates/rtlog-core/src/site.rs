//! Log site descriptors and the build-time contract (spec section 4).
//!
//! Every macro call emits one `static` [`Site`] into a link section collected by
//! `linkme`, so the full contract is known from the binary alone, whether or not
//! a site ever runs.

use crate::hash::{fnv1a32, fnv1a_extend};
use crate::level::Level;
use crate::wire::CONTROL_BASE;

/// What a site describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SiteKind {
    /// A log call; `fmt` is its format string.
    Log = 0,
    /// A span; `fmt` is its name.
    Span = 1,
}

/// One log call site or span site.
#[derive(Debug)]
pub struct Site {
    pub id: u32,
    pub kind: SiteKind,
    pub level: Level,
    pub module: &'static str,
    pub file: &'static str,
    pub line: u32,
    pub column: u32,
    pub fmt: &'static str,
    pub nargs: u8,
}

impl Site {
    /// Build a descriptor; the ID is a content hash of every other field.
    pub const fn new(
        level: Level,
        module: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
        fmt: &'static str,
        nargs: u8,
    ) -> Site {
        Site {
            id: site_id(level, module, file, line, column, fmt, nargs),
            kind: SiteKind::Log,
            level,
            module,
            file,
            line,
            column,
            fmt,
            nargs,
        }
    }
}

impl Site {
    /// Build a span site; `name` is shown as the slice name on the host.
    pub const fn span(
        module: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
        name: &'static str,
    ) -> Site {
        let id = site_id(Level::Info, module, file, line, column, name, 0) ^ 0x5a5a_0000;
        Site {
            id: if id >= CONTROL_BASE {
                id ^ 0x8000_0000
            } else {
                id
            },
            kind: SiteKind::Span,
            level: Level::Info,
            module,
            file,
            line,
            column,
            fmt: name,
            nargs: 0,
        }
    }
}

/// `fnv1a32(module | file | line | column | fmt | level | nargs)`, kept out of
/// the control-frame range.
pub const fn site_id(
    level: Level,
    module: &str,
    file: &str,
    line: u32,
    column: u32,
    fmt: &str,
    nargs: u8,
) -> u32 {
    let mut h = fnv1a32(module.as_bytes());
    h = fnv1a_extend(h, b"|");
    h = fnv1a_extend(h, file.as_bytes());
    h = fnv1a_extend(h, b"|");
    h = fnv1a_extend(h, &line.to_le_bytes());
    h = fnv1a_extend(h, &column.to_le_bytes());
    h = fnv1a_extend(h, b"|");
    h = fnv1a_extend(h, fmt.as_bytes());
    h = fnv1a_extend(h, &[level as u8, nargs]);
    if h >= CONTROL_BASE {
        h ^= 0x8000_0000;
    }
    h
}

#[linkme::distributed_slice]
pub static SITES: [Site];

/// Every site linked into this binary.
pub fn sites() -> &'static [Site] {
    &SITES
}

/// Order-independent hash of a set of site IDs; written in the stream header
/// and recomputed by the decoder from the contract file.
pub fn contract_hash_of(ids: impl IntoIterator<Item = u32>) -> u32 {
    ids.into_iter()
        .fold(0u32, |acc, id| acc.wrapping_add(fnv1a32(&id.to_le_bytes())))
}

/// Contract hash of this binary.
pub fn contract_hash() -> u32 {
    contract_hash_of(SITES.iter().map(|s| s.id))
}

/// Two distinct sites sharing an ID, if any.
pub fn find_collision() -> Option<(&'static Site, &'static Site)> {
    let s = sites();
    for i in 0..s.len() {
        for j in i + 1..s.len() {
            if s[i].id == s[j].id {
                return Some((&s[i], &s[j]));
            }
        }
    }
    None
}

/// Number of `{…}` placeholders in a format string (`{{` and `}}` are escapes).
pub const fn count_placeholders(fmt: &str) -> usize {
    let b = fmt.as_bytes();
    let mut i = 0;
    let mut n = 0;
    while i < b.len() {
        if b[i] == b'{' {
            if i + 1 < b.len() && b[i + 1] == b'{' {
                i += 2;
                continue;
            }
            n += 1;
            while i < b.len() && b[i] != b'}' {
                i += 1;
            }
        } else if b[i] == b'}' && i + 1 < b.len() && b[i + 1] == b'}' {
            i += 1;
        }
        i += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders() {
        assert_eq!(count_placeholders(""), 0);
        assert_eq!(count_placeholders("x={} y={:?}"), 2);
        assert_eq!(count_placeholders("{{literal}} {}"), 1);
        assert_eq!(count_placeholders("{name} {:x} {:>8}"), 3);
    }

    #[test]
    fn id_depends_on_every_field() {
        let base = site_id(Level::Info, "m", "f.rs", 1, 1, "x={}", 1);
        assert_ne!(base, site_id(Level::Warn, "m", "f.rs", 1, 1, "x={}", 1));
        assert_ne!(base, site_id(Level::Info, "n", "f.rs", 1, 1, "x={}", 1));
        assert_ne!(base, site_id(Level::Info, "m", "g.rs", 1, 1, "x={}", 1));
        assert_ne!(base, site_id(Level::Info, "m", "f.rs", 2, 1, "x={}", 1));
        assert_ne!(base, site_id(Level::Info, "m", "f.rs", 1, 2, "x={}", 1));
        assert_ne!(base, site_id(Level::Info, "m", "f.rs", 1, 1, "y={}", 1));
        assert!(base < CONTROL_BASE);
    }

    #[test]
    fn contract_hash_is_order_independent() {
        assert_eq!(contract_hash_of([1, 2, 3]), contract_hash_of([3, 1, 2]));
        assert_ne!(contract_hash_of([1, 2, 3]), contract_hash_of([1, 2, 4]));
    }
}
