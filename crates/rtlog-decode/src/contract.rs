//! Contract loading and validation (spec sections 4 and 9).

use std::collections::HashMap;
use std::fmt;

use rtlog::site::{contract_hash_of, count_placeholders};
use rtlog::wire::CONTROL_BASE;
use rtlog::Level;
use serde::Deserialize;

/// Largest argument count a contract may declare.
pub const MAX_NARGS: u8 = 32;
/// Largest format string accepted.
pub const MAX_FMT: usize = 65_535;

/// Whether a site is a log call or a span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A log call; `fmt` is its format string.
    Log,
    /// A span; `fmt` is its name.
    Span,
}

/// One site as read from the contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteInfo {
    /// Site ID that frames from this site carry.
    pub id: u32,
    /// Log call or span; [`Kind::Log`] when the contract line has no `kind`.
    pub kind: Kind,
    /// Level of the site.
    pub level: Level,
    /// Module path of the call site.
    pub module: String,
    /// Source file of the call site.
    pub file: String,
    /// Source line of the call site.
    pub line: u32,
    /// Source column of the call site.
    pub column: u32,
    /// Format string of a log call, or the name of a span.
    pub fmt: String,
    /// Number of arguments; equals the number of placeholders in `fmt`.
    pub nargs: u8,
}

#[derive(Deserialize)]
struct RawSite {
    id: u32,
    level: String,
    module: String,
    file: String,
    line: u32,
    column: u32,
    fmt: String,
    nargs: u8,
    #[serde(default)]
    kind: Option<String>,
}

/// A validated contract.
#[derive(Clone, Debug, Default)]
pub struct Contract {
    sites: HashMap<u32, SiteInfo>,
    hash: u32,
}

/// Why a contract was rejected. A rejected contract is never partially applied.
#[derive(Debug, PartialEq, Eq)]
pub enum ContractError {
    /// A line is not a valid site object.
    Json {
        /// 1-based line number in the contract text.
        line: usize,
        /// The JSON parser's message.
        msg: String,
    },
    /// A site's `level` is not one of the five level names.
    BadLevel {
        /// 1-based line number in the contract text.
        line: usize,
        /// The level as written.
        level: String,
    },
    /// Two lines declare the same site ID.
    DuplicateId {
        /// The repeated site ID.
        id: u32,
    },
    /// A site ID is in the control-frame range (at or above `CONTROL_BASE`).
    ReservedId {
        /// The reserved site ID.
        id: u32,
    },
    /// A site declares more than [`MAX_NARGS`] arguments.
    TooManyArgs {
        /// The site's ID.
        id: u32,
        /// The declared argument count.
        nargs: u8,
    },
    /// A site's format string is longer than [`MAX_FMT`] bytes.
    FmtTooLong {
        /// The site's ID.
        id: u32,
    },
    /// A site's `kind` is neither `"log"` nor `"span"`.
    BadKind {
        /// 1-based line number in the contract text.
        line: usize,
        /// The kind as written.
        kind: String,
    },
    /// A site's placeholder count differs from its argument count.
    PlaceholderMismatch {
        /// The site's ID.
        id: u32,
        /// Placeholders counted in the format string.
        placeholders: usize,
        /// The declared argument count.
        nargs: u8,
    },
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContractError::Json { line, msg } => write!(f, "line {line}: invalid JSON: {msg}"),
            ContractError::BadLevel { line, level } => {
                write!(f, "line {line}: unknown level {level:?}")
            }
            ContractError::DuplicateId { id } => write!(f, "duplicate site ID {id:#010x}"),
            ContractError::ReservedId { id } => {
                write!(f, "site ID {id:#010x} is in the control-frame range")
            }
            ContractError::TooManyArgs { id, nargs } => {
                write!(f, "site {id:#010x}: {nargs} args > {MAX_NARGS}")
            }
            ContractError::FmtTooLong { id } => {
                write!(f, "site {id:#010x}: format string longer than {MAX_FMT}")
            }
            ContractError::BadKind { line, kind } => {
                write!(f, "line {line}: unknown kind {kind:?}")
            }
            ContractError::PlaceholderMismatch {
                id,
                placeholders,
                nargs,
            } => {
                write!(
                    f,
                    "site {id:#010x}: {placeholders} placeholders but {nargs} args"
                )
            }
        }
    }
}

