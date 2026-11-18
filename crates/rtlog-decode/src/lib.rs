//! Host decoder for [rtlog](https://docs.rs/rtlog): joins a stream with its
//! contract and renders text, JSON lines, a Perfetto / Chrome trace or
//! syslog lines.
//!
//! - [`decode`] / [`decode_with`]: a whole capture at once; [`stream::Decoder`]
//!   event by event. After a damaged frame decoding resumes at the next resync
//!   frame and the gap is reported.
//! - [`syslog`]: RFC 5424 lines for a syslog daemon or a log collector.
//! - [`otlp`]: OpenTelemetry log records (OTLP JSON) for a collector.
//! - [`stdout::init`]: text on stdout while the program runs, like `env_logger`.
//! - [`elf::contract_from_binary`]: the contract from an ELF, Mach-O or PE
//!   binary, firmware included.
//! - [`Contract::verify_stripped`]: check that a stripped image ships no
//!   format string.
//! - The `rtlog` command-line tool (`cargo install rtlog-decode`): `rtlog decode`,
//!   `rtlog check`, `rtlog contract`, `rtlog verify`; see the
//!   [CLI reference](https://github.com/NieWil/rtlog/blob/main/docs/reference.md).
//!
//! ```
//! use rtlog_decode::{decode, Contract, Output};
//!
//! rtlog::info!("motor speed={} rpm", 1200u16);
//! let mut drain = rtlog::Drain::new(Vec::new());
//! drain.poll().unwrap();
//!
//! let contract = Contract::from_jsonl(&rtlog::contract::to_jsonl()).unwrap();
//! let out = decode(&contract, drain.transport(), Output::Text, false).unwrap();
//! assert!(out.text.contains("INFO  motor speed=1200 rpm"));
//! ```

#![warn(missing_docs)]

pub mod contract;
pub mod elf;
pub mod otlp;
mod perfetto;
pub mod render;
pub mod stdout;
pub mod stream;
pub mod syslog;
pub mod value;

use std::collections::HashMap;
use std::fmt::Write as _;

pub use contract::{Contract, ContractError, Kind, SiteInfo, Stripped, VERIFY_MIN_LEN};
pub use stream::{Crash, DecodeError, Decoder, Event, Record, Session};
pub use value::Value;

/// Output format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// `0.000412 [motor] INFO  pid out=0.42 err=-3`, spans as `▶`/`◀` lines.
    Text,
    /// One JSON object per record or finished span.
    Json,
    /// Chrome trace-event JSON for Perfetto, `chrome://tracing` or Speedscope.
    Perfetto,
    /// RFC 5424 syslog lines, one per record; see [`syslog`].
    Syslog,
    /// OpenTelemetry log records, OTLP JSON lines; see [`otlp`].
    OtlpJson,
}

/// What a text line shows before the message, for [`Options::fields`] and
/// [`stdout::Config::fields`]. The order on the line is fixed:
///
/// ```text
/// 0.000412 [motor] INFO  app::motor src/main.rs:42: pid out=0.42 err=-3
/// time     ctx     level module     file    :line   message
/// ```
///
/// The default is time, context and level. Parse a list with
/// [`Fields::parse`]: `"level,module"`, `"default,line"`, `"all"`, `"none"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fields {
    /// Seconds since the first event, to the microsecond.
    pub time: bool,
    /// The name of the thread or task, in brackets.
    pub ctx: bool,
    /// The level.
    pub level: bool,
    /// The module path of the call site (Rust) or its function (C).
    pub module: bool,
    /// The source file of the call site.
    pub file: bool,
    /// The source line, after the file; shows the file too.
    pub line: bool,
}

impl Fields {
    /// Time, context and level: what a line shows unless told otherwise.
    pub const DEFAULT: Fields = Fields {
        time: true,
        ctx: true,
        level: true,
        module: false,
        file: false,
        line: false,
    };
    /// Only the message.
    pub const NONE: Fields = Fields {
        time: false,
        ctx: false,
        level: false,
        module: false,
        file: false,
        line: false,
    };
    /// Every field.
    pub const ALL: Fields = Fields {
        time: true,
        ctx: true,
        level: true,
        module: true,
        file: true,
        line: true,
    };

