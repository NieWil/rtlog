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
    /// The file is not a readable little-endian ELF image, or a site section
    /// could not be read; holds the reason.
    Parse(String),
    /// The image has neither a `linkme_SITES` nor an `rtlog_sites` section.
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

/// A site section must hold whole descriptors. A remainder means the binary
/// lays descriptors out differently (another rtlog version) or the section is
/// damaged; reading on would silently drop sites from the contract.
fn whole_descriptors(bytes: &[u8], size: usize, section: &str) -> Result<(), ElfError> {
    if bytes.len() % size == 0 {
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
        whole_descriptors(bytes, size, "linkme_SITES")?;
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
        whole_descriptors(bytes, size, "rtlog_sites")?;
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
            "not a readable ELF file: short"
        );
        assert!(ElfError::NoSites
            .to_string()
            .starts_with("no rtlog site section (linkme_SITES or rtlog_sites)"));
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
            e.to_string().starts_with("not a readable ELF file: "),
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
}
