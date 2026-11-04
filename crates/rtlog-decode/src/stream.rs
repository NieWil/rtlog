//! Stream parsing (spec section 9): header check, then frames per context.
//!
//! The stream is untrusted input: every read is bounds-checked, every
//! allocation is bounded by the bytes actually present, and the first parse
//! error ends decoding with the byte offset where it happened.
//!
//! [`Session`] decodes one frame at a time (live use: each transport write is
//! one frame); [`Decoder`] walks a whole captured stream with it.

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
    SpanOpen {
        ts_ns: u64,
        ctx: u32,
        id: u64,
        parent: u64,
        site: &'c SiteInfo,
    },
    SpanClose {
        ts_ns: u64,
        ctx: u32,
        id: u64,
    },
    CtxRegister {
        ctx: u32,
        name: String,
    },
    CtxClose {
        ctx: u32,
    },
    Dropped {
        ctx: u32,
        count: u32,
    },
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

/// Decoding state for one stream: header, per-context clocks and names.
pub struct Session<'c> {
    contract: &'c Contract,
    header: Header,
    clocks: HashMap<u32, u64>,
    names: HashMap<u32, String>,
    /// The header's contract hash did not match and decoding was forced.
    pub mismatched: bool,
}

/// Bounds-checked cursor over one buffer; offsets are reported from `base`.
struct Cursor<'b> {
    bytes: &'b [u8],
    pos: usize,
    start: usize,
    base: usize,
}

impl<'b> Cursor<'b> {
    fn bad(&self, reason: impl Into<String>) -> DecodeError {
        DecodeError::BadFrame {
            offset: self.base + self.start,
            reason: reason.into(),
        }
    }

    fn take(&mut self, n: usize, what: &str) -> Result<&'b [u8], DecodeError> {
        let s = self
            .bytes
            .get(self.pos..self.pos + n)
            .ok_or_else(|| self.bad(format!("truncated {what}")))?;
        self.pos += n;
        Ok(s)
    }

    fn u32(&mut self, what: &str) -> Result<u32, DecodeError> {
        let b = self.take(4, what)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self, what: &str) -> Result<u64, DecodeError> {
        let b = self.take(8, what)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }

    fn varint(&mut self, what: &str) -> Result<u64, DecodeError> {
        let (v, n) = varint::decode(&self.bytes[self.pos.min(self.bytes.len())..])
            .ok_or_else(|| self.bad(format!("bad {what}")))?;
        self.pos += n;
        Ok(v)
    }

    fn ctx(&mut self) -> Result<u32, DecodeError> {
        let v = self.varint("context")?;
        u32::try_from(v).map_err(|_| self.bad("context out of range"))
    }
}

impl<'c> Session<'c> {
    /// Check a stream header. With `force`, a contract-hash mismatch is
    /// accepted and flagged in [`mismatched`](Self::mismatched).
    pub fn new(contract: &'c Contract, header: &[u8], force: bool) -> Result<Self, DecodeError> {
        let header = Header::decode(header).ok_or(DecodeError::BadHeader)?;
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
        Ok(Session {
            contract,
            header,
            clocks: HashMap::new(),
            names: HashMap::new(),
            mismatched,
        })
    }

    /// The stream header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Context name, once announced.
    pub fn ctx_name(&self, ctx: u32) -> Option<&str> {
        self.names.get(&ctx).map(String::as_str)
    }

