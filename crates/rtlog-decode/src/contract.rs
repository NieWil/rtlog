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

/// One site as read from the contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteInfo {
    pub id: u32,
    pub level: Level,
    pub module: String,
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub fmt: String,
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
    Json {
        line: usize,
        msg: String,
    },
    BadLevel {
        line: usize,
        level: String,
    },
    DuplicateId {
        id: u32,
    },
    ReservedId {
        id: u32,
    },
    TooManyArgs {
        id: u32,
        nargs: u8,
    },
    FmtTooLong {
        id: u32,
    },
    PlaceholderMismatch {
        id: u32,
        placeholders: usize,
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
            let site = SiteInfo {
                id: r.id,
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
}
