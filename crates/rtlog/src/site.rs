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
///
/// `repr(C)` with a fixed field order, so host tools can read descriptors
/// straight out of a firmware image (`rtlog contract --elf`): `id: u32`,
/// `kind: u8`, `level: u8`, `nargs: u8`, one reserved byte, `line: u32`,
/// `column: u32`, then `module`, `file` and `fmt` as (pointer, length) pairs.
#[derive(Debug)]
#[repr(C)]
pub struct Site {
    /// Site ID written in every frame from this site; see [`site_id`].
    pub id: u32,
    /// Log call or span.
    pub kind: SiteKind,
    /// Level of a log call; always [`Level::Info`] for a span.
    pub level: Level,
    /// Number of arguments; always 0 for a span.
    pub nargs: u8,
    _reserved: u8,
    /// Source line of the macro call, from `line!()`.
    pub line: u32,
    /// Source column of the macro call, from `column!()`.
    pub column: u32,
    /// Module path of the macro call, from `module_path!()`.
    pub module: &'static str,
    /// Source file of the macro call, from `file!()`.
    pub file: &'static str,
    /// Format string of a log call, or the name of a span.
    pub fmt: &'static str,
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
            _reserved: 0,
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
            _reserved: 0,
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

/// Link-time collection of every [`Site`] the macros emit; read it with [`sites`].
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

/// Whether `fmt` uses an inline capture such as `{x}` or `{x:?}`: a `{` whose
/// first character is an identifier start. `{}`, `{:?}`, `{:x}` and `{{` are not.
pub const fn has_inline_capture(fmt: &str) -> bool {
    let b = fmt.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'{' {
            let c = b[i + 1];
            if c == b'{' {
                i += 2;
                continue;
            }
            if c == b'_' || c.is_ascii_alphabetic() {
                return true;
            }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_captures_are_detected() {
        assert!(has_inline_capture("x={x}"));
        assert!(has_inline_capture("{_v:?}"));
        assert!(!has_inline_capture("x={} {:?} {:>8} {{x}}"));
        assert!(!has_inline_capture("{"));
    }

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
    fn descriptor_layout_is_what_host_tools_read() {
        let w = core::mem::size_of::<usize>();
        assert_eq!(core::mem::size_of::<Site>(), 16 + 6 * w);
        assert_eq!(core::mem::align_of::<Site>(), w.max(4));
        let s = Site::new(Level::Warn, "m", "f.rs", 7, 9, "x={}", 1);
        let base = &s as *const Site as usize;
        assert_eq!(&s.kind as *const _ as usize - base, 4);
        assert_eq!(&s.line as *const _ as usize - base, 8);
        assert_eq!(&s.module as *const _ as usize - base, 16);
        assert_eq!(&s.fmt as *const _ as usize - base, 16 + 4 * w);
        // A `&str` is (pointer, length) in that order.
        let words: [usize; 2] = unsafe { core::mem::transmute::<&str, [usize; 2]>(s.fmt) };
        assert_eq!((words[0], words[1]), (s.fmt.as_ptr() as usize, 4));
    }

    /// `site_id` before the control-range fold.
    fn raw_id(level: Level, module: &str, file: &str, line: u32, fmt: &str, nargs: u8) -> u32 {
        let mut h = fnv1a32(module.as_bytes());
        h = fnv1a_extend(h, b"|");
        h = fnv1a_extend(h, file.as_bytes());
        h = fnv1a_extend(h, b"|");
        h = fnv1a_extend(h, &line.to_le_bytes());
        h = fnv1a_extend(h, &1u32.to_le_bytes());
        h = fnv1a_extend(h, b"|");
        h = fnv1a_extend(h, fmt.as_bytes());
        fnv1a_extend(h, &[level as u8, nargs])
    }

    #[test]
    fn a_hash_in_the_control_range_is_moved_out_of_it() {
        // Found by search: this site's raw FNV hash is >= CONTROL_BASE.
        const LINE: u32 = 6_621_171;
        let raw = raw_id(Level::Info, "m", "f.rs", LINE, "x", 0);
        assert!(raw >= CONTROL_BASE, "{raw:#x}");
        let id = site_id(Level::Info, "m", "f.rs", LINE, 1, "x", 0);
        assert_eq!(id, raw ^ 0x8000_0000);
        assert!(id < CONTROL_BASE);
    }

    #[test]
    fn span_sites_describe_a_span() {
        let s = Site::span("m", "f.rs", 12, 1, "parse");
        assert_eq!(s.kind, SiteKind::Span);
        assert_eq!((s.level, s.nargs, s.fmt), (Level::Info, 0, "parse"));
        assert_eq!((s.module, s.file, s.line, s.column), ("m", "f.rs", 12, 1));
        assert_eq!(
            s.id,
            site_id(Level::Info, "m", "f.rs", 12, 1, "parse", 0) ^ 0x5a5a_0000
        );
        // A span never shares the ID of a log site with the same text.
        assert_ne!(
            s.id,
            Site::new(Level::Info, "m", "f.rs", 12, 1, "parse", 0).id
        );
    }

    #[test]
    fn a_span_id_in_the_control_range_is_moved_out_of_it() {
        // Found by search: this span's salted ID lands in the control range.
        const LINE: u32 = 1_526_560;
        let salted = site_id(Level::Info, "m", "f.rs", LINE, 1, "span", 0) ^ 0x5a5a_0000;
        assert!(salted >= CONTROL_BASE, "{salted:#x}");
        let s = Site::span("m", "f.rs", LINE, 1, "span");
        assert_eq!(s.id, salted ^ 0x8000_0000);
        assert!(s.id < CONTROL_BASE);
    }

    #[test]
    fn contract_hash_is_order_independent() {
        assert_eq!(contract_hash_of([1, 2, 3]), contract_hash_of([3, 1, 2]));
        assert_ne!(contract_hash_of([1, 2, 3]), contract_hash_of([1, 2, 4]));
    }
}
