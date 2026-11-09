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
            Item::Crash { ts_ns, ctx, crash } => {
                let ts = us(*ts_ns, t0);
                let name = format!(
                    "{} {}",
                    if crash.kind == 3 { "SNAPSHOT" } else { "CRASH" },
                    crash.describe()
                );
                // On the crashing thread's track, and as a global marker across all tracks.
                ev.push(json!({"ph": "i", "s": "t", "pid": 1, "tid": ctx, "ts": ts, "cat": "crash", "name": name}));
                ev.push(json!({"ph": "i", "s": "g", "pid": 1, "tid": ctx, "ts": ts, "cat": "crash", "name": name}));
            }
            Item::Gap {
                ts_ns,
                offset,
                skipped,
                ..
            } => {
                ev.push(json!({
                    "ph": "i", "s": "g", "pid": 1, "tid": 0, "ts": us(*ts_ns, t0), "cat": "rtlog",
                    "name": format!("stream gap: {skipped} bytes skipped at byte {offset}"),
                }));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Kind, SiteInfo};
    use crate::stream::{Crash, Record};

    fn site(fmt: &str, nargs: u8) -> SiteInfo {
        SiteInfo {
            id: 1,
            kind: Kind::Log,
            level: rtlog::Level::Warn,
            module: "m".into(),
            file: "f.rs".into(),
            line: 9,
            column: 1,
            fmt: fmt.into(),
            nargs,
        }
    }

    fn crash(kind: u8, message: &str) -> Crash {
        Crash {
            kind,
            signal: if kind == 2 { 11 } else { 0 },
            code: 1,
            addr: 0x10,
            pc: 0,
            message: message.into(),
        }
    }

    fn trace(items: &[Item<'_>], names: &HashMap<u32, String>) -> Vec<Json> {
        let t: Json = serde_json::from_str(&export(items, names, 1_000)).unwrap();
        assert_eq!(t["otherData"]["clock_hz"], 1_000);
        t["traceEvents"].as_array().unwrap().clone()
    }

    fn named<'a>(ev: &'a [Json], ph: &str, name: &str) -> Vec<&'a Json> {
        ev.iter()
            .filter(|e| e["ph"] == ph && e["name"] == name)
            .collect()
    }

    #[test]
    fn named_numeric_args_become_counters_and_others_do_not() {
        let s = site("a={} {} b={} c={} s={}", 5);
        let rec = Item::Record(Record {
            ts_ns: 2_000,
            ctx: 0,
            site: &s,
            args: vec![
                Value::I(-3),
                Value::U(9),
                Value::F(f64::NAN),
                Value::Bool(true),
                Value::Str {
                    s: "x".into(),
                    truncated: false,
                },
            ],
        });
        let ev = trace(&[rec], &HashMap::new());
        let counters: Vec<_> = ev.iter().filter(|e| e["ph"] == "C").collect();
        // Signed and bool counters; none for the unnamed `{}`, the NaN or the string.
        assert_eq!(counters.len(), 2, "{counters:?}");
        assert_eq!(named(&ev, "C", "ctx0/a")[0]["args"]["a"], -3.0);
        assert_eq!(named(&ev, "C", "ctx0/c")[0]["args"]["c"], 1.0);
        let i = named(&ev, "i", "a=-3 9 b=NaN c=true s=x")[0];
        assert_eq!(i["cat"], "warn");
        assert_eq!(i["args"]["site"], "f.rs:9");
        assert_eq!(i["args"]["arg1"], 9);
        assert_eq!(i["args"]["b"], Json::Null);
        assert_eq!(i["args"]["s"], json!({"s": "x", "truncated": false}));
    }

    #[test]
    fn float_args_are_counters_on_the_named_thread() {
        let s = site("t={:.1}", 1);
        let rec = Item::Record(Record {
            ts_ns: 0,
            ctx: 4,
            site: &s,
            args: vec![Value::F(21.5)],
        });
        let names = HashMap::from([(4, "sensor".to_owned())]);
        let ev = trace(&[rec], &names);
        assert_eq!(named(&ev, "C", "sensor/t")[0]["args"]["t"], 21.5);
        let meta = named(&ev, "M", "thread_name");
        assert_eq!(meta[0]["tid"], 4);
        assert_eq!(meta[0]["args"]["name"], "sensor");
    }

    #[test]
    fn spans_never_closed_end_at_the_last_event() {
        let items = [
            Item::SpanOpen {
                ts_ns: 1_000,
                ctx: 1,
                id: 10,
                parent: 0,
                name: "outer",
            },
            Item::SpanOpen {
                ts_ns: 2_000,
                ctx: 1,
                id: 11,
                parent: 10,
                name: "inner",
            },
            // A close for a span this capture never saw open is dropped.
            Item::SpanClose {
                ts_ns: 3_000,
                ctx: 1,
                id: 99,
            },
            Item::SpanClose {
                ts_ns: 4_000,
                ctx: 1,
                id: 11,
            },
            Item::Dropped {
                ts_ns: 9_000,
                ctx: 1,
                count: 1,
            },
        ];
        let ev = trace(&items, &HashMap::new());
        let ends: Vec<_> = ev.iter().filter(|e| e["ph"] == "E").collect();
        assert_eq!(ends.len(), 2, "{ends:?}");
        assert_eq!(ends[0]["name"], "inner");
        assert_eq!(ends[0]["ts"], 3.0);
        assert_eq!(ends[0].get("args"), None);
        assert_eq!(ends[1]["name"], "outer");
        assert_eq!(
            ends[1]["ts"], 8.0,
            "closed at the last event, 8 µs after t0"
        );
        assert_eq!(ends[1]["args"]["unterminated"], true);
    }

    #[test]
    fn drops_are_instants_and_a_running_counter() {
        let items = [
            Item::Dropped {
                ts_ns: 0,
                ctx: 2,
                count: 2,
            },
            Item::Dropped {
                ts_ns: 5_000,
                ctx: 3,
                count: 3,
            },
        ];
        let ev = trace(&items, &HashMap::new());
        let i = named(&ev, "i", "dropped 3 frames");
        assert_eq!(i[0]["tid"], 3);
        assert_eq!(i[0]["ts"], 5.0);
        assert_eq!(named(&ev, "i", "dropped 2 frames").len(), 1);
        let totals: Vec<_> = named(&ev, "C", "dropped")
            .iter()
            .map(|c| c["args"]["dropped"].as_u64().unwrap())
            .collect();
        assert_eq!(totals, [2, 5]);
    }

    #[test]
    fn crashes_are_marked_on_their_track_and_globally() {
        let items = [
            Item::Crash {
                ts_ns: 1_000,
                ctx: 5,
                crash: crash(2, "SIGSEGV"),
            },
            Item::Crash {
                ts_ns: 2_000,
                ctx: 6,
                crash: crash(3, "on demand"),
            },
        ];
        let ev = trace(&items, &HashMap::new());
        let segv = named(&ev, "i", "CRASH SIGSEGV (signal 11, code 1) addr=0x10");
        let scopes: Vec<_> = segv.iter().map(|e| e["s"].as_str().unwrap()).collect();
        assert_eq!(scopes, ["t", "g"]);
        assert!(segv.iter().all(|e| e["cat"] == "crash" && e["tid"] == 5));
        assert_eq!(named(&ev, "i", "SNAPSHOT snapshot: on demand").len(), 2);
    }

    #[test]
    fn gaps_are_global_instants() {
        let items = [Item::Gap {
            ts_ns: 7_000,
            offset: 40,
            reason: "bad".into(),
            skipped: 10,
        }];
        let ev = trace(&items, &HashMap::new());
        let g = named(&ev, "i", "stream gap: 10 bytes skipped at byte 40");
        assert_eq!(g[0]["s"], "g");
        assert_eq!(g[0]["cat"], "rtlog");
        assert_eq!(g[0]["ts"], 0.0);
    }
}
