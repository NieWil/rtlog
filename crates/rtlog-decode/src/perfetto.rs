//! Export to the Chrome trace-event JSON format, which Perfetto
//! (<https://ui.perfetto.dev>), `chrome://tracing` and Speedscope open directly.
//!
//! Mapping (spec section 10): each context is a thread track, spans are slices,
//! log records are instant events carrying their typed arguments, numeric
//! arguments named in the format string (`err={}`) become counter tracks, and
//! drop reports become instant events plus a `dropped` counter.

use std::collections::HashMap;

use serde_json::{json, Map, Value as Json};

use crate::render;
use crate::value::Value;
use crate::Item;

fn us(ts_ns: u64, t0: u64) -> f64 {
    ts_ns.saturating_sub(t0) as f64 / 1_000.0
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::U(n) => Some(*n as f64),
        Value::I(n) => Some(*n as f64),
        Value::F(f) if f.is_finite() => Some(*f),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

pub(crate) fn export(items: &[Item<'_>], names: &HashMap<u32, String>, clock_hz: u32) -> String {
    let t0 = items.iter().map(Item::ts).min().unwrap_or(0);
    let mut ev: Vec<Json> = Vec::with_capacity(items.len() * 2 + names.len() + 1);
    ev.push(json!({"ph": "M", "name": "process_name", "pid": 1, "args": {"name": "rtlog"}}));
    let mut ctxs: Vec<_> = names.iter().collect();
    ctxs.sort();
    for (ctx, name) in ctxs {
        ev.push(
            json!({"ph": "M", "name": "thread_name", "pid": 1, "tid": ctx, "args": {"name": name}}),
        );
        ev.push(json!({"ph": "M", "name": "thread_sort_index", "pid": 1, "tid": ctx, "args": {"sort_index": ctx}}));
    }
    let ctx_name = |ctx: u32| {
        names
            .get(&ctx)
            .cloned()
            .unwrap_or_else(|| format!("ctx{ctx}"))
    };
    // Open spans by ID; `E` events repeat the name so every viewer pairs them.
    let mut open: HashMap<u64, &str> = HashMap::new();
    let mut arg_names: HashMap<u32, Vec<Option<String>>> = HashMap::new();
    let mut drops_total = 0u64;
    let mut last_ts = t0;

    for item in items {
        last_ts = last_ts.max(item.ts());
        match item {
            Item::Record(r) => {
                let ts = us(r.ts_ns, t0);
                let keys = arg_names
                    .entry(r.site.id)
                    .or_insert_with(|| render::arg_names(&r.site.fmt));
                let mut args = Map::new();
                args.insert("level".into(), json!(r.site.level.as_str()));
                args.insert(
                    "site".into(),
                    json!(format!("{}:{}", r.site.file, r.site.line)),
                );
                for (i, v) in r.args.iter().enumerate() {
                    let key = keys
                        .get(i)
                        .cloned()
                        .flatten()
                        .unwrap_or_else(|| format!("arg{i}"));
                    if let Some(n) = number(v) {
                        if keys.get(i).is_some_and(Option::is_some) {
                            ev.push(json!({
                                "ph": "C", "pid": 1, "ts": ts,
                                "name": format!("{}/{}", ctx_name(r.ctx), key),
                                "args": {key.clone(): n},
                            }));
                        }
                    }
                    args.insert(key, serde_json::to_value(v).unwrap_or(Json::Null));
                }
                ev.push(json!({
                    "ph": "i", "s": "t", "pid": 1, "tid": r.ctx, "ts": ts,
                    "name": render::format(&r.site.fmt, &r.args),
                    "cat": r.site.level.as_str(),
                    "args": args,
                }));
            }
            Item::SpanOpen {
                ts_ns,
                ctx,
                id,
                name,
                ..
            } => {
                open.insert(*id, name);
                ev.push(json!({"ph": "B", "pid": 1, "tid": ctx, "ts": us(*ts_ns, t0), "name": name, "cat": "span"}));
            }
            Item::SpanClose { ts_ns, ctx, id } => {
                if let Some(name) = open.remove(id) {
                    ev.push(json!({"ph": "E", "pid": 1, "tid": ctx, "ts": us(*ts_ns, t0), "name": name, "cat": "span"}));
                }
            }
            Item::Dropped { ts_ns, ctx, count } => {
                drops_total += u64::from(*count);
                let ts = us(*ts_ns, t0);
                ev.push(json!({
                    "ph": "i", "s": "t", "pid": 1, "tid": ctx, "ts": ts, "cat": "rtlog",
                    "name": format!("dropped {count} frames"),
                }));
                ev.push(json!({"ph": "C", "pid": 1, "ts": ts, "name": "dropped", "args": {"dropped": drops_total}}));
            }
        }
    }
    // Close spans still open at the end of the capture so viewers nest correctly.
    for item in items.iter().rev() {
        if let Item::SpanOpen { ctx, id, .. } = item {
            if let Some(name) = open.remove(id) {
                ev.push(json!({
                    "ph": "E", "pid": 1, "tid": ctx, "ts": us(last_ts, t0), "name": name, "cat": "span",
                    "args": {"unterminated": true},
                }));
            }
        }
    }
    json!({
        "traceEvents": ev,
        "displayTimeUnit": "ns",
        "otherData": {"source": "rtlog", "clock_hz": clock_hz},
    })
    .to_string()
}