    /// Parse a comma-separated list of `time`, `ctx`, `level`, `module`,
    /// `file`, `line`, plus `default` (time, ctx, level), `all` and `none`.
    /// The line shows exactly what the list names: `"default,module"` adds
    /// the module to the usual line, `"level"` shows the level alone.
    /// Returns the name that is not a field on error.
    pub fn parse(list: &str) -> Result<Fields, String> {
        let mut f = Fields::NONE;
        for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            match name {
                "time" => f.time = true,
                "ctx" => f.ctx = true,
                "level" => f.level = true,
                "module" => f.module = true,
                "file" => f.file = true,
                "line" => f.line = true,
                "default" => {
                    f.time = true;
                    f.ctx = true;
                    f.level = true;
                }
                "all" => f = Fields::ALL,
                "none" => {}
                other => return Err(other.to_owned()),
            }
        }
        Ok(f)
    }
}

impl Default for Fields {
    fn default() -> Self {
        Fields::DEFAULT
    }
}

/// Decoding options.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Output format; defaults to [`Output::Text`].
    pub output: Output,
    /// Decode even if the stream's contract hash does not match.
    pub force: bool,
    /// ANSI colours in text output.
    pub color: bool,
    /// Show span open/close lines in text output.
    pub spans: bool,
    /// What a text line shows before the message; see [`Fields`].
    pub fields: Fields,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            output: Output::Text,
            force: false,
            color: false,
            spans: true,
            fields: Fields::DEFAULT,
        }
    }
}

/// One timeline item, in capture order before sorting.
pub(crate) enum Item<'c> {
    Record(Record<'c>),
    SpanOpen {
        ts_ns: u64,
        ctx: u32,
        id: u64,
        parent: u64,
        name: &'c str,
    },
    SpanClose {
        ts_ns: u64,
        ctx: u32,
        id: u64,
    },
    Dropped {
        ts_ns: u64,
        ctx: u32,
        count: u32,
    },
    Crash {
        ts_ns: u64,
        ctx: u32,
        crash: Crash,
    },
    /// A stretch of the stream that could not be decoded: it starts at byte
    /// `offset`; decoding went on `skipped` bytes later, at the next resync frame.
    Gap {
        ts_ns: u64,
        offset: usize,
        reason: String,
        skipped: usize,
    },
}

impl Item<'_> {
    pub(crate) fn ts(&self) -> u64 {
        match self {
            Item::Record(r) => r.ts_ns,
            Item::SpanOpen { ts_ns, .. }
            | Item::SpanClose { ts_ns, .. }
            | Item::Dropped { ts_ns, .. }
            | Item::Gap { ts_ns, .. }
            | Item::Crash { ts_ns, .. } => *ts_ns,
        }
    }
}

/// Result of decoding a whole stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decoded {
    /// Rendered output. Text lines show seconds since the first event; JSON and
    /// Perfetto keep absolute nanoseconds and microseconds since the first event.
    pub text: String,
    /// Log records decoded.
    pub records: usize,
    /// Spans opened (`span_open` frames decoded).
    pub spans: usize,
    /// Sum of the counts in all `dropped` frames.
    pub dropped: u64,
    /// Contexts that announced a name (distinct `ctx_register` IDs).
    pub contexts: usize,
    /// Set when decoding stopped early (a bad frame with no resync frame after
    /// it); the output holds what came before.
    pub error: Option<DecodeError>,
    /// Bad stretches skipped by resuming at a resync frame. Each is a `GAP` line
    /// in the text output and a `gap` record in JSON.
    pub gaps: usize,
    /// Bytes of the stream skipped in those gaps.
    pub skipped_bytes: usize,
    /// The crash record, when the stream is a crash dump.
    pub crash: Option<Crash>,
}

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const MAGENTA: &str = "\x1b[35m";
const RED_BOLD: &str = "\x1b[1;31m";

fn level_color(level: rtlog::Level) -> &'static str {
    match level {
        rtlog::Level::Trace => "\x1b[2;37m",
        rtlog::Level::Debug => "\x1b[34m",
        rtlog::Level::Info => "\x1b[32m",
        rtlog::Level::Warn => "\x1b[33m",
        rtlog::Level::Error => "\x1b[1;31m",
    }
}

