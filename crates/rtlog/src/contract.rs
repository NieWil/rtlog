//! Contract file: one JSON line per site (spec section 4).

use std::string::String;

use crate::site::{self, Site};

fn push_json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&std::format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// One site as a contract JSON line (no trailing newline).
pub fn site_json(s: &Site) -> String {
    let mut o = String::with_capacity(128);
    o.push_str(&std::format!(
        "{{\"id\":{},\"level\":\"{}\",\"module\":",
        s.id,
        s.level.as_str()
    ));
    push_json_str(&mut o, s.module);
    o.push_str(",\"file\":");
    push_json_str(&mut o, s.file);
    o.push_str(&std::format!(
        ",\"line\":{},\"column\":{},\"fmt\":",
        s.line,
        s.column
    ));
    push_json_str(&mut o, s.fmt);
    o.push_str(&std::format!(",\"nargs\":{}", s.nargs));
    if s.kind == crate::site::SiteKind::Span {
        o.push_str(",\"kind\":\"span\"");
    }
    o.push('}');
    o
}

/// The contract of this binary, sorted by site ID, one JSON object per line.
pub fn to_jsonl() -> String {
    let mut sites: std::vec::Vec<&Site> = site::sites().iter().collect();
    sites.sort_by_key(|s| s.id);
    let mut out = String::new();
    for s in sites {
        out.push_str(&site_json(s));
        out.push('\n');
    }
    out
}

/// Error from `write`.
#[derive(Debug)]
pub enum ContractError {
    /// Two sites hash to the same ID.
    Collision {
        /// The shared site ID.
        id: u32,
        /// `file:line` of the first site.
        a: String,
        /// `file:line` of the second site.
        b: String,
    },
    /// Writing the contract file failed.
    Io(std::io::Error),
}

impl core::fmt::Display for ContractError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ContractError::Collision { id, a, b } => {
                write!(f, "site ID collision {id:#010x}: {a} and {b}")
            }
            ContractError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ContractError {}

/// Check for collisions and write the contract to `path`.
pub fn write(path: impl AsRef<std::path::Path>) -> Result<(), ContractError> {
    if let Some((a, b)) = site::find_collision() {
        return Err(ContractError::Collision {
            id: a.id,
            a: std::format!("{}:{}", a.file, a.line),
            b: std::format!("{}:{}", b.file, b.line),
        });
    }
    std::fs::write(path, to_jsonl()).map_err(ContractError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::Level;

    #[test]
    fn escapes_json() {
        let s = Site::new(Level::Warn, "m", "a\\b.rs", 3, 4, "say \"{}\"\n", 1);
        let j = site_json(&s);
        assert!(j.contains(r#""file":"a\\b.rs""#), "{j}");
        assert!(j.contains(r#""fmt":"say \"{}\"\n""#), "{j}");
        assert!(j.contains(r#""level":"warn""#));
        assert!(j.ends_with(r#""nargs":1}"#));
    }

    #[test]
    fn escapes_control_characters() {
        let s = Site::new(Level::Info, "m", "f.rs", 1, 1, "a\rb\tc\u{1}d", 0);
        let j = site_json(&s);
        assert!(j.contains(r#""fmt":"a\rb\tc\u0001d""#), "{j}");
    }

    #[test]
    fn span_sites_are_marked() {
        let s = Site::span("m", "f.rs", 1, 1, "parse");
        let j = site_json(&s);
        assert!(j.ends_with(r#""nargs":0,"kind":"span"}"#), "{j}");
    }

    #[test]
    fn to_jsonl_lists_every_site_sorted_by_id() {
        let text = to_jsonl();
        let ids: std::vec::Vec<u32> = text
            .lines()
            .map(|l| {
                let rest = l.strip_prefix(r#"{"id":"#).unwrap();
                rest[..rest.find(',').unwrap()].parse().unwrap()
            })
            .collect();
        assert_eq!(ids.len(), site::sites().len());
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:?}");
    }

    #[test]
    fn an_unwritable_path_is_an_io_error() {
        let dir =
            std::env::temp_dir().join(std::format!("rtlog-no-such-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let err = write(dir.join("app.contract.jsonl")).unwrap_err();
        let ContractError::Io(e) = &err else {
            panic!("expected an I/O error, got {err:?}");
        };
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
        // Display shows the underlying error.
        assert_eq!(std::format!("{err}"), std::format!("{e}"));
    }

    #[test]
    fn write_creates_the_contract_file() {
        let path =
            std::env::temp_dir().join(std::format!("rtlog-contract-{}.jsonl", std::process::id()));
        write(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), to_jsonl());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_collision_names_both_sites() {
        let e = ContractError::Collision {
            id: 0x1234,
            a: "a.rs:1".into(),
            b: "b.rs:2".into(),
        };
        assert_eq!(
            std::format!("{e}"),
            "site ID collision 0x00001234: a.rs:1 and b.rs:2"
        );
    }
}
