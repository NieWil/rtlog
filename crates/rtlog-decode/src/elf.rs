//! Extract a contract from an ELF image's site descriptors (spec section 4).
//!
//! Two layouts are understood:
//!
//! - Rust (`linkme_SITES`): `rtlog::Site`, `repr(C)`: `id: u32`, `kind`,
//!   `level`, `nargs`, one reserved byte, `line: u32`, `column: u32`, then
//!   `module`, `file`, `fmt` as (pointer, length) pairs. The ID is stored.
//! - C / C++ (`rtlog_sites`, from `rtlog.h`): `magic` "RTLS", `line`, `level`,
//!   `nargs`, `kind`, reserved bytes, then `fmt`, `file`, `func` as
//!   NUL-terminated string pointers. The ID is the entry's index + 1.
//!
//! Position-independent executables leave pointers in data sections to be
//! filled in at load time by `RELATIVE` dynamic relocations; those are applied
//! here, so PIE binaries, shared objects and static firmware all work.

use std::collections::HashMap;
use std::fmt;

use object::{Object, ObjectSection, RelocationFlags, RelocationTarget};

/// Why a contract could not be extracted.
#[derive(Debug, PartialEq, Eq)]
pub enum ElfError {
    Parse(String),
    NoSites,
    BadEntry { index: usize, reason: String },
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ElfError::Parse(e) => write!(f, "not a readable ELF file: {e}"),
            ElfError::NoSites => write!(
                f,
                "no rtlog site section (linkme_SITES or rtlog_sites); was the binary stripped of it, or built without rtlog?"
            ),
            ElfError::BadEntry { index, reason } => write!(f, "site descriptor {index}: {reason}"),
        }
    }
}

impl std::error::Error for ElfError {}

struct Image<'d> {
    file: object::File<'d>,
    ptr: usize,
    /// Address → addend of RELATIVE dynamic relocations.
    relative: HashMap<u64, u64>,
}

fn is_relative(arch: object::Architecture, r_type: u32) -> bool {
    use object::Architecture::*;
    matches!(
        (arch, r_type),
        (X86_64, 8) | (I386, 8) | (Aarch64, 1027) | (Arm, 23) | (Riscv64, 3) | (Riscv32, 3)
    )
}

impl<'d> Image<'d> {
    fn parse(data: &'d [u8]) -> Result<Self, ElfError> {
        let file = object::File::parse(data).map_err(|e| ElfError::Parse(e.to_string()))?;
        if file.format() != object::BinaryFormat::Elf {
            return Err(ElfError::Parse("only ELF images are supported".into()));
        }
        if !file.is_little_endian() {
            return Err(ElfError::Parse(
                "big-endian images are not supported".into(),
            ));
        }
        let ptr = if file.is_64() { 8 } else { 4 };
        let mut relative = HashMap::new();
        if let Some(relocs) = file.dynamic_relocations() {
            for (addr, r) in relocs {
                if let RelocationFlags::Elf { r_type } = r.flags() {
                    if is_relative(file.architecture(), r_type)
                        && matches!(
                            r.target(),
                            RelocationTarget::Absolute | RelocationTarget::Symbol(_)
                        )
                        && !r.has_implicit_addend()
                    {
                        relative.insert(addr, r.addend() as u64);
                    }
                }
            }
        }
        Ok(Image {
            file,
            ptr,
            relative,
        })
    }

    fn word(&self, bytes: &[u8], at: usize) -> Option<u64> {
        let b = bytes.get(at..at + self.ptr)?;
        Some(if self.ptr == 8 {
            u64::from_le_bytes(b.try_into().ok()?)
        } else {
            u64::from(u32::from_le_bytes(b.try_into().ok()?))
        })
    }

    /// A pointer-sized field at virtual address `addr`, with its relocation applied.
    fn pointer(&self, bytes: &[u8], at: usize, addr: u64) -> Option<u64> {
        let raw = self.word(bytes, at)?;
        Some(match self.relative.get(&addr) {
            Some(&addend) if raw == 0 => addend,
            _ => raw,
        })
    }

    /// `len` bytes at virtual address `addr`, from whichever section holds them.
    fn bytes_at(&self, addr: u64, len: Option<usize>) -> Option<&'d [u8]> {
        for s in self.file.sections() {
            let (start, size) = (s.address(), s.size());
            if addr < start || addr >= start + size {
                continue;
            }
            let data = s.data().ok()?;
            let off = usize::try_from(addr - start).ok()?;
            let rest = data.get(off..)?;
            return match len {
                Some(n) => rest.get(..n),
                None => rest.iter().position(|&b| b == 0).map(|n| &rest[..n]),
            };
        }
        None
    }

    fn string(
        &self,
        addr: u64,
        len: Option<usize>,
        what: &str,
        index: usize,
    ) -> Result<String, ElfError> {
        let b = self.bytes_at(addr, len).ok_or_else(|| ElfError::BadEntry {
            index,
            reason: format!("{what} pointer {addr:#x} is outside the image"),
        })?;
        String::from_utf8(b.to_vec()).map_err(|_| ElfError::BadEntry {
            index,
            reason: format!("{what} is not UTF-8"),
        })
    }
}