impl std::error::Error for ContractError {}

impl Contract {
    /// Parse and validate a JSON-lines contract.
    pub fn from_jsonl(text: &str) -> Result<Contract, ContractError> {
        let mut sites = HashMap::new();
        for (i, raw) in text.lines().enumerate() {
            let line = i + 1;
            if raw.trim().is_empty() {
                continue;
            }
            let r: RawSite = serde_json::from_str(raw).map_err(|e| ContractError::Json {
                line,
                msg: e.to_string(),
            })?;
            let level = Level::parse(&r.level).ok_or(ContractError::BadLevel {
                line,
                level: r.level.clone(),
            })?;
            if r.id >= CONTROL_BASE {
                return Err(ContractError::ReservedId { id: r.id });
            }
            if r.nargs > MAX_NARGS {
                return Err(ContractError::TooManyArgs {
                    id: r.id,
                    nargs: r.nargs,
                });
            }
            if r.fmt.len() > MAX_FMT {
                return Err(ContractError::FmtTooLong { id: r.id });
            }
            let placeholders = count_placeholders(&r.fmt);
            if placeholders != usize::from(r.nargs) {
                return Err(ContractError::PlaceholderMismatch {
                    id: r.id,
                    placeholders,
                    nargs: r.nargs,
                });
            }
            let kind = match r.kind.as_deref() {
                None | Some("log") => Kind::Log,
                Some("span") => Kind::Span,
                Some(k) => {
                    return Err(ContractError::BadKind {
                        line,
                        kind: k.to_owned(),
                    })
                }
            };
            let site = SiteInfo {
                id: r.id,
                kind,
                level,
                module: r.module,
                file: r.file,
                line: r.line,
                column: r.column,
                fmt: r.fmt,
                nargs: r.nargs,
            };
            if sites.insert(site.id, site).is_some() {
                return Err(ContractError::DuplicateId { id: r.id });
            }
        }
        let hash = contract_hash_of(sites.keys().copied());
        Ok(Contract { sites, hash })
    }

    /// The contract hash a matching stream header carries.
    pub fn hash(&self) -> u32 {
        self.hash
    }

    /// Look up a site.
    pub fn site(&self, id: u32) -> Option<&SiteInfo> {
        self.sites.get(&id)
    }

    /// Number of sites.
    pub fn len(&self) -> usize {
        self.sites.len()
    }

    /// Whether the contract has no sites.
    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"{"id":7,"level":"info","module":"m","file":"f.rs","line":1,"column":2,"fmt":"x={}","nargs":1}"#;