struct Painter {
    on: bool,
}

impl Painter {
    fn paint(&self, color: &str, s: &str) -> String {
        if self.on {
            format!("{color}{s}{RESET}")
        } else {
            s.to_owned()
        }
    }
}

fn stamp(ts_ns: u64, t0: u64) -> String {
    let rel = ts_ns.saturating_sub(t0);
    format!(
        "{}.{:06}",
        rel / 1_000_000_000,
        (rel % 1_000_000_000) / 1_000
    )
}

fn human_ns(ns: u64) -> String {
    match ns {
        0..=9_999 => format!("{ns} ns"),
        10_000..=9_999_999 => format!("{:.1} µs", ns as f64 / 1e3),
        10_000_000..=9_999_999_999 => format!("{:.2} ms", ns as f64 / 1e6),
        _ => format!("{:.3} s", ns as f64 / 1e9),
    }
}

/// Renders timeline items as text lines, one item at a time. Shared by the
/// whole-capture decoder and the live stdout mode.
pub(crate) struct TextRenderer {
    paint: Painter,
    spans: bool,
    pub(crate) fields: Fields,
    mark: &'static str,
    t0: Option<u64>,
    pub(crate) names: HashMap<u32, String>,
    depth: HashMap<u32, usize>,
    open: HashMap<u64, (u64, String)>,
}

impl TextRenderer {
    /// `t0`: the timestamp shown as zero; `None` takes the first item's.
    pub(crate) fn new(color: bool, spans: bool, mismatched: bool, t0: Option<u64>) -> Self {
        TextRenderer {
            paint: Painter { on: color },
            spans,
            fields: Fields::DEFAULT,
            mark: if mismatched { "?" } else { "" },
            t0,
            names: HashMap::new(),
            depth: HashMap::new(),
            open: HashMap::new(),
        }
    }

    fn prefix(&mut self, ts_ns: u64, ctx: u32) -> String {
        self.prefix_with(ts_ns, ctx, true)
    }

    /// `anchor`: whether this item may set the time shown as zero. A drop
    /// report carries no time of its own, so it must not: if it arrived first
    /// every later line would print absolute ticks.
    fn prefix_with(&mut self, ts_ns: u64, ctx: u32, anchor: bool) -> String {
        let name = self
            .names
            .get(&ctx)
            .cloned()
            .unwrap_or_else(|| format!("ctx{ctx}"));
        self.labelled(ts_ns, &name, anchor)
    }

    /// `<seconds> [<label>] `, as far as the fields ask; see
    /// [`Self::prefix_with`] for `anchor`.
    fn labelled(&mut self, ts_ns: u64, label: &str, anchor: bool) -> String {
        let t0 = match self.t0 {
            Some(t) => t,
            None if anchor => *self.t0.get_or_insert(ts_ns),
            None => ts_ns,
        };
        let mut out = String::new();
        if self.fields.time {
            out.push_str(&self.paint.paint(DIM, &stamp(ts_ns, t0)));
            out.push(' ');
        }
        if self.fields.ctx {
            out.push_str(&self.paint.paint(CYAN, &format!("[{label}]")));
            out.push(' ');
        }
        out
    }

    /// `<level> `, padded to five letters, when levels are shown.
    fn tag(&self, color: &str, tag: &str) -> String {
        if self.fields.level {
            format!("{} ", self.paint.paint(color, tag))
        } else {
            String::new()
        }
    }

    /// `<module> <file>:<line>: ` for a call site, as far as asked for.
    fn origin(&self, site: &SiteInfo) -> String {
        let mut out = String::new();
        if self.fields.module && !site.module.is_empty() {
            out.push_str(&site.module);
            out.push(' ');
        }
        if self.fields.line {
            out.push_str(&format!("{}:{}: ", site.file, site.line));
        } else if self.fields.file {
            out.push_str(&format!("{}: ", site.file));
        }
        if out.is_empty() {
            out
        } else {
            self.paint.paint(DIM, &out)
        }
    }

