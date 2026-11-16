//! Extract a contract from a binary's site descriptors (spec section 4):
//! ELF (Linux, the BSDs, firmware), Mach-O (macOS) and PE (Windows).
//!
//! Two layouts are understood:
//!
//! - Rust: `rtlog::Site`, `repr(C)`: `id: u32`, `kind`, `level`, `nargs`, one
//!   reserved byte, `line: u32`, `column: u32`, then `module`, `file`, `fmt`
//!   as (pointer, length) pairs. The ID is stored.
//! - C / C++ (from `rtlog.h`): `magic` "RTLS", `line`, `level`, `nargs`,
//!   `kind`, reserved bytes, then `fmt`, `file`, `func` as NUL-terminated
//!   string pointers. The ID is the entry's index + 1.
//!
//! Where the descriptors are:
//!
//! | Format | Rust (`linkme`) | C / C++ |
//! | --- | --- | --- |
//! | ELF | `linkme_SITES` | `rtlog_sites` |
//! | Mach-O | `__DATA,__linkme<hash>` | `__DATA,rtlog_sites` |
//! | PE | `.linkme_SITES` (`.linkme_` once the linker cuts the name to 8 bytes) | `rtlog` |
//!
//! On ELF the Rust section holds rtlog's descriptors and nothing else, so
//! every entry must decode. On Mach-O the section name is a hash, and on PE
//! the linker merges every `linkme` slice of the program into one section, so
//! there the Rust sections are scanned: an entry counts when its strings
//! resolve and its stored ID equals the content hash of its other fields,
//! which data of another slice does not do by accident.
//!
//! Position-independent executables leave pointers in data sections to be
//! filled in at load time: `RELATIVE` dynamic relocations on ELF, rebases or
//! chained fixups on Mach-O. Those are applied here, so PIE binaries, shared
//! objects and static firmware all work. PE images store absolute addresses.

use std::collections::HashMap;
use std::fmt;

use object::{BinaryFormat, Object, ObjectSection, RelocationFlags, RelocationTarget};

/// Why a contract could not be extracted.
#[derive(Debug, PartialEq, Eq)]
pub enum ElfError {
    /// The file is not a readable little-endian ELF, Mach-O or PE image, or
    /// a site section could not be read; holds the reason.
    Parse(String),
    /// The image has no site section, or (Mach-O, PE) none with a descriptor in it.
    NoSites,
    /// A site descriptor could not be decoded.
    BadEntry {
        /// 0-based index of the descriptor in its section.
        index: usize,
        /// What was wrong with it, such as `bad level` or `truncated`.
        reason: String,
    },
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ElfError::Parse(e) => write!(f, "not a readable ELF, Mach-O or PE file: {e}"),
            ElfError::NoSites => write!(
                f,
                "no rtlog site descriptors (sections linkme_SITES or rtlog_sites on ELF, \
                 __linkme* or rtlog_sites on Mach-O, .linkme_* or rtlog on PE); \
                 was the binary stripped of them, or built without rtlog?"
            ),
            ElfError::BadEntry { index, reason } => write!(f, "site descriptor {index}: {reason}"),
        }
    }
}

impl std::error::Error for ElfError {}

struct Image<'d> {
    file: object::File<'d>,
    format: BinaryFormat,
    ptr: usize,
    /// Address → addend of RELATIVE dynamic relocations (ELF).
    relative: HashMap<u64, u64>,
    /// Address the image is linked at (Mach-O: its `__TEXT` segment).
    base: u64,
}

/// Low 36 bits of a Mach-O chained-fixup rebase: the target, as an address
/// (`DYLD_CHAINED_PTR_64`) or as an offset from the image base
/// (`DYLD_CHAINED_PTR_64_OFFSET`). The bits above chain to the next fixup.
const MACHO_TARGET: u64 = (1 << 36) - 1;

/// The thin Mach-O image inside a universal ("fat") binary: its first
/// architecture. Any other input is returned as it is.
fn thin(data: &[u8]) -> &[u8] {
    use object::read::macho::{FatArch, MachOFatFile32, MachOFatFile64};
    let first = match object::FileKind::parse(data) {
        Ok(object::FileKind::MachOFat32) => MachOFatFile32::parse(data)
            .ok()
            .and_then(|f| f.arches().first().and_then(|a| a.data(data).ok())),
        Ok(object::FileKind::MachOFat64) => MachOFatFile64::parse(data)
            .ok()
            .and_then(|f| f.arches().first().and_then(|a| a.data(data).ok())),
        _ => None,
    };
    first.unwrap_or(data)
}

fn is_relative(arch: object::Architecture, r_type: u32) -> bool {
    use object::Architecture::*;
    matches!(
        (arch, r_type),
        (X86_64, 8) | (I386, 8) | (Aarch64, 1027) | (Arm, 23) | (Riscv64, 3) | (Riscv32, 3)
    )
}

