//! Host decoder for rtlog: joins a stream with its contract and renders text,
//! JSON lines or a Perfetto / Chrome trace (spec sections 9 and 10).
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

pub mod contract;
pub mod elf;
mod perfetto;
pub mod render;
pub mod stdout;
pub mod stream;
pub mod value;

use std::collections::HashMap;
use std::fmt::Write as _;

pub use contract::{Contract, ContractError, Kind, SiteInfo};
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
}

/// Decoding options.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub output: Output,
    /// Decode even if the stream's contract hash does not match.
    pub force: bool,
    /// ANSI colours in text output.
    pub color: bool,
    /// Show span open/close lines in text output.
    pub spans: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            output: Output::Text,
            force: false,
            color: false,
            spans: true,
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
    pub records: usize,
    pub spans: usize,
    pub dropped: u64,
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

    /// `<seconds> [<label>]`; see [`Self::prefix_with`] for `anchor`.
    fn labelled(&mut self, ts_ns: u64, label: &str, anchor: bool) -> String {
        let t0 = match self.t0 {
            Some(t) => t,
            None if anchor => *self.t0.get_or_insert(ts_ns),
            None => ts_ns,
        };
        format!(
            "{} {}",
            self.paint.paint(DIM, &stamp(ts_ns, t0)),
            self.paint.paint(CYAN, &format!("[{label}]"))
        )
    }

    /// The text line for `item`, if it has one.
    pub(crate) fn render(&mut self, item: &Item<'_>) -> Option<String> {
        let p = Painter { on: self.paint.on };
        Some(match item {
            Item::Record(r) => {
                let d = self.depth.get(&r.ctx).copied().unwrap_or(0);
                let level = format!("{:<5}", r.site.level.as_str().to_ascii_uppercase());
                format!(
                    "{} {} {}{}{}",
                    self.prefix(r.ts_ns, r.ctx),
                    p.paint(level_color(r.site.level), &level),
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
                    "{prefix} {} {indent}{}",
                    p.paint(MAGENTA, "SPAN "),
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
                    "{} {} {indent}{} {}",
                    self.prefix(*ts_ns, *ctx),
                    p.paint(MAGENTA, "SPAN "),
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
                "{} {} {}",
                self.labelled(*ts_ns, "stream", false),
                p.paint(RED_BOLD, "GAP  "),
                p.paint(
                    RED_BOLD,
                    &format!("bad data at byte {offset} ({reason}); {skipped} bytes skipped, resumed at the next resync frame")
                ),
            ),
            Item::Dropped { ts_ns, ctx, count } => format!(
                "{} {} {}",
                self.prefix_with(*ts_ns, *ctx, false),
                p.paint(RED_BOLD, "DROP "),
                p.paint(RED_BOLD, &format!("{count} frames dropped")),
            ),
            Item::Crash { ts_ns, ctx, crash } => format!(
                "{} {} {}",
                self.prefix(*ts_ns, *ctx),
                p.paint(RED_BOLD, if crash.kind == 3 { "SNAP " } else { "CRASH" }),
                p.paint(RED_BOLD, &crash.describe()),
            ),
        })
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
/// Header and contract errors are returned as `Err`; a bad frame later in the
/// stream ends decoding and is reported in [`Decoded::error`].
pub fn decode_with(
    contract: &Contract,
    bytes: &[u8],
    opts: &Options,
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

    #[test]
    fn human_durations() {
        assert_eq!(human_ns(950), "950 ns");
        assert_eq!(human_ns(12_340), "12.3 µs");
        assert_eq!(human_ns(12_340_000), "12.34 ms");
        assert_eq!(human_ns(12_340_000_000), "12.340 s");
    }
}