    /// Decode exactly one frame (as handed to a transport in one write).
    pub fn decode_frame(&mut self, frame: &[u8]) -> Result<Event<'c>, DecodeError> {
        let (ev, used) = self.next_in(frame, 0, 0)?;
        if used != frame.len() {
            return Err(DecodeError::BadFrame {
                offset: used,
                reason: format!("{} trailing bytes", frame.len() - used),
            });
        }
        Ok(ev)
    }

    fn timed(&mut self, c: &mut Cursor<'_>) -> Result<(u64, u32), DecodeError> {
        let delta = c.varint("timestamp")?;
        let ctx = c.ctx()?;
        let ticks = self.clocks.entry(ctx).or_insert(0);
        *ticks = ticks.saturating_add(delta);
        let hz = u128::from(self.header.clock_hz.max(1));
        let ts_ns = (u128::from(*ticks) * 1_000_000_000 / hz).min(u128::from(u64::MAX)) as u64;
        Ok((ts_ns, ctx))
    }

    /// Parse the event starting at `bytes[pos]`; returns it and the new position.
    fn next_in(
        &mut self,
        bytes: &[u8],
        pos: usize,
        base: usize,
    ) -> Result<(Event<'c>, usize), DecodeError> {
        let mut c = Cursor {
            bytes,
            pos,
            start: pos,
            base,
        };
        let id = c.u32("site ID")?;
        let ev = match id {
            wire::CTRL_CTX_REGISTER => {
                let ctx = c.ctx()?;
                let len = usize::from(c.take(1, "name")?[0]);
                let name = String::from_utf8_lossy(c.take(len, "name")?).into_owned();
                self.names.insert(ctx, name.clone());
                Event::CtxRegister { ctx, name }
            }
            wire::CTRL_CTX_CLOSE => Event::CtxClose { ctx: c.ctx()? },
            wire::CTRL_DROPPED => {
                let ctx = c.ctx()?;
                Event::Dropped {
                    ctx,
                    count: c.u32("count")?,
                }
            }
            wire::CTRL_SPAN_OPEN => {
                let (ts_ns, ctx) = self.timed(&mut c)?;
                let site_id = c.u32("span site")?;
                let id = c.u64("span ID")?;
                let parent = c.u64("parent span")?;
                let site = self
                    .contract
                    .site(site_id)
                    .ok_or(DecodeError::UnknownSite {
                        offset: base + pos,
                        id: site_id,
                    })?;
                Event::SpanOpen {
                    ts_ns,
                    ctx,
                    id,
                    parent,
                    site,
                }
            }
            wire::CTRL_SPAN_CLOSE => {
                let (ts_ns, ctx) = self.timed(&mut c)?;
                Event::SpanClose {
                    ts_ns,
                    ctx,
                    id: c.u64("span ID")?,
                }
            }
            id if id >= wire::CONTROL_BASE => {
                return Err(c.bad(format!("unsupported control frame {id:#010x}")))
            }
            id => {
                let site = self.contract.site(id).ok_or(DecodeError::UnknownSite {
                    offset: base + pos,
                    id,
                })?;
                let (ts_ns, ctx) = self.timed(&mut c)?;
                let mut input = &bytes[c.pos.min(bytes.len())..];
                let before = input.len();
                let mut args = Vec::with_capacity(usize::from(site.nargs));
                for _ in 0..site.nargs {
                    args.push(
                        value::parse(&mut input)
                            .map_err(|e: ValueError| c.bad(format!("{e:?}")))?,
                    );
                }
                c.pos += before - input.len();
                Event::Record(Record {
                    ts_ns,
                    ctx,
                    site,
                    args,
                })
            }
        };
        Ok((ev, c.pos))
    }
}

/// Iterator over the events of one captured stream.
pub struct Decoder<'c, 'b> {
    session: Session<'c>,
    bytes: &'b [u8],
    pos: usize,
    done: bool,
    /// The header's contract hash did not match and decoding was forced.
    pub mismatched: bool,
}

impl<'c, 'b> Decoder<'c, 'b> {
    /// Check the header; see [`Session::new`].
    pub fn new(contract: &'c Contract, bytes: &'b [u8], force: bool) -> Result<Self, DecodeError> {
        let session = Session::new(contract, bytes, force)?;
        let mismatched = session.mismatched;
        Ok(Decoder {
            session,
            bytes,
            pos: HEADER_LEN,
            done: false,
            mismatched,
        })
    }

    /// The stream header.
    pub fn header(&self) -> &Header {
        self.session.header()
    }

    /// Context names seen so far.
    pub fn ctx_name(&self, ctx: u32) -> Option<&str> {
        self.session.ctx_name(ctx)
    }
}

impl<'c> Iterator for Decoder<'c, '_> {
    type Item = Result<Event<'c>, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.pos >= self.bytes.len() {
            return None;
        }
        match self.session.next_in(self.bytes, self.pos, 0) {
            Ok((ev, pos)) => {
                self.pos = pos;
                Some(Ok(ev))
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}
