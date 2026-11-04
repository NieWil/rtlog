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
    o.push_str(&std::format!(",\"nargs\":{}}}", s.nargs));
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

/// Error from [`write`].
#[derive(Debug)]
pub enum ContractError {
    /// Two sites hash to the same ID.
    Collision {
        id: u32,
        a: String,
        b: String,
    },
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
}