    #[test]
    fn parses_valid() {
        let c = Contract::from_jsonl(&format!("{LINE}\n\n")).unwrap();
        assert_eq!(c.len(), 1);
        let s = c.site(7).unwrap();
        assert_eq!((s.level, s.fmt.as_str(), s.nargs), (Level::Info, "x={}", 1));
        assert_eq!(c.hash(), contract_hash_of([7]));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(
            Contract::from_jsonl("{"),
            Err(ContractError::Json { line: 1, .. })
        ));
        assert_eq!(
            Contract::from_jsonl(&format!("{LINE}\n{LINE}")).unwrap_err(),
            ContractError::DuplicateId { id: 7 }
        );
        let bad_level = LINE.replace("info", "loud");
        assert!(matches!(
            Contract::from_jsonl(&bad_level),
            Err(ContractError::BadLevel { .. })
        ));
        let reserved = LINE.replace("\"id\":7", "\"id\":4294967040");
        assert!(matches!(
            Contract::from_jsonl(&reserved),
            Err(ContractError::ReservedId { .. })
        ));
        let many = LINE.replace("\"nargs\":1", "\"nargs\":33");
        assert!(matches!(
            Contract::from_jsonl(&many),
            Err(ContractError::TooManyArgs { .. })
        ));
        let kind = LINE.replace("\"nargs\":1", "\"nargs\":1,\"kind\":\"blob\"");
        assert!(matches!(
            Contract::from_jsonl(&kind),
            Err(ContractError::BadKind { .. })
        ));
        let mismatch = LINE.replace("\"nargs\":1", "\"nargs\":2");
        assert!(matches!(
            Contract::from_jsonl(&mismatch),
            Err(ContractError::PlaceholderMismatch { .. })
        ));
        let long = LINE.replace("x={}", &format!("{}{{}}", "a".repeat(MAX_FMT)));
        assert!(matches!(
            Contract::from_jsonl(&long),
            Err(ContractError::FmtTooLong { .. })
        ));
    }

    #[test]
    fn errors_display() {
        assert!(ContractError::DuplicateId { id: 1 }
            .to_string()
            .contains("duplicate"));
    }

    #[test]
    fn every_error_has_a_message_naming_the_line_or_site() {
        let cases = [
            (
                ContractError::Json {
                    line: 3,
                    msg: "EOF".into(),
                },
                "line 3: invalid JSON: EOF",
            ),
            (
                ContractError::BadLevel {
                    line: 2,
                    level: "loud".into(),
                },
                "line 2: unknown level \"loud\"",
            ),
            (
                ContractError::DuplicateId { id: 7 },
                "duplicate site ID 0x00000007",
            ),
            (
                ContractError::ReservedId { id: 0xffff_ff00 },
                "site ID 0xffffff00 is in the control-frame range",
            ),
            (
                ContractError::TooManyArgs { id: 7, nargs: 40 },
                "site 0x00000007: 40 args > 32",
            ),
            (
                ContractError::FmtTooLong { id: 7 },
                "site 0x00000007: format string longer than 65535",
            ),
            (
                ContractError::BadKind {
                    line: 1,
                    kind: "blob".into(),
                },
                "line 1: unknown kind \"blob\"",
            ),
            (
                ContractError::PlaceholderMismatch {
                    id: 7,
                    placeholders: 1,
                    nargs: 2,
                },
                "site 0x00000007: 1 placeholders but 2 args",
            ),
        ];
        for (e, want) in cases {
            assert_eq!(e.to_string(), want);
        }
    }

    #[test]
    fn missing_field_is_a_json_error_on_its_line() {
        let no_fmt = LINE.replace(r#""fmt":"x={}","#, "");
        let e = Contract::from_jsonl(&format!("{LINE}\n{no_fmt}")).unwrap_err();
        match e {
            ContractError::Json { line, msg } => {
                assert_eq!(line, 2);
                assert!(msg.contains("missing field `fmt`"), "{msg}");
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn wrong_typed_field_is_a_json_error() {
        let cases = [
            (LINE.replace("\"line\":1", "\"line\":\"1\""), "invalid type"),
            (LINE.replace("\"id\":7", "\"id\":-7"), "invalid value"),
            (
                LINE.replace("\"nargs\":1", "\"nargs\":300"),
                "invalid value",
            ),
            (
                LINE.replace("\"nargs\":1", "\"nargs\":1,\"kind\":5"),
                "invalid type",
            ),
            ("[1,2,3]".to_owned(), "invalid type"),
        ];
        for (text, want) in cases {
            match Contract::from_jsonl(&text) {
                Err(ContractError::Json { line: 1, msg }) => {
                    assert!(msg.contains(want), "{text}: {msg}")
                }
                r => panic!("{text}: {r:?}"),
            }
        }
    }

    #[test]
    fn empty_contract_has_no_sites() {
        let c = Contract::from_jsonl("\n  \n").unwrap();
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
        assert_eq!(c.site(7), None);
        assert_eq!(c.hash(), contract_hash_of([]));
        assert!(!Contract::from_jsonl(LINE).unwrap().is_empty());
    }

    #[test]
    fn explicit_kinds_are_read() {
        let span = LINE.replace("\"nargs\":1", "\"nargs\":1,\"kind\":\"span\"");
        let log = LINE
            .replace("\"id\":7", "\"id\":8")
            .replace("\"nargs\":1", "\"nargs\":1,\"kind\":\"log\"");
        let c = Contract::from_jsonl(&format!("{span}\n{log}")).unwrap();
        assert_eq!(c.site(7).unwrap().kind, Kind::Span);
        assert_eq!(c.site(8).unwrap().kind, Kind::Log);
    }
}