    /// The text line for `item`, if it has one.
    pub(crate) fn render(&mut self, item: &Item<'_>) -> Option<String> {
        let p = Painter { on: self.paint.on };
        Some(match item {
            Item::Record(r) => {
                let d = self.depth.get(&r.ctx).copied().unwrap_or(0);
                let level = format!("{:<5}", r.site.level.as_str().to_ascii_uppercase());
                format!(
                    "{}{}{}{}{}{}",
                    self.prefix(r.ts_ns, r.ctx),
                    self.tag(level_color(r.site.level), &level),
                    self.origin(r.site),
                    "  ".repeat(d),
                    self.mark,
                    render::format(&r.site.fmt, &r.args),
                )
            }
            Item::SpanOpen {
                ts_ns,
                ctx,
                id,
                name,
                ..
            } => {
                let prefix = self.prefix(*ts_ns, *ctx);
                let d = self.depth.entry(*ctx).or_insert(0);
                let indent = "  ".repeat(*d);
                *d += 1;
                self.open.insert(*id, (*ts_ns, (*name).to_owned()));
                if !self.spans {
                    return None;
                }
                format!(
                    "{prefix}{}{indent}{}",
                    self.tag(MAGENTA, "SPAN "),
                    p.paint(MAGENTA, &format!("▶ {name}"))
                )
            }
            Item::SpanClose { ts_ns, ctx, id } => {
                let (start, name) = self.open.remove(id)?;
                let d = self.depth.entry(*ctx).or_insert(1);
                *d = d.saturating_sub(1);
                let indent = "  ".repeat(*d);
                if !self.spans {
                    return None;
                }
                format!(
                    "{}{}{indent}{} {}",
                    self.prefix(*ts_ns, *ctx),
                    self.tag(MAGENTA, "SPAN "),
                    p.paint(MAGENTA, &format!("◀ {name}")),
                    p.paint(DIM, &human_ns(ts_ns.saturating_sub(start))),
                )
            }
            Item::Gap {
                ts_ns,
                offset,
                reason,
                skipped,
            } => format!(
                "{}{}{}",
                self.labelled(*ts_ns, "stream", false),
                self.tag(RED_BOLD, "GAP  "),
                p.paint(
                    RED_BOLD,
                    &format!("bad data at byte {offset} ({reason}); {skipped} bytes skipped, resumed at the next resync frame")
                ),
            ),
            Item::Dropped { ts_ns, ctx, count } => format!(
                "{}{}{}",
                self.prefix_with(*ts_ns, *ctx, false),
                self.tag(RED_BOLD, "DROP "),
                p.paint(RED_BOLD, &format!("{count} frames dropped")),
            ),
            Item::Crash { ts_ns, ctx, crash } => format!(
                "{}{}{}",
                self.prefix(*ts_ns, *ctx),
                self.tag(RED_BOLD, if crash.kind == 3 { "SNAP " } else { "CRASH" }),
                p.paint(RED_BOLD, &crash.describe()),
            ),
        })
    }
}

/// Where the target's clock sits in wall-clock time, for the exporters that
/// write a time of day: target time `target` was wall-clock time `wall`
/// (nanoseconds since 1970-01-01T00:00:00Z).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WallClock {
    wall: u64,
    target: u64,
}