impl<'d> Image<'d> {
    fn parse(data: &'d [u8], elf_only: bool) -> Result<Self, ElfError> {
        let file = object::File::parse(thin(data)).map_err(|e| ElfError::Parse(e.to_string()))?;
        let format = file.format();
        let known = match format {
            BinaryFormat::Elf => true,
            BinaryFormat::MachO | BinaryFormat::Pe => !elf_only,
            _ => false,
        };
        if !known {
            return Err(ElfError::Parse(if elf_only {
                "only ELF images are supported".into()
            } else {
                "only ELF, Mach-O and PE images are supported".into()
            }));
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
        let base = match format {
            BinaryFormat::MachO => {
                use object::ObjectSegment;
                file.segments()
                    .find(|s| s.name().ok().flatten() == Some("__TEXT"))
                    .map_or(0, |s| s.address())
            }
            _ => 0,
        };
        Ok(Image {
            file,
            format,
            ptr,
            relative,
            base,
        })
    }

    /// Whether `addr` lies in a section of the image.
    fn mapped(&self, addr: u64) -> bool {
        self.file
            .sections()
            .any(|s| addr >= s.address() && addr - s.address() < s.size())
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
        if self.format == BinaryFormat::MachO && self.ptr == 8 && !self.mapped(raw) {
            // A chained fixup; a plain rebase (older linkers) is mapped as it is.
            let target = raw & MACHO_TARGET;
            return Some(if self.mapped(target) {
                target
            } else {
                self.base.wrapping_add(target)
            });
        }
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

/// A site section must hold whole descriptors. A remainder means the binary
/// lays descriptors out differently (another rtlog version) or the section is
/// damaged; reading on would silently drop sites from the contract.
fn whole_descriptors(bytes: &[u8], size: usize, section: &str) -> Result<(), ElfError> {
    if bytes.len().is_multiple_of(size) {
        return Ok(());
    }
    Err(ElfError::BadEntry {
        index: bytes.len() / size,
        reason: format!(
            "{section} is {} bytes, not a whole number of {size}-byte descriptors \
             (built with a different rtlog version?)",
            bytes.len()
        ),
    })
}

/// Size of a Rust descriptor for `w`-byte pointers.
const fn rust_size(w: usize) -> usize {
    16 + 6 * w
}

/// Decode the Rust descriptor `e`, entry `i` of a section, loaded at `addr`.
fn rust_site(img: &Image<'_>, e: &[u8], i: usize, addr: u64) -> Result<(u32, String), ElfError> {
    let w = img.ptr;
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
            .pointer(e, off, addr + off as u64)
            .ok_or_else(|| bad("truncated"))?;
        let n = img.word(e, off + w).ok_or_else(|| bad("truncated"))? as usize;
        strs.push(img.string(p, Some(n), what, i)?);
    }
    let line = json_line(
        u32_at(0),
        level,
        &strs[0],
        &strs[1],
        u32_at(8),
        u32_at(12),
        &strs[2],
        e[6],
        e[4] == 1,
    );
    Ok((u32_at(0), line))
}

/// Whether the Rust descriptor `e` is one of rtlog's: its strings resolve and
/// its ID is the content hash of its other fields.
fn rust_site_checks(img: &Image<'_>, e: &[u8], addr: u64) -> bool {
    use rtlog::Level;
    let w = img.ptr;
    let level = match e[5] {
        0 => Level::Trace,
        1 => Level::Debug,
        2 => Level::Info,
        3 => Level::Warn,
        4 => Level::Error,
        _ => return false,
    };
    if e[4] > 1 || e[7] != 0 {
        return false;
    }
    let u32_at = |o: usize| u32::from_le_bytes(e[o..o + 4].try_into().unwrap());
    let mut strs = Vec::new();
    for k in 0..3 {
        let off = 16 + 2 * k * w;
        let (Some(p), Some(n)) = (img.pointer(e, off, addr + off as u64), img.word(e, off + w))
        else {
            return false;
        };
        let Ok(s) = img.string(p, usize::try_from(n).ok(), "", 0) else {
            return false;
        };
        strs.push(s);
    }
    let (line, column) = (u32_at(8), u32_at(12));
    let id = if e[4] == 1 {
        rtlog::site::span_id(&strs[0], &strs[1], line, column, &strs[2])
    } else {
        rtlog::site::site_id(level, &strs[0], &strs[1], line, column, &strs[2], e[6])
    };
    id == u32_at(0)
}

/// Read every site descriptor in an ELF image and return the contract as
/// JSON lines (sorted by ID), ready for [`crate::Contract::from_jsonl`].
///
/// Mach-O and PE files are rejected; [`contract_from_binary`] reads all three.
pub fn contract_from_elf(data: &[u8]) -> Result<String, ElfError> {
    contract_of(Image::parse(data, true)?)
}

/// Read every site descriptor in an ELF, Mach-O or PE image (an executable,
/// a shared library, firmware) and return the contract as JSON lines (sorted
/// by ID), ready for [`crate::Contract::from_jsonl`].
///
/// Of a universal Mach-O binary, the first architecture is read.
pub fn contract_from_binary(data: &[u8]) -> Result<String, ElfError> {
    contract_of(Image::parse(data, false)?)
}

fn contract_of(img: Image<'_>) -> Result<String, ElfError> {
    let mut lines: Vec<(u32, String)> = Vec::new();
    let mut found = false;
    let w = img.ptr;

    for sec in img.file.sections() {
        let name = sec.name().unwrap_or("");
        // Rust: exact on ELF; scanned on Mach-O and PE (see the module docs).
        let (rust, scan) = match img.format {
            BinaryFormat::Elf => (name == "linkme_SITES", false),
            BinaryFormat::MachO => (name.starts_with("__linkme"), true),
            _ => (name.starts_with(".linkme_"), true),
        };
        // C: the first and last entries of a PE section are all-zero markers.
        let c = match img.format {
            BinaryFormat::Pe => name == "rtlog",
            _ => name == "rtlog_sites",
        };
        if !rust && !c {
            continue;
        }
        let bytes = sec.data().map_err(|e| ElfError::Parse(e.to_string()))?;
        let base = sec.address();

        if rust && !scan {
            found = true;
            let size = rust_size(w);
            whole_descriptors(bytes, size, name)?;
            for (i, e) in bytes.chunks_exact(size).enumerate() {
                lines.push(rust_site(&img, e, i, base + (i * size) as u64)?);
            }
        } else if rust {
            let size = rust_size(w);
            let (mut at, mut i) = (0, 0);
            while let Some(e) = bytes.get(at..at + size) {
                let addr = base + at as u64;
                if rust_site_checks(&img, e, addr) {
                    found = true;
                    lines.push(rust_site(&img, e, i, addr)?);
                    i += 1;
                    at += size;
                } else {
                    at += w;
                }
            }
        } else {
            let pe = img.format == BinaryFormat::Pe;
            let size = 16 + 3 * w;
            whole_descriptors(bytes, size, name)?;
            for (i, e) in bytes.chunks_exact(size).enumerate() {
                if pe && e.iter().all(|&b| b == 0) {
                    continue; // start or end marker, or the linker's padding
                }
                found = true;
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
                // The index counts from the start marker on PE, which is entry 0.
                let id = i as u32 + u32::from(!pe);
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

#[cfg(test)]
mod tests {
    use super::*;

    const SHT_PROGBITS: u32 = 1;
    /// Section type 4: relocations with explicit addends.
    const SHT_RELOCS_ADDEND: u32 = 4;
    const EM_ARM: u16 = 40;
    const EM_X86_64: u16 = 62;
    const R_X86_64_64: u64 = 1;
    const R_X86_64_RELATIVE: u64 = 8;

    /// One section of a synthetic image.
    struct Sec {
        name: &'static str,
        kind: u32,
        addr: u64,
        data: Vec<u8>,
        /// Size written in the section header; the data length unless changed.
        size: u64,
    }

    fn sec(name: &'static str, addr: u64, data: Vec<u8>) -> Sec {
        let size = data.len() as u64;
        Sec {
            name,
            kind: SHT_PROGBITS,
            addr,
            data,
            size,
        }
    }

    /// A minimal ELF image: header, section contents, `.shstrtab`, section headers.
    fn image(is64: bool, little: bool, machine: u16, secs: &[Sec]) -> Vec<u8> {
        let put = |out: &mut Vec<u8>, v: u64, n: usize| {
            let b = v.to_le_bytes();
            if little {
                out.extend_from_slice(&b[..n]);
            } else {
                out.extend(b[..n].iter().rev());
            }
        };
        let pad = |v: &mut Vec<u8>| v.resize(v.len().next_multiple_of(8), 0);
        let w = if is64 { 8 } else { 4 };
        let (ehsize, shentsize) = if is64 { (64, 64) } else { (52, 40) };

        let mut shstr = vec![0u8];
        let mut names = Vec::new();
        for n in secs.iter().map(|s| s.name).chain([".shstrtab"]) {
            names.push(shstr.len() as u64);
            shstr.extend_from_slice(n.as_bytes());
            shstr.push(0);
        }
        let mut out = vec![0u8; ehsize];
        pad(&mut out);
        let mut offsets = Vec::new();
        for d in secs.iter().map(|s| &s.data[..]).chain([&shstr[..]]) {
            offsets.push(out.len() as u64);
            out.extend_from_slice(d);
            pad(&mut out);
        }
        let shoff = out.len() as u64;

        let class = if is64 { 2 } else { 1 };
        let data = if little { 1 } else { 2 };
        let mut h = vec![0x7f, b'E', b'L', b'F', class, data, 1];
        h.resize(16, 0);
        put(&mut h, 3, 2); // ET_DYN
        put(&mut h, u64::from(machine), 2);
        put(&mut h, 1, 4); // EV_CURRENT
        put(&mut h, 0, w); // entry
        put(&mut h, 0, w); // phoff
        put(&mut h, shoff, w);
        put(&mut h, 0, 4); // flags
        put(&mut h, ehsize as u64, 2);
        put(&mut h, 0, 2); // phentsize
        put(&mut h, 0, 2); // phnum
        put(&mut h, shentsize as u64, 2);
        put(&mut h, secs.len() as u64 + 2, 2);
        put(&mut h, secs.len() as u64 + 1, 2);
        out[..ehsize].copy_from_slice(&h);

        out.resize(out.len() + shentsize, 0); // null section header
        for i in 0..=secs.len() {
            let (kind, flags, addr, size, entsize) = match secs.get(i) {
                Some(s) if s.kind == SHT_RELOCS_ADDEND => (s.kind, 2, s.addr, s.size, 24),
                Some(s) => (s.kind, 2, s.addr, s.size, 0), // SHF_ALLOC
                None => (3, 0, 0, shstr.len() as u64, 0),  // SHT_STRTAB
            };
            put(&mut out, names[i], 4);
            put(&mut out, u64::from(kind), 4);
            put(&mut out, flags, w);
            put(&mut out, addr, w);
            put(&mut out, offsets[i], w);
            put(&mut out, size, w);
            put(&mut out, 0, 4); // link: no .dynsym, so relocation sections count as dynamic
            put(&mut out, 0, 4); // info
            put(&mut out, 1, w); // addralign
            put(&mut out, entsize, w);
        }
        out
    }

    const RODATA: u64 = 0x1000;
    const SITES: u64 = 0x2000;
    /// `.rodata`: "app", "src/main.rs" and "x={}", each NUL-terminated so the
    /// C layout can use the same strings.
    const STRS: &[u8] = b"app\0src/main.rs\0x={}\0";
    const APP: (u64, u64) = (RODATA, 3);
    const MAIN_RS: (u64, u64) = (RODATA + 4, 11);
    const FMT: (u64, u64) = (RODATA + 16, 4);

    /// A Rust `linkme_SITES` descriptor, ID 0x1234, for a `w`-byte pointer target.
    fn rust_site(w: usize, level: u8, strs: [(u64, u64); 3]) -> Vec<u8> {
        let mut e = 0x1234u32.to_le_bytes().to_vec();
        e.extend([0, level, 1, 0]); // kind log, level, nargs, reserved
        e.extend(7u32.to_le_bytes());
        e.extend(5u32.to_le_bytes());
        for (p, n) in strs {
            e.extend(&p.to_le_bytes()[..w]);
            e.extend(&n.to_le_bytes()[..w]);
        }
        e
    }

    /// A C `rtlog_sites` descriptor (`rtlog.h`) for a 64-bit target, line 42.
    fn c_site(magic: &[u8; 4], level: u8, kind: u8, ptrs: [u64; 3]) -> Vec<u8> {
        let mut e = magic.to_vec();
        e.extend(42u32.to_le_bytes());
        e.extend([level, 1, kind, 0, 0, 0, 0, 0]);
        for p in ptrs {
            e.extend(p.to_le_bytes());
        }
        e
    }

    const RUST_LINE: &str = r#"{"id":4660,"level":"info","module":"app","file":"src/main.rs","line":7,"column":5,"fmt":"x={}","nargs":1}"#;

    fn rust_image(is64: bool, machine: u16, sites: Vec<u8>) -> Vec<u8> {
        image(
            is64,
            true,
            machine,
            &[
                sec(".rodata", RODATA, STRS.to_vec()),
                sec("linkme_SITES", SITES, sites),
            ],
        )
    }

    fn c_image(sites: Vec<u8>) -> Vec<u8> {
        image(
            true,
            true,
            EM_X86_64,
            &[
                sec(".rodata", RODATA, STRS.to_vec()),
                sec("rtlog_sites", SITES, sites),
            ],
        )
    }

    #[test]
    fn errors_display() {
        assert_eq!(
            ElfError::Parse("short".into()).to_string(),
            "not a readable ELF, Mach-O or PE file: short"
        );
        assert!(ElfError::NoSites
            .to_string()
            .starts_with("no rtlog site descriptors (sections linkme_SITES or rtlog_sites on ELF"));
        assert_eq!(
            ElfError::BadEntry {
                index: 3,
                reason: "bad level".into()
            }
            .to_string(),
            "site descriptor 3: bad level"
        );
    }

    #[test]
    fn non_elf_data_is_a_parse_error() {
        let e = contract_from_elf(b"\x7fELG not an image, just text").unwrap_err();
        assert!(matches!(e, ElfError::Parse(_)), "{e:?}");
        assert!(
            e.to_string()
                .starts_with("not a readable ELF, Mach-O or PE file: "),
            "{e}"
        );
    }

    #[test]
    fn truncated_header_is_a_parse_error() {
        let full = rust_image(true, EM_X86_64, rust_site(8, 2, [APP, MAIN_RS, FMT]));
        assert!(matches!(
            contract_from_elf(&full[..40]),
            Err(ElfError::Parse(_))
        ));
    }

    #[test]
    fn big_endian_image_is_rejected() {
        let data = image(
            false,
            false,
            EM_ARM,
            &[sec(".rodata", RODATA, STRS.to_vec())],
        );
        assert_eq!(
            contract_from_elf(&data),
            Err(ElfError::Parse(
                "big-endian images are not supported".into()
            ))
        );
    }

    #[test]
    fn image_without_site_sections_has_no_sites() {
        let data = image(
            true,
            true,
            EM_X86_64,
            &[sec(".rodata", RODATA, STRS.to_vec())],
        );
        assert_eq!(contract_from_elf(&data), Err(ElfError::NoSites));
    }

    #[test]
    fn rust_descriptor_64_bit_becomes_a_contract_line() {
        let data = rust_image(true, EM_X86_64, rust_site(8, 2, [APP, MAIN_RS, FMT]));
        let jsonl = contract_from_elf(&data).unwrap();
        assert_eq!(jsonl, format!("{RUST_LINE}\n"));
        let c = crate::Contract::from_jsonl(&jsonl).unwrap();
        assert_eq!(c.site(0x1234).unwrap().fmt, "x={}");
    }

    #[test]
    fn rust_descriptor_32_bit_reads_4_byte_pointers() {
        let mut sites = rust_site(4, 2, [APP, MAIN_RS, FMT]);
        sites[4] = 1; // a span site
        let data = rust_image(false, EM_ARM, sites);
        let want = RUST_LINE.replace("\"nargs\":1}", "\"nargs\":1,\"kind\":\"span\"}");
        assert_eq!(contract_from_elf(&data).unwrap(), format!("{want}\n"));
    }

    #[test]
    fn rust_descriptor_with_bad_level_is_rejected() {
        let data = rust_image(true, EM_X86_64, rust_site(8, 5, [APP, MAIN_RS, FMT]));
        assert_eq!(
            contract_from_elf(&data),
            Err(ElfError::BadEntry {
                index: 0,
                reason: "bad level".into()
            })
        );
    }

    #[test]
    fn pointer_outside_the_image_is_rejected() {
        let data = rust_image(
            true,
            EM_X86_64,
            rust_site(8, 2, [(0x9000, 3), MAIN_RS, FMT]),
        );
        assert_eq!(
            contract_from_elf(&data),
            Err(ElfError::BadEntry {
                index: 0,
                reason: "module pointer 0x9000 is outside the image".into()
            })
        );
    }

    #[test]
    fn string_running_past_its_section_is_rejected() {
        // Starts inside `.rodata` but claims more bytes than the section holds.
        let data = rust_image(
            true,
            EM_X86_64,
            rust_site(8, 2, [APP, (MAIN_RS.0, 500), FMT]),
        );
        assert_eq!(
            contract_from_elf(&data).unwrap_err().to_string(),
            "site descriptor 0: file pointer 0x1004 is outside the image"
        );
    }

    #[test]
    fn non_utf8_string_is_rejected() {
        let data = image(
            true,
            true,
            EM_X86_64,
            &[
                sec(".rodata", RODATA, b"\xff\xfe\xfd".to_vec()),
                sec("linkme_SITES", SITES, rust_site(8, 2, [APP, APP, APP])),
            ],
        );
        assert_eq!(
            contract_from_elf(&data),
            Err(ElfError::BadEntry {
                index: 0,
                reason: "module is not UTF-8".into()
            })
        );
    }

    #[test]
    fn site_section_past_end_of_file_is_a_parse_error() {
        for name in ["linkme_SITES", "rtlog_sites"] {
            let mut s = sec(name, SITES, vec![0; 8]);
            s.size = 1 << 20;
            let data = image(true, true, EM_X86_64, &[s]);
            assert!(
                matches!(contract_from_elf(&data), Err(ElfError::Parse(_))),
                "{name}"
            );
        }
    }

    /// A site section that does not hold whole descriptors is an error, not a
    /// contract with the partial descriptor silently left out.
    #[test]
    fn partial_rust_descriptor_is_an_error() {
        // One 64-byte descriptor plus 6 stray bytes.
        let mut sites = rust_site(8, 2, [APP, MAIN_RS, FMT]);
        sites.extend([0u8; 6]);
        match contract_from_elf(&rust_image(true, EM_X86_64, sites)) {
            Err(ElfError::BadEntry { index, reason }) => {
                assert_eq!(index, 1);
                assert!(
                    reason.contains(
                        "linkme_SITES is 70 bytes, not a whole number of 64-byte descriptors"
                    ),
                    "{reason}"
                );
            }
            other => panic!("{other:?}"),
        }
        // Shorter than one descriptor: not an empty contract.
        assert!(matches!(
            contract_from_elf(&rust_image(true, EM_X86_64, vec![0; 10])),
            Err(ElfError::BadEntry { index: 0, .. })
        ));
    }

    #[test]
    fn partial_c_descriptor_is_an_error() {
        let mut sites = c_site(b"RTLS", 3, 0, [FMT.0, MAIN_RS.0, APP.0]);
        sites.truncate(sites.len() - 1);
        match contract_from_elf(&c_image(sites)) {
            Err(ElfError::BadEntry { index: 0, reason }) => {
                assert!(reason.contains("rtlog_sites is 39 bytes"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
    }

    /// A PIE leaves pointer fields zero and stores the target in a RELATIVE
    /// relocation's addend; only RELATIVE relocations of a zero field apply.
    #[test]
    fn relative_relocations_fill_zero_pointers() {
        let sites = rust_site(8, 2, [(0, 3), MAIN_RS, FMT]);
        let reloc = |off: u64, ty: u64, addend: u64| {
            let mut r = (SITES + off).to_le_bytes().to_vec();
            r.extend(ty.to_le_bytes()); // symbol 0, type `ty`
            r.extend(addend.to_le_bytes());
            r
        };
        let mut relocs = reloc(16, R_X86_64_RELATIVE, APP.0); // module: zero field, applied
        relocs.extend(reloc(32, R_X86_64_RELATIVE, 0x9999)); // file: non-zero field, kept
        relocs.extend(reloc(48, R_X86_64_64, 0x9999)); // fmt: not RELATIVE, ignored
        let mut r = sec(".relocs", 0x3000, relocs);
        r.kind = SHT_RELOCS_ADDEND;
        let data = image(
            true,
            true,
            EM_X86_64,
            &[
                sec(".rodata", RODATA, STRS.to_vec()),
                sec("linkme_SITES", SITES, sites),
                r,
            ],
        );
        assert_eq!(contract_from_elf(&data).unwrap(), format!("{RUST_LINE}\n"));
    }

    #[test]
    fn c_descriptors_are_numbered_from_one() {
        // Pointers are fmt, file, func; strings are NUL-terminated.
        let mut sites = c_site(b"RTLS", 3, 0, [FMT.0, MAIN_RS.0, APP.0]);
        sites.extend(c_site(b"RTLS", 0, 1, [APP.0, MAIN_RS.0, APP.0]));
        assert_eq!(
            contract_from_elf(&c_image(sites)).unwrap(),
            concat!(
                r#"{"id":1,"level":"warn","module":"app","file":"src/main.rs","line":42,"column":0,"fmt":"x={}","nargs":1}"#,
                "\n",
                r#"{"id":2,"level":"trace","module":"app","file":"src/main.rs","line":42,"column":0,"fmt":"app","nargs":1,"kind":"span"}"#,
                "\n"
            )
        );
    }

    #[test]
    fn c_descriptor_with_bad_magic_is_rejected() {
        let mut sites = c_site(b"RTLS", 2, 0, [FMT.0, MAIN_RS.0, APP.0]);
        sites.extend(c_site(b"XXXX", 2, 0, [FMT.0, MAIN_RS.0, APP.0]));
        assert_eq!(
            contract_from_elf(&c_image(sites)),
            Err(ElfError::BadEntry {
                index: 1,
                reason: "bad magic".into()
            })
        );
    }

    #[test]
    fn c_descriptor_with_bad_level_is_rejected() {
        let sites = c_site(b"RTLS", 9, 0, [FMT.0, MAIN_RS.0, APP.0]);
        assert_eq!(
            contract_from_elf(&c_image(sites)),
            Err(ElfError::BadEntry {
                index: 0,
                reason: "bad level".into()
            })
        );
    }

    #[test]
    fn c_string_without_terminator_is_rejected() {
        let data = image(
            true,
            true,
            EM_X86_64,
            &[
                sec(".rodata", RODATA, b"no nul here".to_vec()),
                sec("rtlog_sites", SITES, c_site(b"RTLS", 2, 0, [RODATA; 3])),
            ],
        );
        assert_eq!(
            contract_from_elf(&data).unwrap_err().to_string(),
            "site descriptor 0: fmt pointer 0x1000 is outside the image"
        );
    }

    #[test]
    fn rust_and_c_sites_merge_sorted_by_id() {
        let data = image(
            true,
            true,
            EM_X86_64,
            &[
                sec(".rodata", RODATA, STRS.to_vec()),
                sec("linkme_SITES", SITES, rust_site(8, 2, [APP, MAIN_RS, FMT])),
                sec(
                    "rtlog_sites",
                    0x4000,
                    c_site(b"RTLS", 2, 0, [FMT.0, MAIN_RS.0, APP.0]),
                ),
            ],
        );
        let jsonl = contract_from_elf(&data).unwrap();
        let ids: Vec<u64> = jsonl
            .lines()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["id"]
                    .as_u64()
                    .unwrap()
            })
            .collect();
        assert_eq!(ids, [1, 0x1234]);
        assert_eq!(crate::Contract::from_jsonl(&jsonl).unwrap().len(), 2);
    }

    // ---- Mach-O and PE ---------------------------------------------------

    const MACHO_BASE: u64 = 0x1_0000_0000;
    const PE_BASE: u64 = 0x1_4000_0000;

    /// A `w`-byte-pointer Rust descriptor whose ID is the hash of its fields,
    /// as the macros compute it; its strings are `STRS` loaded at `strs`.
    fn hashed_site(strs: u64, span: bool) -> Vec<u8> {
        let id = if span {
            rtlog::site::span_id("app", "src/main.rs", 7, 5, "x={}")
        } else {
            rtlog::site::site_id(rtlog::Level::Info, "app", "src/main.rs", 7, 5, "x={}", 1)
        };
        let mut e = id.to_le_bytes().to_vec();
        e.extend([u8::from(span), 2, u8::from(!span), 0]);
        e.extend(7u32.to_le_bytes());
        e.extend(5u32.to_le_bytes());
        for (p, n) in [(strs, 3u64), (strs + 4, 11), (strs + 16, 4)] {
            e.extend(p.to_le_bytes());
            e.extend(n.to_le_bytes());
        }
        e
    }

    /// A minimal 64-bit Mach-O executable: `__TEXT,__cstring` with `STRS` at
    /// `MACHO_BASE + 0x1000`, and one `__DATA` section `name` at + 0x2000.
    fn macho(name: &str, sites: &[u8]) -> Vec<u8> {
        fn name16(n: &str) -> [u8; 16] {
            let mut b = [0u8; 16];
            b[..n.len()].copy_from_slice(n.as_bytes());
            b
        }
        fn segment(out: &mut Vec<u8>, seg: &str, sect: &str, addr: u64, off: u32, len: u64) {
            out.extend(0x19u32.to_le_bytes()); // LC_SEGMENT_64
            out.extend((72u32 + 80).to_le_bytes());
            out.extend(name16(seg));
            // The segment starts on the 64 KiB boundary below its section:
            // `__TEXT` at the image base, as in a linked executable.
            for v in [addr & !0xffff, 0x10000, 0, u64::from(off) + len] {
                out.extend(v.to_le_bytes());
            }
            for v in [7u32, 5, 1, 0] {
                out.extend(v.to_le_bytes()); // maxprot, initprot, nsects, flags
            }
            out.extend(name16(sect));
            out.extend(name16(seg));
            out.extend(addr.to_le_bytes());
            out.extend(len.to_le_bytes());
            for v in [off, 3, 0, 0, 0, 0, 0, 0] {
                out.extend(v.to_le_bytes());
            }
        }
        let mut out = Vec::new();
        for v in [0xfeed_facfu32, 0x0100_000c, 0, 2, 2, 2 * 152, 0, 0] {
            out.extend(v.to_le_bytes()); // header: arm64, MH_EXECUTE, 2 commands
        }
        segment(
            &mut out,
            "__TEXT",
            "__cstring",
            MACHO_BASE + 0x1000,
            0x400,
            STRS.len() as u64,
        );
        segment(
            &mut out,
            "__DATA",
            name,
            MACHO_BASE + 0x2000,
            0x800,
            sites.len() as u64,
        );
        out.resize(0x400, 0);
        out.extend(STRS);
        out.resize(0x800, 0);
        out.extend(sites);
        out
    }

    /// A minimal PE32+ image: `.rdata` with `STRS` at RVA 0x1000 and one more
    /// section `name` at RVA 0x2000.
    fn pe(name: &str, sites: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; 0x40];
        out[..2].copy_from_slice(b"MZ");
        out[0x3c] = 0x40; // e_lfanew
        out.extend(b"PE\0\0");
        out.extend(0x8664u16.to_le_bytes()); // x86-64
        out.extend(2u16.to_le_bytes()); // sections
        out.extend([0u8; 12]);
        out.extend(240u16.to_le_bytes()); // optional header size
        out.extend(0x22u16.to_le_bytes());
        let mut opt = vec![0u8; 240];
        opt[..2].copy_from_slice(&0x20bu16.to_le_bytes()); // PE32+
        opt[24..32].copy_from_slice(&PE_BASE.to_le_bytes());
        opt[32..36].copy_from_slice(&0x1000u32.to_le_bytes()); // section alignment
        opt[36..40].copy_from_slice(&0x200u32.to_le_bytes()); // file alignment
        opt[56..60].copy_from_slice(&0x3000u32.to_le_bytes()); // image size
        opt[60..64].copy_from_slice(&0x400u32.to_le_bytes()); // headers size
        opt[108..112].copy_from_slice(&16u32.to_le_bytes()); // data directories
        out.extend(opt);
        for (n, rva, off, len) in [
            (".rdata", 0x1000u32, 0x400u32, STRS.len() as u32),
            (name, 0x2000, 0x800, sites.len() as u32),
        ] {
            let mut nm = [0u8; 8];
            nm[..n.len()].copy_from_slice(n.as_bytes());
            out.extend(nm);
            for v in [len, rva, len, off, 0, 0, 0, 0x4000_0040] {
                out.extend(v.to_le_bytes());
            }
        }
        out.resize(0x400, 0);
        out.extend(STRS);
        out.resize(0x800, 0);
        out.extend(sites);
        out
    }

    const HASHED_FIELDS: &str =
        r#""level":"info","module":"app","file":"src/main.rs","line":7,"column":5,"fmt":"x={}""#;

    #[test]
    fn contract_from_elf_reads_elf_only() {
        let img = macho("__linkmeAbCdEfGh", &hashed_site(MACHO_BASE + 0x1000, false));
        assert_eq!(
            contract_from_elf(&img),
            Err(ElfError::Parse("only ELF images are supported".into()))
        );
        assert!(contract_from_binary(&img).is_ok());
        // contract_from_binary reads ELF as before.
        let elf = rust_image(true, EM_X86_64, rust_site(8, 2, [APP, MAIN_RS, FMT]));
        assert_eq!(
            contract_from_binary(&elf).unwrap(),
            format!("{RUST_LINE}\n")
        );
    }

    #[test]
    fn macho_rust_sites_are_found_by_their_hash() {
        let strs = MACHO_BASE + 0x1000;
        // Data of another linkme slice before and between the descriptors.
        let mut sites = vec![0xAAu8; 16];
        sites.extend(hashed_site(strs, false));
        sites.extend([0u8; 8]);
        sites.extend(hashed_site(strs, true));
        let out = contract_from_binary(&macho("__linkmeAbCdEfGh", &sites)).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert!(lines.iter().all(|l| l.contains(HASHED_FIELDS)), "{out}");
        assert_eq!(out.matches(r#""kind":"span""#).count(), 1);
    }

    #[test]
    fn macho_chained_fixups_are_decoded() {
        let strs = MACHO_BASE + 0x1000;
        // DYLD_CHAINED_PTR_64: the address in the low 36 bits, chain bits above.
        let mut absolute = hashed_site(strs, false);
        // DYLD_CHAINED_PTR_64_OFFSET: an offset from the image base instead.
        let mut offset = hashed_site(0x1000, false);
        for e in [&mut absolute, &mut offset] {
            for k in 0..3 {
                e[16 + 16 * k + 6] |= 0x10; // "next fixup" bits (bit 52)
            }
        }
        for (what, sites) in [("absolute", absolute), ("offset", offset)] {
            let out = contract_from_binary(&macho("__linkmeAbCdEfGh", &sites))
                .unwrap_or_else(|e| panic!("{what}: {e}"));
            assert!(out.contains(HASHED_FIELDS), "{out}");
        }
    }

    #[test]
    fn macho_c_sites_are_numbered_from_one() {
        let s = MACHO_BASE + 0x1000;
        let mut sites = c_site(b"RTLS", 2, 0, [s + 16, s + 4, s]);
        sites.extend(c_site(b"RTLS", 3, 1, [s + 16, s + 4, s]));
        let out = contract_from_binary(&macho("rtlog_sites", &sites)).unwrap();
        assert!(
            out.starts_with(
                r#"{"id":1,"level":"info","module":"app","file":"src/main.rs","line":42"#
            ),
            "{out}"
        );
        assert!(out.contains(r#"{"id":2,"level":"warn""#), "{out}");
    }

    #[test]
    fn macho_without_descriptors_has_no_sites() {
        // A linkme section of some other slice: nothing in it hashes.
        let img = macho("__linkmeAbCdEfGh", &[0x55u8; 96]);
        assert_eq!(contract_from_binary(&img), Err(ElfError::NoSites));
        // A descriptor whose ID does not match its fields is not ours either.
        let mut site = hashed_site(MACHO_BASE + 0x1000, false);
        site[0] ^= 1;
        let img = macho("__linkmeAbCdEfGh", &site);
        assert_eq!(contract_from_binary(&img), Err(ElfError::NoSites));
        // Nor one with an unknown level, kind or a string outside the image.
        for (at, v) in [(5, 9u8), (4, 2), (7, 1), (17, 0xff)] {
            let mut site = hashed_site(MACHO_BASE + 0x1000, false);
            site[at] = v;
            let img = macho("__linkmeAbCdEfGh", &site);
            assert_eq!(contract_from_binary(&img), Err(ElfError::NoSites), "{at}");
        }
    }

    #[test]
    fn pe_rust_sites_are_found_in_the_merged_linkme_section() {
        let mut sites = vec![0u8; 8];
        sites.extend(hashed_site(PE_BASE + 0x1000, false));
        sites.extend([0u8; 24]);
        let out = contract_from_binary(&pe(".linkme_", &sites)).unwrap();
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.contains(HASHED_FIELDS), "{out}");
    }

    #[test]
    fn pe_c_sites_skip_the_markers_and_count_from_the_first() {
        let s = PE_BASE + 0x1000;
        let mut sites = vec![0u8; 40]; // start marker
        sites.extend(c_site(b"RTLS", 2, 0, [s + 16, s + 4, s]));
        sites.extend(c_site(b"RTLS", 4, 0, [s + 16, s + 4, s]));
        sites.extend([0u8; 40]); // end marker
        let out = contract_from_binary(&pe("rtlog", &sites)).unwrap();
        let ids: Vec<&str> = out.lines().map(|l| &l[..8]).collect();
        assert_eq!(ids, [r#"{"id":1,"#, r#"{"id":2,"#], "{out}");
        // Only markers: no sites.
        let img = pe("rtlog", &[0u8; 80]);
        assert_eq!(contract_from_binary(&img), Err(ElfError::NoSites));
        // Anything else that is not a descriptor is an error, as on ELF.
        let mut bad = vec![0u8; 40];
        bad.extend(c_site(b"XXXX", 2, 0, [s + 16, s + 4, s]));
        assert_eq!(
            contract_from_binary(&pe("rtlog", &bad)),
            Err(ElfError::BadEntry {
                index: 1,
                reason: "bad magic".into()
            })
        );
    }

    #[test]
    fn universal_macho_reads_the_first_architecture() {
        let thin_img = macho("__linkmeAbCdEfGh", &hashed_site(MACHO_BASE + 0x1000, false));
        // fat_header and one fat_arch, big-endian; the image follows at 0x1000.
        let mut fat = Vec::new();
        for v in [
            0xcafe_babeu32,
            1,
            0x0100_000c,
            0,
            0x1000,
            thin_img.len() as u32,
            12,
        ] {
            fat.extend(v.to_be_bytes());
        }
        fat.resize(0x1000, 0);
        fat.extend(&thin_img);
        assert_eq!(contract_from_binary(&fat), contract_from_binary(&thin_img));
        assert!(contract_from_binary(&fat).unwrap().contains(HASHED_FIELDS));
        // A fat header with no readable architecture is a parse error.
        let e = contract_from_binary(&fat[..0x20]).unwrap_err();
        assert!(matches!(e, ElfError::Parse(_)), "{e:?}");
    }

    #[test]
    fn other_object_formats_are_rejected() {
        // A WebAssembly module header.
        let e = contract_from_binary(b"\0asm\x01\0\0\0").unwrap_err();
        assert!(matches!(e, ElfError::Parse(_)), "{e:?}");
    }
}
