//! Host decoder for rtlog: joins a stream with its contract and renders text or
//! JSON lines (spec section 9).
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
pub mod render;
pub mod stream;
pub mod value;

use std::collections::HashMap;

pub use contract::{Contract, ContractError, SiteInfo};
pub use stream::{DecodeError, Decoder, Event, Record};
pub use value::Value;

/// Output format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// `12.345678 [motor] INFO  pid out=0.42 err=-3`
    Text,
    /// One JSON object per record.
    Json,
}

/// Render a record as one text line (no newline); time is shown relative to `t0_ns`.
pub fn text_line(r: &Record<'_>, ctx_name: &str, mismatched: bool, t0_ns: u64) -> String {
    let rel = r.ts_ns.saturating_sub(t0_ns);
    let secs = rel / 1_000_000_000;
    let micros = (rel % 1_000_000_000) / 1_000;
    let level = r.site.level.as_str().to_ascii_uppercase();
    let mark = if mismatched { "?" } else { "" };
    format!(
        "{secs}.{micros:06} [{ctx_name}] {level:<5} {mark}{}",
        render::format(&r.site.fmt, &r.args)
    )
}

/// Render a record as one JSON object.
pub fn json_line(r: &Record<'_>, ctx_name: &str) -> serde_json::Value {
    serde_json::json!({
        "ts_ns": r.ts_ns,
        "ctx": r.ctx,
        "ctx_name": ctx_name,
        "level": r.site.level.as_str(),
        "site": r.site.id,
        "module": r.site.module,
        "file": r.site.file,
        "line": r.site.line,
        "message": render::format(&r.site.fmt, &r.args),
        "args": r.args,
    })
}

/// Result of decoding a whole stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decoded {
    /// Rendered output, one line per record, sorted by timestamp. Text lines
    /// show seconds since the first record; JSON keeps absolute `ts_ns`.
    pub text: String,
    pub records: usize,
    pub dropped: u64,
    pub contexts: usize,
    /// Set when decoding stopped early; the output holds what came before.
    pub error: Option<DecodeError>,
}

/// Decode a whole stream; records are merged across contexts by timestamp.
///
/// Header and contract errors are returned as `Err`; a bad frame later in the
/// stream ends decoding and is reported in [`Decoded::error`].
pub fn decode(
    contract: &Contract,
    bytes: &[u8],
    out: Output,
    force: bool,
) -> Result<Decoded, DecodeError> {
    let mut dec = Decoder::new(contract, bytes, force)?;
    let mismatched = dec.mismatched;
    let mut names: HashMap<u32, String> = HashMap::new();
    let mut records = Vec::new();
    let mut result = Decoded::default();
    for ev in dec.by_ref() {
        match ev {
            Ok(Event::Record(r)) => records.push(r),
            Ok(Event::CtxRegister { ctx, name }) => {
                names.insert(ctx, name);
            }
            Ok(Event::Dropped { count, .. }) => result.dropped += u64::from(count),
            Ok(Event::CtxClose { .. }) => {}
            Err(e) => result.error = Some(e),
        }
    }
    records.sort_by_key(|r| r.ts_ns);
    let t0 = records.first().map_or(0, |r| r.ts_ns);
    for r in &records {
        let fallback = format!("ctx{}", r.ctx);
        let name = names.get(&r.ctx).map(String::as_str).unwrap_or(&fallback);
        match out {
            Output::Text => result.text.push_str(&text_line(r, name, mismatched, t0)),
            Output::Json => result.text.push_str(&json_line(r, name).to_string()),
        }
        result.text.push('\n');
    }
    result.records = records.len();
    result.contexts = names.len();
    Ok(result)
}