impl WallClock {
    /// From `epoch_ns`, the wall-clock time of the target's time zero, if
    /// given; else from `start_ns`, the wall-clock time of the first event
    /// of `items`. A drop report or a gap has no time of its own and is not
    /// the first event.
    pub(crate) fn new(
        items: &[Item<'_>],
        epoch_ns: Option<u64>,
        start_ns: Option<u64>,
    ) -> Option<Self> {
        if let Some(wall) = epoch_ns {
            return Some(WallClock { wall, target: 0 });
        }
        let first = items
            .iter()
            .filter(|i| !matches!(i, Item::Dropped { .. } | Item::Gap { .. }))
            .map(Item::ts)
            .min()
            .unwrap_or(0);
        start_ns.map(|wall| WallClock {
            wall,
            target: first,
        })
    }

    /// The wall-clock time of target time `ts_ns`. The target's clock may
    /// itself read more than the wall-clock start (a clock that counts from
    /// 1970 and a start time in the past), so only the difference is used.
    pub(crate) fn at(&self, ts_ns: u64) -> u64 {
        self.wall.saturating_add(ts_ns.saturating_sub(self.target))
    }
}

/// Decode a whole stream; see [`decode_with`].
pub fn decode(
    contract: &Contract,
    bytes: &[u8],
    output: Output,
    force: bool,
) -> Result<Decoded, DecodeError> {
    decode_with(
        contract,
        bytes,
        &Options {
            output,
            force,
            ..Options::default()
        },
    )
}

/// Decode a whole stream; events are merged across contexts by timestamp.
///
/// Header and contract errors are returned as `Err`. A bad frame later in the
/// stream is skipped up to the next resync frame and counted in
/// [`Decoded::gaps`] and [`Decoded::skipped_bytes`]; decoding stops early, with
/// the reason in [`Decoded::error`], only when there is no resync frame left
/// to resume at.
pub fn decode_with(
    contract: &Contract,
    bytes: &[u8],
    opts: &Options,
) -> Result<Decoded, DecodeError> {
    decode_inner(
        contract,
        bytes,
        opts,
        &syslog::Config::default(),
        &otlp::Config::default(),
    )
}

/// [`decode_with`], with the settings of the syslog and OTLP outputs.
pub(crate) fn decode_inner(
    contract: &Contract,
    bytes: &[u8],
    opts: &Options,
    syslog: &syslog::Config,
    otlp: &otlp::Config,
) -> Result<Decoded, DecodeError> {
    let mut dec = Decoder::new(contract, bytes, opts.force)?;
    let mismatched = dec.mismatched;
    let clock_hz = dec.header().clock_hz;
    let mut names: HashMap<u32, String> = HashMap::new();
    let mut last_ts: HashMap<u32, u64> = HashMap::new();
    let mut items = Vec::new();
    let mut result = Decoded::default();
    // Latest time seen on any context: where a gap is placed on the timeline.
    let mut now_ns = 0u64;
    while let Some(ev) = dec.next() {
        match &ev {
            Ok(Event::Record(r)) => now_ns = now_ns.max(r.ts_ns),
            Ok(Event::SpanOpen { ts_ns, .. } | Event::SpanClose { ts_ns, .. }) => {
                now_ns = now_ns.max(*ts_ns)
            }
            _ => {}
        }
        match ev {
            Ok(Event::Record(r)) => {
                last_ts.insert(r.ctx, r.ts_ns);
                items.push(Item::Record(r));
            }
            Ok(Event::SpanOpen {
                ts_ns,
                ctx,
                id,
                parent,
                site,
            }) => {
                last_ts.insert(ctx, ts_ns);
                items.push(Item::SpanOpen {
                    ts_ns,
                    ctx,
                    id,
                    parent,
                    name: &site.fmt,
                });
            }
            Ok(Event::SpanClose { ts_ns, ctx, id }) => {
                last_ts.insert(ctx, ts_ns);
                items.push(Item::SpanClose { ts_ns, ctx, id });
            }
            Ok(Event::CtxRegister { ctx, name }) => {
                names.insert(ctx, name);
            }
            Ok(Event::Dropped { ctx, count }) => {
                result.dropped += u64::from(count);
                let ts_ns = last_ts.get(&ctx).copied().unwrap_or(0);
                items.push(Item::Dropped { ts_ns, ctx, count });
            }
            Ok(Event::Crash { ts_ns, ctx, crash }) => {
                result.crash = Some(crash.clone());
                items.push(Item::Crash { ts_ns, ctx, crash });
            }
            Ok(Event::CtxClose { .. } | Event::CtxTime { .. } | Event::Resync) => {}
            Err(e) => match dec.last_skip {
                // Resumes at the next resync frame: note the gap and go on.
                Some(skipped) => {
                    let (offset, reason) = match &e {
                        DecodeError::BadFrame { offset, reason } => (*offset, reason.clone()),
                        DecodeError::UnknownSite { offset, id } => {
                            (*offset, format!("unknown site {id:#010x}"))
                        }
                        other => (0, other.to_string()),
                    };
                    result.gaps += 1;
                    items.push(Item::Gap {
                        ts_ns: now_ns,
                        offset,
                        reason,
                        skipped,
                    });
                }
                None => result.error = Some(e),
            },
        }
    }
    result.skipped_bytes = dec.skipped_bytes;
    items.sort_by_key(Item::ts);
    result.records = items
        .iter()
        .filter(|i| matches!(i, Item::Record(_)))
        .count();
    result.spans = items
        .iter()
        .filter(|i| matches!(i, Item::SpanOpen { .. }))
        .count();
    result.contexts = names.len();
    // Drop reports carry no timestamp of their own; never let them set t0.
    let t0 = items
        .iter()
        .filter(|i| !matches!(i, Item::Dropped { .. } | Item::Gap { .. }))
        .map(Item::ts)
        .min()
        .unwrap_or(0);
    let name_of = |ctx: u32| {
        names
            .get(&ctx)
            .cloned()
            .unwrap_or_else(|| format!("ctx{ctx}"))
    };

    match opts.output {
        Output::Perfetto => result.text = perfetto::export(&items, &names, clock_hz),
        Output::OtlpJson => result.text = otlp::export(&items, &names, mismatched, otlp),
        Output::Syslog => {
            result.text = syslog::export(&items, &names, mismatched, opts.spans, syslog)
        }
        Output::Json => {
            let mut open: HashMap<u64, (u64, u64, &str)> = HashMap::new();
            for item in &items {
                match item {
                    Item::Record(r) => {
                        let line = serde_json::json!({
                            "type": "log",
                            "ts_ns": r.ts_ns,
                            "ctx": r.ctx,
                            "ctx_name": name_of(r.ctx),
                            "level": r.site.level.as_str(),
                            "site": r.site.id,
                            "module": r.site.module,
                            "file": r.site.file,
                            "line": r.site.line,
                            "message": render::format(&r.site.fmt, &r.args),
                            "args": r.args,
                        });
                        writeln!(result.text, "{line}").unwrap();
                    }
                    Item::SpanOpen {
                        ts_ns,
                        id,
                        parent,
                        name,
                        ..
                    } => {
                        open.insert(*id, (*ts_ns, *parent, name));
                    }
                    Item::SpanClose { ts_ns, ctx, id } => {
                        if let Some((start, parent, name)) = open.remove(id) {
                            let line = serde_json::json!({
                                "type": "span", "ts_ns": start, "dur_ns": ts_ns - start, "ctx": ctx,
                                "ctx_name": name_of(*ctx), "name": name, "id": id, "parent": parent,
                            });
                            writeln!(result.text, "{line}").unwrap();
                        }
                    }
                    Item::Dropped { ts_ns, ctx, count } => {
                        let line = serde_json::json!({
                            "type": "dropped", "ts_ns": ts_ns, "ctx": ctx, "ctx_name": name_of(*ctx), "count": count,
                        });
                        writeln!(result.text, "{line}").unwrap();
                    }
                    Item::Gap {
                        ts_ns,
                        offset,
                        reason,
                        skipped,
                    } => {
                        let line = serde_json::json!({
                            "type": "gap", "ts_ns": ts_ns, "offset": offset, "reason": reason, "skipped": skipped,
                        });
                        writeln!(result.text, "{line}").unwrap();
                    }
                    Item::Crash { ts_ns, ctx, crash } => {
                        let line = serde_json::json!({
                            "type": "crash", "ts_ns": ts_ns, "ctx": ctx, "ctx_name": name_of(*ctx),
                            "kind": match crash.kind { 1 => "panic", 2 => "signal", 3 => "snapshot", _ => "unknown" },
                            "signal": crash.signal, "code": crash.code, "addr": crash.addr, "pc": crash.pc,
                            "message": crash.message, "description": crash.describe(),
                        });
                        writeln!(result.text, "{line}").unwrap();
                    }
                }
            }
        }
        Output::Text => {
            let mut r = TextRenderer::new(opts.color, opts.spans, mismatched, Some(t0));
            r.fields = opts.fields;
            r.names = names.clone();
            for item in &items {
                if let Some(line) = r.render(item) {
                    result.text.push_str(&line);
                    result.text.push('\n');
                }
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drop report has no time of its own. If one is the first thing a live
    /// stream shows, the lines after it must still count from their own first
    /// event, not from tick 0 (which printed absolute ticks).
    #[test]
    fn a_drop_report_first_does_not_set_the_time_origin() {
        let mut r = TextRenderer::new(false, true, false, None);
        let dropped = r
            .render(&Item::Dropped {
                ts_ns: 0,
                ctx: 3,
                count: 2,
            })
            .unwrap();
        assert!(dropped.starts_with("0.000000 "), "{dropped}");
        let crash = Crash {
            kind: 3,
            signal: 0,
            code: 0,
            addr: 0,
            pc: 0,
            message: "x".into(),
        };
        let line = r
            .render(&Item::Crash {
                ts_ns: 5_000_000_000,
                ctx: 3,
                crash,
            })
            .unwrap();
        assert!(
            line.starts_with("0.000000 "),
            "first real event is time zero: {line}"
        );
    }

    const SITE: &str = r#"{"id":7,"level":"info","module":"m","file":"f.rs","line":1,"column":1,"fmt":"x={}","nargs":1}"#;

    /// Header, a frame for unknown site 99, a resync frame, then `x=5` from site 7.
    fn stream_with_unknown_site(c: &Contract) -> Vec<u8> {
        let header = rtlog::wire::Header::new(c.hash(), 1_000_000_000).encode();
        let mut b = header.to_vec();
        b.extend(99u32.to_le_bytes());
        b.extend(rtlog::wire::CTRL_RESYNC.to_le_bytes());
        b.extend(header);
        b.extend(7u32.to_le_bytes());
        b.extend([10, 0, rtlog::arg::Tag::U8 as u8, 5]); // delta 10 ticks, ctx 0, u8 5
        b
    }

    #[test]
    fn unknown_site_before_a_resync_frame_is_a_gap() {
        let c = Contract::from_jsonl(SITE).unwrap();
        let bytes = stream_with_unknown_site(&c);
        let d = decode(&c, &bytes, Output::Text, false).unwrap();
        assert_eq!((d.gaps, d.skipped_bytes, d.records), (1, 4, 1));
        assert_eq!(d.error, None);
        let lines: Vec<&str> = d.text.lines().collect();
        assert!(
            lines[0].ends_with(
                "[stream] GAP   bad data at byte 17 (unknown site 0x00000063); 4 bytes skipped, resumed at the next resync frame"
            ),
            "{}",
            lines[0]
        );
        assert!(lines[1].ends_with("[ctx0] INFO  x=5"), "{}", lines[1]);
    }

    #[test]
    fn fields_parse_lists_and_refuse_unknown_names() {
        assert_eq!(Fields::parse("default"), Ok(Fields::DEFAULT));
        assert_eq!(Fields::parse("time, ctx ,level"), Ok(Fields::DEFAULT));
        assert_eq!(Fields::parse("all"), Ok(Fields::ALL));
        assert_eq!(Fields::parse("none"), Ok(Fields::NONE));
        assert_eq!(Fields::parse(""), Ok(Fields::NONE));
        assert_eq!(
            Fields::parse("default,module,line"),
            Ok(Fields {
                module: true,
                line: true,
                ..Fields::DEFAULT
            })
        );
        assert_eq!(
            Fields::parse("level,file"),
            Ok(Fields {
                level: true,
                file: true,
                ..Fields::NONE
            })
        );
        assert_eq!(Fields::parse("time,thread"), Err("thread".to_owned()));
        assert_eq!(Fields::default(), Fields::DEFAULT);
    }

    /// The record line of the two-frame stream, shown with `fields`.
    fn line_with(fields: &str, color: bool) -> String {
        let c = Contract::from_jsonl(SITE).unwrap();
        let bytes = stream_with_unknown_site(&c);
        let opts = Options {
            fields: Fields::parse(fields).unwrap(),
            color,
            ..Options::default()
        };
        let text = decode_with(&c, &bytes, &opts).unwrap().text;
        text.lines().nth(1).unwrap().to_owned()
    }

    #[test]
    fn a_line_shows_the_fields_asked_for_in_a_fixed_order() {
        assert_eq!(line_with("default", false), "0.000000 [ctx0] INFO  x=5");
        assert_eq!(
            line_with("all", false),
            "0.000000 [ctx0] INFO  m f.rs:1: x=5"
        );
        assert_eq!(
            line_with("line,module,default", false),
            line_with("all", false)
        );
        assert_eq!(
            line_with("default,module", false),
            "0.000000 [ctx0] INFO  m x=5"
        );
        assert_eq!(
            line_with("default,file", false),
            "0.000000 [ctx0] INFO  f.rs: x=5"
        );
        assert_eq!(line_with("level,line", false), "INFO  f.rs:1: x=5");
        assert_eq!(line_with("time", false), "0.000000 x=5");
        assert_eq!(line_with("ctx", false), "[ctx0] x=5");
        assert_eq!(line_with("none", false), "x=5");
        // The origin is dimmed as one piece.
        assert_eq!(
            line_with("module,line", true),
            format!("{DIM}m f.rs:1: {RESET}x=5")
        );
    }

    #[test]
    fn the_fields_apply_to_every_kind_of_line() {
        let c = Contract::from_jsonl(SITE).unwrap();
        let bytes = stream_with_unknown_site(&c);
        let opts = Options {
            fields: Fields::NONE,
            ..Options::default()
        };
        let text = decode_with(&c, &bytes, &opts).unwrap().text;
        assert!(text.starts_with("bad data at byte 17 "), "{text}");
        let mut r = TextRenderer::new(false, true, false, Some(0));
        r.fields = Fields::parse("level").unwrap();
        let open = Item::SpanOpen {
            ts_ns: 0,
            ctx: 1,
            id: 5,
            parent: 0,
            name: "work",
        };
        assert_eq!(r.render(&open).unwrap(), "SPAN  ▶ work");
        let dropped = Item::Dropped {
            ts_ns: 0,
            ctx: 1,
            count: 2,
        };
        assert_eq!(r.render(&dropped).unwrap(), "DROP  2 frames dropped");
    }

    #[test]
    fn colour_wraps_each_field_in_ansi_codes() {
        let c = Contract::from_jsonl(SITE).unwrap();
        let bytes = stream_with_unknown_site(&c);
        let opts = Options {
            color: true,
            ..Options::default()
        };
        let text = decode_with(&c, &bytes, &opts).unwrap().text;
        let record = text.lines().nth(1).unwrap();
        assert_eq!(
            record,
            format!("{DIM}0.000000{RESET} {CYAN}[ctx0]{RESET} \x1b[32mINFO {RESET} x=5")
        );
        assert!(text.contains(&format!("{RED_BOLD}GAP  {RESET}")), "{text}");
    }

    #[test]
    fn span_lines_are_hidden_when_spans_are_off() {
        let mut r = TextRenderer::new(false, false, false, Some(0));
        let open = Item::SpanOpen {
            ts_ns: 0,
            ctx: 1,
            id: 5,
            parent: 0,
            name: "work",
        };
        let close = Item::SpanClose {
            ts_ns: 2_000,
            ctx: 1,
            id: 5,
        };
        assert_eq!(r.render(&open), None);
        assert_eq!(r.render(&close), None);
        // Still tracked while hidden: nesting depth is back to 0, nothing left open.
        assert_eq!(r.open.len(), 0);
        assert_eq!(r.depth.get(&1), Some(&0));

        let mut on = TextRenderer::new(false, true, false, Some(0));
        assert!(on.render(&open).unwrap().ends_with("SPAN  ▶ work"));
        assert!(on.render(&close).unwrap().ends_with("SPAN  ◀ work 2000 ns"));
    }

    #[test]
    fn human_durations() {
        assert_eq!(human_ns(950), "950 ns");
        assert_eq!(human_ns(12_340), "12.3 µs");
        assert_eq!(human_ns(12_340_000), "12.34 ms");
        assert_eq!(human_ns(12_340_000_000), "12.340 s");
    }
}