fn level_name(v: u8) -> Option<&'static str> {
    ["trace", "debug", "info", "warn", "error"]
        .get(usize::from(v))
        .copied()
}

/// One contract line, in the same key order as the runtime's contract writer.
#[allow(clippy::too_many_arguments)]
fn json_line(
    id: u32,
    level: &str,
    module: &str,
    file: &str,
    line: u32,
    column: u32,
    fmt: &str,
    nargs: u8,
    span: bool,
) -> String {
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    format!(
        "{{\"id\":{id},\"level\":\"{level}\",\"module\":{},\"file\":{},\"line\":{line},\"column\":{column},\"fmt\":{},\"nargs\":{nargs}{}}}",
        q(module),
        q(file),
        q(fmt),
        if span { ",\"kind\":\"span\"" } else { "" }
    )
}

/// Read every site descriptor in an ELF image and return the contract as
/// JSON lines (sorted by ID), ready for [`crate::Contract::from_jsonl`].
pub fn contract_from_elf(data: &[u8]) -> Result<String, ElfError> {
    let img = Image::parse(data)?;
    let mut lines: Vec<(u32, String)> = Vec::new();
    let mut found = false;

    if let Some(sec) = img.file.section_by_name("linkme_SITES") {
        found = true;
        let bytes = sec.data().map_err(|e| ElfError::Parse(e.to_string()))?;
        let (w, base) = (img.ptr, sec.address());
        let size = 16 + 6 * w;
        for (i, e) in bytes.chunks_exact(size).enumerate() {
            let at = |o: usize| base + (i * size + o) as u64;
            let u32_at = |o: usize| u32::from_le_bytes(e[o..o + 4].try_into().unwrap());
            let bad = |r: &str| ElfError::BadEntry {
                index: i,
                reason: r.to_owned(),
            };
            let level = level_name(e[5]).ok_or_else(|| bad("bad level"))?;
            let mut strs = Vec::new();
            for (k, what) in ["module", "file", "fmt"].iter().enumerate() {
                let off = 16 + 2 * k * w;
                let p = img
                    .pointer(e, off, at(off))
                    .ok_or_else(|| bad("truncated"))?;
                let n = img.word(e, off + w).ok_or_else(|| bad("truncated"))? as usize;
                strs.push(img.string(p, Some(n), what, i)?);
            }
            lines.push((
                u32_at(0),
                json_line(
                    u32_at(0),
                    level,
                    &strs[0],
                    &strs[1],
                    u32_at(8),
                    u32_at(12),
                    &strs[2],
                    e[6],
                    e[4] == 1,
                ),
            ));
        }
    }

    if let Some(sec) = img.file.section_by_name("rtlog_sites") {
        found = true;
        let bytes = sec.data().map_err(|e| ElfError::Parse(e.to_string()))?;
        let (w, base) = (img.ptr, sec.address());
        let size = 16 + 3 * w;
        for (i, e) in bytes.chunks_exact(size).enumerate() {
            let at = |o: usize| base + (i * size + o) as u64;
            let bad = |r: &str| ElfError::BadEntry {
                index: i,
                reason: r.to_owned(),
            };
            if e[0..4] != *b"RTLS" {
                return Err(bad("bad magic"));
            }
            let line = u32::from_le_bytes(e[4..8].try_into().unwrap());
            let level = level_name(e[8]).ok_or_else(|| bad("bad level"))?;
            let mut strs = Vec::new();
            for (k, what) in ["fmt", "file", "func"].iter().enumerate() {
                let off = 16 + k * w;
                let p = img
                    .pointer(e, off, at(off))
                    .ok_or_else(|| bad("truncated"))?;
                strs.push(img.string(p, None, what, i)?);
            }
            let id = i as u32 + 1;
            lines.push((
                id,
                json_line(
                    id,
                    level,
                    &strs[2],
                    &strs[1],
                    line,
                    0,
                    &strs[0],
                    e[9],
                    e[10] == 1,
                ),
            ));
        }
    }

    if !found {
        return Err(ElfError::NoSites);
    }
    lines.sort_by_key(|(id, _)| *id);
    let mut out = String::new();
    for (_, l) in lines {
        out.push_str(&l);
        out.push('\n');
    }
    Ok(out)
}
