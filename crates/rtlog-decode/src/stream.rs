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
    /// Context ID of the ring the frame came from.
    pub ctx: u32,
    /// The contract entry for the frame's site ID.
    pub site: &'c SiteInfo,
    /// Decoded arguments in call order; `site.nargs` of them.
    pub args: Vec<Value>,
}

/// One item of a decoded stream.
#[derive(Clone, Debug, PartialEq)]
pub enum Event<'c> {
    /// A log frame.
    Record(Record<'c>),
    /// A span was entered.
    SpanOpen {
        /// Absolute time on the target clock, in nanoseconds.
        ts_ns: u64,
        /// Context that entered the span.
        ctx: u32,
        /// Span ID: high 32 bits are the context ID, low 32 bits a per-context counter.
        id: u64,
        /// ID of the enclosing span, or 0 at the root.
        parent: u64,
        /// The contract entry of the span site; `site.fmt` is the span name.
        site: &'c SiteInfo,
    },
    /// A span was exited.
    SpanClose {
        /// Absolute time on the target clock, in nanoseconds.
        ts_ns: u64,
        /// Context that exited the span.
        ctx: u32,
        /// ID of the span, as in its [`Event::SpanOpen`].
        id: u64,
    },
    /// A context announced its name.
    CtxRegister {
        /// The context ID.
        ctx: u32,
        /// Its name; invalid UTF-8 is replaced with U+FFFD.
        name: String,
    },
    /// A context ended: its ring was closed and fully drained.
    CtxClose {
        /// The context ID.
        ctx: u32,
    },
    /// Frames were dropped because a ring was full or a call did not fit.
    Dropped {
        /// Context whose frames were dropped.
        ctx: u32,
        /// Frames dropped since the previous report for that context.
        count: u32,
    },
    /// A context's clock was set (crash dumps carry only the tail of a ring).
    CtxTime {
        /// The context ID.
        ctx: u32,
        /// Absolute clock value in target ticks; later deltas add to it.
        ticks: u64,
    },
    /// A resync frame: the stream header again, as a point to recover at.
    Resync,
    /// The process crashed (or a snapshot was requested); last frame of a dump.
    Crash {
        /// Absolute time on the target clock, in nanoseconds.
        ts_ns: u64,
        /// Context that wrote the crash frame.
        ctx: u32,
        /// What the crash frame says.
        crash: Crash,
    },
}

/// What a crash record says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crash {
    /// 1 = panic, 2 = signal, 3 = snapshot requested by the program.
    pub kind: u8,
    /// Signal number for a signal crash, 0 otherwise.
    pub signal: i32,
    /// The signal's `si_code` for a signal crash, 0 otherwise.
    pub code: i32,
    /// Faulting address (signals), 0 otherwise.
    pub addr: u64,
    /// Program counter at the fault, where the platform reports it.
    pub pc: u64,
    /// Panic message, signal name or snapshot reason; at most 127 bytes, with
    /// invalid UTF-8 replaced with U+FFFD.
    pub message: String,
}

impl Crash {
    /// One-line description (control characters from the file become spaces).
    pub fn describe(&self) -> String {
        let message: String = self
            .message
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let message = &message;
        match self.kind {
            1 => format!("panic: {message}"),
            2 => {
                let mut s = format!("{message} (signal {}, code {})", self.signal, self.code);
                // SIGILL 4, SIGBUS 7, SIGFPE 8, SIGSEGV 11: the address means
                // something (data address, or faulting instruction for FPE/ILL).
                if matches!(self.signal, 4 | 7 | 8 | 11) {
                    s.push_str(&format!(" addr={:#x}", self.addr));
                }
                if self.pc != 0 {
                    s.push_str(&format!(" pc={:#x}", self.pc));
                }
                s
            }
            3 => format!("snapshot: {message}"),
            k => format!("crash kind {k}: {message}"),
        }
    }
}

/// Why decoding stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The stream does not start with a valid header.
    BadHeader,
    /// Unsupported frame grammar.
    WireVersion(u8),
    /// The stream was produced by a different build.
    ContractMismatch {
        /// Contract hash in the stream header.
        stream: u32,
        /// Hash of the loaded contract.
        contract: u32,
    },
    /// A frame names a site the contract does not have.
    UnknownSite {
        /// Byte offset of the frame in the input.
        offset: usize,
        /// The unknown site ID.
        id: u32,
    },
    /// A frame ends early or holds invalid bytes.
    BadFrame {
        /// Byte offset of the frame in the input; for trailing bytes after a
        /// frame given to [`Session::decode_frame`], the offset where they start.
        offset: usize,
        /// What was wrong, such as `truncated name`.
        reason: String,
    },
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
            wire::CTRL_RESYNC => {
                // A copy of the stream header: it must be the one this stream began
                // with, or this is not a resync point of it.
                let h = Header::decode(c.take(HEADER_LEN, "resync header")?)
                    .ok_or_else(|| c.bad("bad resync header"))?;
                if h != self.header {
                    return Err(c.bad("resync header differs from the stream's"));
                }
                Event::Resync
            }
            wire::CTRL_CTX_CLOSE => Event::CtxClose { ctx: c.ctx()? },
            wire::CTRL_DROPPED => {
                let ctx = c.ctx()?;
                Event::Dropped {
                    ctx,
                    count: c.u32("count")?,
                }
            }
            wire::CTRL_CTX_TIME => {
                let ctx = c.ctx()?;
                let ticks = c.u64("ticks")?;
                self.clocks.insert(ctx, ticks);
                Event::CtxTime { ctx, ticks }
            }
            wire::CTRL_CRASH => {
                let (ts_ns, ctx) = self.timed(&mut c)?;
                let kind = c.take(1, "crash kind")?[0];
                let signal = c.u32("signal")? as i32;
                let code = c.u32("signal code")? as i32;
                let addr = c.u64("fault address")?;
                let pc = c.u64("program counter")?;
                let len = c.take(1, "message")?[0];
                let message = String::from_utf8_lossy(c.take(usize::from(len & 0x7f), "message")?)
                    .into_owned();
                Event::Crash {
                    ts_ns,
                    ctx,
                    crash: Crash {
                        kind,
                        signal,
                        code,
                        addr,
                        pc,
                        message,
                    },
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
    recover: bool,
    /// The header's contract hash did not match and decoding was forced.
    pub mismatched: bool,
    /// Bytes skipped so far to get past bad frames.
    pub skipped_bytes: usize,
    /// After an `Err`, how many bytes were skipped to the next resync point
    /// (`None` when there is none and decoding ended).
    pub last_skip: Option<usize>,
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
            recover: true,
            mismatched,
            skipped_bytes: 0,
            last_skip: None,
        })
    }

    /// Whether to skip to the next resync frame after a bad frame (default), or
    /// to end decoding at the first one.
    pub fn recovering(mut self, on: bool) -> Self {
        self.recover = on;
        self
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
                // Look for the next resync frame past the start of the bad one.
                let from = self.pos + 1;
                let next = self
                    .recover
                    .then(|| {
                        self.bytes
                            .get(from..)?
                            .windows(wire::RESYNC_MARKER.len())
                            .position(|w| w == wire::RESYNC_MARKER)
                            .map(|i| from + i)
                    })
                    .flatten();
                match next {
                    Some(at) => {
                        self.last_skip = Some(at - self.pos);
                        self.skipped_bytes += at - self.pos;
                        self.pos = at;
                    }
                    None => {
                        self.last_skip = None;
                        self.done = true;
                    }
                }
                Some(Err(e))
            }
        }
    }
}
