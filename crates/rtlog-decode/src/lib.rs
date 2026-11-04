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
mod perfetto;
pub mod render;
pub mod stream;
pub mod value;

use std::collections::HashMap;
use std::fmt::Write as _;

pub use contract::{Contract, ContractError, Kind, SiteInfo};
pub use stream::{DecodeError, Decoder, Event, Record, Session};
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
}

impl Item<'_> {
    pub(crate) fn ts(&self) -> u64 {
        match self {
            Item::Record(r) => r.ts_ns,
            Item::SpanOpen { ts_ns, .. }
            | Item::SpanClose { ts_ns, .. }
            | Item::Dropped { ts_ns, .. } => *ts_ns,
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
    /// Set when decoding stopped early; the output holds what came before.
    pub error: Option<DecodeError>,
}

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const MAGENTA: &str = "\x1b[35m";

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
    for ev in dec.by_ref() {
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
            Ok(Event::CtxClose { .. }) => {}
            Err(e) => result.error = Some(e),
        }
    }
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
        .filter(|i| !matches!(i, Item::Dropped { .. }))
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
                }
            }
        }
        Output::Text => {
            let p = Painter { on: opts.color };
            let mark = if mismatched { "?" } else { "" };
            let mut depth: HashMap<u32, usize> = HashMap::new();
            let mut open: HashMap<u64, (u64, &str)> = HashMap::new();
            for item in &items {
                match item {
                    Item::Record(r) => {
                        let d = depth.get(&r.ctx).copied().unwrap_or(0);
                        let level = format!("{:<5}", r.site.level.as_str().to_ascii_uppercase());
                        writeln!(
                            result.text,
                            "{} {} {} {}{mark}{}",
                            p.paint(DIM, &stamp(r.ts_ns, t0)),
                            p.paint(CYAN, &format!("[{}]", name_of(r.ctx))),
                            p.paint(level_color(r.site.level), &level),
                            "  ".repeat(d),
                            render::format(&r.site.fmt, &r.args),
                        )
                        .unwrap();
                    }
                    Item::SpanOpen {
                        ts_ns,
                        ctx,
                        id,
                        name,
                        ..
                    } => {
                        let d = depth.entry(*ctx).or_insert(0);
                        open.insert(*id, (*ts_ns, name));
                        if opts.spans {
                            writeln!(
                                result.text,
                                "{} {} {} {}{}",
                                p.paint(DIM, &stamp(*ts_ns, t0)),
                                p.paint(CYAN, &format!("[{}]", name_of(*ctx))),
                                p.paint(MAGENTA, "SPAN "),
                                "  ".repeat(*d),
                                p.paint(MAGENTA, &format!("▶ {name}")),
                            )
                            .unwrap();
                        }
                        *d += 1;
                    }
                    Item::SpanClose { ts_ns, ctx, id } => {
                        let Some((start, name)) = open.remove(id) else {
                            continue;
                        };
                        let d = depth.entry(*ctx).or_insert(1);
                        *d = d.saturating_sub(1);
                        if opts.spans {
                            writeln!(
                                result.text,
                                "{} {} {} {}{} {}",
                                p.paint(DIM, &stamp(*ts_ns, t0)),
                                p.paint(CYAN, &format!("[{}]", name_of(*ctx))),
                                p.paint(MAGENTA, "SPAN "),
                                "  ".repeat(*d),
                                p.paint(MAGENTA, &format!("◀ {name}")),
                                p.paint(DIM, &human_ns(ts_ns - start)),
                            )
                            .unwrap();
                        }
                    }
                    Item::Dropped { ts_ns, ctx, count } => {
                        writeln!(
                            result.text,
                            "{} {} {} {}",
                            p.paint(DIM, &stamp(*ts_ns, t0)),
                            p.paint(CYAN, &format!("[{}]", name_of(*ctx))),
                            p.paint("\x1b[1;31m", "DROP "),
                            p.paint("\x1b[1;31m", &format!("{count} frames dropped")),
                        )
                        .unwrap();
                    }
                }
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_durations() {
        assert_eq!(human_ns(950), "950 ns");
        assert_eq!(human_ns(12_340), "12.3 µs");
        assert_eq!(human_ns(12_340_000), "12.34 ms");
        assert_eq!(human_ns(12_340_000_000), "12.340 s");
    }
}
