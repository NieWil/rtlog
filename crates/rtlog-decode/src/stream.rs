//! Stream parsing (spec section 9): header check, then frames per context.
//!
//! The stream is untrusted input: every read is bounds-checked, every
//! allocation is bounded by the bytes actually present, and the first parse
//! error ends decoding with the byte offset where it happened.

use std::collections::HashMap;
use std::fmt;

use rtlog::varint;
use rtlog::wire::{self, Header, HEADER_LEN};

use crate::contract::{Contract, SiteInfo};
use crate::value::{self, Value, ValueError};

/// One decoded log record.
#[derive(Clone, Debug, PartialEq)]
pub struct Record<'c> {
    /// Absolute time on the target clock, in nanoseconds.
    pub ts_ns: u64,
    pub ctx: u32,
    pub site: &'c SiteInfo,
    pub args: Vec<Value>,
}

/// One item of a decoded stream.
#[derive(Clone, Debug, PartialEq)]
pub enum Event<'c> {
    Record(Record<'c>),
    CtxRegister { ctx: u32, name: String },
    CtxClose { ctx: u32 },
    Dropped { ctx: u32, count: u32 },
}

/// Why decoding stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The stream does not start with a valid header.
    BadHeader,
    /// Unsupported frame grammar.
    WireVersion(u8),
    /// The stream was produced by a different build.
    ContractMismatch { stream: u32, contract: u32 },
    /// A frame names a site the contract does not have.
    UnknownSite { offset: usize, id: u32 },
    /// A frame ends early or holds invalid bytes.
    BadFrame { offset: usize, reason: String },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::BadHeader => write!(f, "not an rtlog stream (bad or missing header)"),
            DecodeError::WireVersion(v) => write!(f, "unsupported wire version {v}"),
            DecodeError::ContractMismatch { stream, contract } => write!(
                f,
                "contract mismatch: stream {stream:#010x}, contract {contract:#010x} (use --force to decode anyway)"
            ),
            DecodeError::UnknownSite { offset, id } => write!(f, "offset {offset}: unknown site {id:#010x}"),
            DecodeError::BadFrame { offset, reason } => write!(f, "offset {offset}: bad frame: {reason}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Iterator over the events of one stream.
pub struct Decoder<'c, 'b> {
    contract: &'c Contract,
    header: Header,
    bytes: &'b [u8],
    pos: usize,
    clocks: HashMap<u32, u64>,
    names: HashMap<u32, String>,
    done: bool,
    /// The header's contract hash did not match and decoding was forced.
    pub mismatched: bool,
}

impl<'c, 'b> Decoder<'c, 'b> {
    /// Check the header. With `force`, a contract-hash mismatch is accepted and
    /// flagged in [`mismatched`](Self::mismatched).
    pub fn new(contract: &'c Contract, bytes: &'b [u8], force: bool) -> Result<Self, DecodeError> {
        let header = Header::decode(bytes).ok_or(DecodeError::BadHeader)?;
        if header.wire_version != wire::WIRE_VERSION {
            return Err(DecodeError::WireVersion(header.wire_version));
        }
        let mismatched = header.contract_hash != contract.hash();
        if mismatched && !force {
            return Err(DecodeError::ContractMismatch {
                stream: header.contract_hash,
                contract: contract.hash(),
            });
        }
        Ok(Decoder {
            contract,
            header,
            bytes,
            pos: HEADER_LEN,
            clocks: HashMap::new(),
            names: HashMap::new(),
            done: false,
            mismatched,
        })
    }

    /// The stream header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Context names seen so far.
    pub fn ctx_name(&self, ctx: u32) -> Option<&str> {
        self.names.get(&ctx).map(String::as_str)
    }

    fn bad(&self, offset: usize, reason: impl Into<String>) -> DecodeError {
        DecodeError::BadFrame {
            offset,
            reason: reason.into(),
        }
    }

    fn varint(&mut self, start: usize, what: &str) -> Result<u64, DecodeError> {
        let (v, n) = varint::decode(&self.bytes[self.pos..])
            .ok_or_else(|| self.bad(start, format!("bad {what}")))?;
        self.pos += n;
        Ok(v)
    }

    fn ctx(&mut self, start: usize) -> Result<u32, DecodeError> {
        let v = self.varint(start, "context")?;
        u32::try_from(v).map_err(|_| self.bad(start, "context out of range"))
    }

    fn next_event(&mut self) -> Result<Event<'c>, DecodeError> {
        let start = self.pos;
        let rest = &self.bytes[self.pos..];
        if rest.len() < 4 {
            return Err(self.bad(start, "truncated site ID"));
        }
        let id = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
        self.pos += 4;
        match id {
            wire::CTRL_CTX_REGISTER => {
                let ctx = self.ctx(start)?;
                let mut input = &self.bytes[self.pos..];
                let len = *input
                    .first()
                    .ok_or_else(|| self.bad(start, "truncated name"))?
                    as usize;
                input = &input[1..];
                if input.len() < len {
                    return Err(self.bad(start, "truncated name"));
                }
                let name = String::from_utf8_lossy(&input[..len]).into_owned();
                self.pos += 1 + len;
                self.names.insert(ctx, name.clone());
                Ok(Event::CtxRegister { ctx, name })
            }
            wire::CTRL_CTX_CLOSE => Ok(Event::CtxClose {
                ctx: self.ctx(start)?,
            }),
            wire::CTRL_DROPPED => {
                let ctx = self.ctx(start)?;
                let b = self
                    .bytes
                    .get(self.pos..self.pos + 4)
                    .ok_or_else(|| self.bad(start, "truncated count"))?;
                let count = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                self.pos += 4;
                Ok(Event::Dropped { ctx, count })
            }
            id if id >= wire::CONTROL_BASE => {
                Err(self.bad(start, format!("unsupported control frame {id:#010x}")))
            }
            id => {
                let site = self
                    .contract
                    .site(id)
                    .ok_or(DecodeError::UnknownSite { offset: start, id })?;
                let delta = self.varint(start, "timestamp")?;
                let ctx = self.ctx(start)?;
                let mut input = &self.bytes[self.pos..];
                let before = input.len();
                let mut args = Vec::with_capacity(usize::from(site.nargs));
                for _ in 0..site.nargs {
                    let v = value::parse(&mut input)
                        .map_err(|e: ValueError| self.bad(start, format!("{e:?}")))?;
                    args.push(v);
                }
                self.pos += before - input.len();
                let ticks = self.clocks.entry(ctx).or_insert(0);
                *ticks = ticks.saturating_add(delta);
                let hz = u128::from(self.header.clock_hz.max(1));
                let ts_ns =
                    (u128::from(*ticks) * 1_000_000_000 / hz).min(u128::from(u64::MAX)) as u64;
                Ok(Event::Record(Record {
                    ts_ns,
                    ctx,
                    site,
                    args,
                }))
            }
        }
    }
}

impl<'c> Iterator for Decoder<'c, '_> {
    type Item = Result<Event<'c>, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.pos >= self.bytes.len() {
            return None;
        }
        let r = self.next_event();
        if r.is_err() {
            self.done = true;
        }
        Some(r)
    }
}
