//! OpenTelemetry exporter: a decoded capture as [OTLP] log records in the
//! JSON encoding, one `LogsData` object per line with up to
//! [`BATCH`] records each. This is the file format the OpenTelemetry
//! Collector's `otlpjsonfile` receiver reads; the same objects are the body
//! of an OTLP/HTTP request to `/v1/logs`.
//!
//! One log record per rtlog record, drop report, stream gap and crash:
//!
//! | OTLP field | Value |
//! | --- | --- |
//! | `timeUnixNano`, `observedTimeUnixNano` | wall-clock time of the record, when [`Config::epoch_ns`] or [`Config::start_ns`] is given; left out otherwise |
//! | `severityNumber`, `severityText` | from the level, see [`severity`] |
//! | `body` | the formatted message |
//! | `eventName` | `rtlog.dropped`, `rtlog.gap`, `rtlog.crash` or `rtlog.snapshot` on those records |
//! | attributes | `code.file.path`, `code.line.number`, `thread.name`, `thread.id` (the context ID), `rtlog.site`, `rtlog.module`, `rtlog.template` (the format string), `rtlog.ts_ns` (time on the target's clock) and one typed `rtlog.arg.<n>` per argument |
//! | resource | `service.name` ([`Config::service_name`]), `host.name` if given |
//! | scope | `rtlog`, with the decoder's version |
//!
//! Spans are not exported yet: they belong in OTLP traces, not logs.
//!
//! [OTLP]: https://opentelemetry.io/docs/specs/otlp/

use std::collections::HashMap;

use rtlog::Level;
use serde_json::{json, Value as Json};

use crate::{render, Contract, DecodeError, Decoded, Item, Options, Value};

/// Log records per output line.
pub const BATCH: usize = 1000;

/// How the OTLP records are filled in; see the [module documentation](self).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The `service.name` resource attribute: the program that logged.
    /// Default `rtlog`.
    pub service_name: String,
    /// The `host.name` resource attribute: the machine the capture came
    /// from. Left out when empty (the default).
    pub host_name: String,
    /// Wall-clock time of the target's time zero, in nanoseconds since
    /// 1970-01-01T00:00:00Z.
    pub epoch_ns: Option<u64>,
    /// Wall-clock time of the first event of the capture, for when the
    /// target's time zero is not known. Used when `epoch_ns` is `None`. With
    /// neither, the records carry no time.
    pub start_ns: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            service_name: "rtlog".to_owned(),
            host_name: String::new(),
            epoch_ns: None,
            start_ns: None,
        }
    }
}

/// The OpenTelemetry severity number and text of a level: `trace` 1,
/// `debug` 5, `info` 9, `warn` 13, `error` 17.
pub fn severity(level: Level) -> (u8, &'static str) {
    match level {
        Level::Trace => (1, "TRACE"),
        Level::Debug => (5, "DEBUG"),
        Level::Info => (9, "INFO"),
        Level::Warn => (13, "WARN"),
        Level::Error => (17, "ERROR"),
    }
}

/// Standard base64 with padding: how OTLP JSON writes bytes.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// An argument as an OTLP `AnyValue`. 64-bit integers are strings in OTLP
/// JSON; an unsigned value above `i64::MAX` does not fit `intValue` and is
/// written as a string.
fn any_value(v: &Value) -> Json {
    match v {
        Value::U(u) => match i64::try_from(*u) {
            Ok(i) => json!({ "intValue": i.to_string() }),
            Err(_) => json!({ "stringValue": u.to_string() }),
        },
        Value::I(i) => json!({ "intValue": i.to_string() }),
        Value::F(f) if f.is_finite() => json!({ "doubleValue": f }),
        // JSON has no NaN or infinity; OTLP JSON writes them as strings.
        Value::F(f) => json!({ "doubleValue": f.to_string() }),
        Value::Bool(b) => json!({ "boolValue": b }),
        Value::Char(c) => json!({ "stringValue": c.to_string() }),
        Value::Str { s, truncated } => {
            json!({ "stringValue": if *truncated { format!("{s}…") } else { s.clone() } })
        }
        Value::Bytes { bytes, .. } => json!({ "bytesValue": base64(bytes) }),
        Value::Option(None) => json!({}),
        Value::Option(Some(inner)) => any_value(inner),
        Value::Array(items) => {
            json!({ "arrayValue": { "values": items.iter().map(any_value).collect::<Vec<_>>() } })
        }
    }
}

fn attr(key: &str, value: Json) -> Json {
    json!({ "key": key, "value": value })
}

fn text(key: &str, s: &str) -> Json {
    attr(key, json!({ "stringValue": s }))
}

fn int(key: &str, n: u64) -> Json {
    attr(key, json!({ "intValue": n.to_string() }))
}

/// Render sorted timeline items as OTLP JSON lines.
pub(crate) fn export(
    items: &[Item<'_>],
    names: &HashMap<u32, String>,
    mismatched: bool,
    cfg: &Config,
) -> String {
    let clock = crate::WallClock::new(items, cfg.epoch_ns, cfg.start_ns);
    let thread = |attrs: &mut Vec<Json>, ctx: u32| {
        let name = names
            .get(&ctx)
            .cloned()
            .unwrap_or_else(|| format!("ctx{ctx}"));
        attrs.push(text("thread.name", &name));
        attrs.push(int("thread.id", u64::from(ctx)));
    };
    let mut records = Vec::new();
    for item in items {
        // (severity, body, event name, attributes, context)
        let (ts_ns, (number, label), body, event, mut attrs) = match item {
            Item::Record(r) => {
                let site = r.site;
                let mut attrs = vec![
                    text("code.file.path", &site.file),
                    int("code.line.number", u64::from(site.line)),
                    text("rtlog.site", &format!("{:08x}", site.id)),
                    text("rtlog.template", &site.fmt),
                ];
                if !site.module.is_empty() {
                    attrs.push(text("rtlog.module", &site.module));
                }
                thread(&mut attrs, r.ctx);
                for (i, a) in r.args.iter().enumerate() {
                    attrs.push(attr(&format!("rtlog.arg.{i}"), any_value(a)));
                }
                (
                    r.ts_ns,
                    severity(site.level),
                    render::format(&site.fmt, &r.args),
                    None,
                    attrs,
                )
            }
            Item::SpanOpen { .. } | Item::SpanClose { .. } => continue,
            Item::Dropped { ts_ns, ctx, count } => {
                let mut attrs = vec![int("rtlog.dropped", u64::from(*count))];
                thread(&mut attrs, *ctx);
                (
                    *ts_ns,
                    severity(Level::Warn),
                    format!("{count} frames dropped"),
                    Some("rtlog.dropped"),
                    attrs,
                )
            }
            Item::Gap {
                ts_ns,
                offset,
                reason,
                skipped,
            } => (
                *ts_ns,
                severity(Level::Error),
                format!("bad data at byte {offset} ({reason}); {skipped} bytes skipped, resumed at the next resync frame"),
                Some("rtlog.gap"),
                vec![
                    int("rtlog.gap.offset", *offset as u64),
                    int("rtlog.gap.skipped", *skipped as u64),
                ],
            ),
            Item::Crash { ts_ns, ctx, crash } => {
                let mut attrs = Vec::new();
                thread(&mut attrs, *ctx);
                // A snapshot is informational; a panic or a fatal signal is fatal.
                let (sev, event) = if crash.kind == 3 {
                    (severity(Level::Info), "rtlog.snapshot")
                } else {
                    ((21, "FATAL"), "rtlog.crash")
                };
                (*ts_ns, sev, crash.describe(), Some(event), attrs)
            }
        };
        attrs.push(int("rtlog.ts_ns", ts_ns));
        let mut record = json!({
            "severityNumber": number,
            "severityText": label,
            "body": { "stringValue": body },
            "attributes": attrs,
        });
        if let Some(c) = clock {
            let t = c.at(ts_ns).to_string();
            record["timeUnixNano"] = json!(t);
            record["observedTimeUnixNano"] = json!(t);
        }
        if let Some(name) = event {
            record["eventName"] = json!(name);
        }
        records.push(record);
    }
    let mut resource = vec![text("service.name", &cfg.service_name)];
    if !cfg.host_name.is_empty() {
        resource.push(text("host.name", &cfg.host_name));
    }
    if mismatched {
        resource.push(text("rtlog.contract", "mismatch"));
    }
    let mut out = String::new();
    for batch in records.chunks(BATCH) {
        let line = json!({
            "resourceLogs": [{
                "resource": { "attributes": resource },
                "scopeLogs": [{
                    "scope": { "name": "rtlog", "version": env!("CARGO_PKG_VERSION") },
                    "logRecords": batch,
                }],
            }],
        });
        out.push_str(&line.to_string());
        out.push('\n');
    }
    out
}

/// Decode a whole stream to OTLP JSON lines with `cfg`; as
/// [`decode_with`](crate::decode_with), whose `Output::OtlpJson` uses
/// [`Config::default`]. `opts.output`, `opts.color` and `opts.spans` are not
/// used.
pub fn decode(
    contract: &Contract,
    bytes: &[u8],
    opts: &Options,
    cfg: &Config,
) -> Result<Decoded, DecodeError> {
    let opts = Options {
        output: crate::Output::OtlpJson,
        ..*opts
    };
    crate::decode_inner(
        contract,
        bytes,
        &opts,
        &crate::syslog::Config::default(),
        cfg,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads() {
        for (bytes, want) in [
            (&b""[..], ""),
            (b"a", "YQ=="),
            (b"ab", "YWI="),
            (b"abc", "YWJj"),
            (b"abcd", "YWJjZA=="),
            (b"\xff\xfe\xfd", "//79"),
        ] {
            assert_eq!(base64(bytes), want);
        }
    }

    #[test]
    fn every_argument_type_has_an_any_value() {
        let cases = [
            (Value::U(7), json!({"intValue": "7"})),
            (
                Value::U(u64::MAX),
                json!({"stringValue": "18446744073709551615"}),
            ),
            (Value::I(-3), json!({"intValue": "-3"})),
            (Value::F(0.5), json!({"doubleValue": 0.5})),
            (Value::F(f64::INFINITY), json!({"doubleValue": "inf"})),
            (Value::Bool(true), json!({"boolValue": true})),
            (Value::Char('é'), json!({"stringValue": "é"})),
            (
                Value::Str {
                    s: "ab".into(),
                    truncated: false,
                },
                json!({"stringValue": "ab"}),
            ),
            (
                Value::Str {
                    s: "ab".into(),
                    truncated: true,
                },
                json!({"stringValue": "ab…"}),
            ),
            (
                Value::Bytes {
                    bytes: vec![1, 2],
                    truncated: false,
                },
                json!({"bytesValue": "AQI="}),
            ),
            (Value::Option(None), json!({})),
            (
                Value::Option(Some(Box::new(Value::U(1)))),
                json!({"intValue": "1"}),
            ),
            (
                Value::Array(vec![Value::U(1), Value::Bool(false)]),
                json!({"arrayValue": {"values": [{"intValue": "1"}, {"boolValue": false}]}}),
            ),
        ];
        for (value, want) in cases {
            assert_eq!(any_value(&value), want, "{value:?}");
        }
    }

    fn site(level: Level, module: &str) -> crate::SiteInfo {
        crate::SiteInfo {
            id: 0x2f6c_01aa,
            kind: crate::Kind::Log,
            level,
            module: module.to_owned(),
            file: "src/main.rs".to_owned(),
            line: 42,
            column: 5,
            fmt: "pid out={}".to_owned(),
            nargs: 1,
        }
    }

    fn attrs(record: &Json) -> HashMap<String, Json> {
        record["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| (a["key"].as_str().unwrap().to_owned(), a["value"].clone()))
            .collect()
    }

    #[test]
    fn every_kind_of_item_becomes_a_log_record() {
        let info = site(Level::Info, "app::motor");
        let trace = site(Level::Trace, "");
        let crash = |kind| crate::Crash {
            kind,
            signal: 0,
            code: 0,
            addr: 0,
            pc: 0,
            message: "watchdog".into(),
        };
        let items = [
            Item::SpanOpen {
                ts_ns: 100,
                ctx: 1,
                id: 9,
                parent: 0,
                name: "step",
            },
            Item::Record(crate::Record {
                ts_ns: 412_000,
                ctx: 1,
                site: &info,
                args: vec![Value::U(7)],
            }),
            Item::Record(crate::Record {
                ts_ns: 500_000,
                ctx: 2,
                site: &trace,
                args: vec![Value::F(0.5)],
            }),
            Item::SpanClose {
                ts_ns: 2_100,
                ctx: 1,
                id: 9,
            },
            Item::Dropped {
                ts_ns: 600_000,
                ctx: 2,
                count: 3,
            },
            Item::Gap {
                ts_ns: 700_000,
                offset: 17,
                reason: "bad tag".into(),
                skipped: 4,
            },
            Item::Crash {
                ts_ns: 800_000,
                ctx: 1,
                crash: crash(3),
            },
            Item::Crash {
                ts_ns: 900_000,
                ctx: 1,
                crash: crash(1),
            },
        ];
        let names = HashMap::from([(1, "main".to_owned())]);
        let cfg = Config {
            service_name: "motor".into(),
            host_name: "gw1".into(),
            epoch_ns: Some(1_794_907_800_000_000_000),
            start_ns: None,
        };
        let out = export(&items, &names, true, &cfg);
        assert_eq!(out.lines().count(), 1);
        let data: Json = serde_json::from_str(&out).unwrap();
        let resource = &data["resourceLogs"][0];
        assert_eq!(
            resource["resource"]["attributes"],
            json!([
                {"key": "service.name", "value": {"stringValue": "motor"}},
                {"key": "host.name", "value": {"stringValue": "gw1"}},
                {"key": "rtlog.contract", "value": {"stringValue": "mismatch"}},
            ])
        );
        let scope = &resource["scopeLogs"][0];
        assert_eq!(scope["scope"]["name"], "rtlog");
        let records = scope["logRecords"].as_array().unwrap();
        assert_eq!(records.len(), 6, "spans are not log records");

        let r = &records[0];
        assert_eq!(r["timeUnixNano"], "1794907800000412000");
        assert_eq!(r["observedTimeUnixNano"], r["timeUnixNano"]);
        assert_eq!(
            (r["severityNumber"].as_u64(), r["severityText"].as_str()),
            (Some(9), Some("INFO"))
        );
        assert_eq!(r["body"], json!({"stringValue": "pid out=7"}));
        assert!(r.get("eventName").is_none());
        let a = attrs(r);
        assert_eq!(a["code.file.path"], json!({"stringValue": "src/main.rs"}));
        assert_eq!(a["code.line.number"], json!({"intValue": "42"}));
        assert_eq!(a["rtlog.site"], json!({"stringValue": "2f6c01aa"}));
        assert_eq!(a["rtlog.template"], json!({"stringValue": "pid out={}"}));
        assert_eq!(a["rtlog.module"], json!({"stringValue": "app::motor"}));
        assert_eq!(a["thread.name"], json!({"stringValue": "main"}));
        assert_eq!(a["thread.id"], json!({"intValue": "1"}));
        assert_eq!(a["rtlog.arg.0"], json!({"intValue": "7"}));
        assert_eq!(a["rtlog.ts_ns"], json!({"intValue": "412000"}));

        let a = attrs(&records[1]);
        assert_eq!(records[1]["severityNumber"], 1);
        assert_eq!(a["thread.name"], json!({"stringValue": "ctx2"}));
        assert!(!a.contains_key("rtlog.module"));
        assert_eq!(a["rtlog.arg.0"], json!({"doubleValue": 0.5}));

        assert_eq!(records[2]["eventName"], "rtlog.dropped");
        assert_eq!(records[2]["severityText"], "WARN");
        assert_eq!(
            attrs(&records[2])["rtlog.dropped"],
            json!({"intValue": "3"})
        );
        assert_eq!(records[3]["eventName"], "rtlog.gap");
        assert_eq!(records[3]["severityNumber"], 17);
        let a = attrs(&records[3]);
        assert_eq!(a["rtlog.gap.offset"], json!({"intValue": "17"}));
        assert!(!a.contains_key("thread.id"));
        assert_eq!(records[4]["eventName"], "rtlog.snapshot");
        assert_eq!(records[4]["severityNumber"], 9);
        assert_eq!(records[5]["eventName"], "rtlog.crash");
        assert_eq!(
            (
                records[5]["severityNumber"].as_u64(),
                records[5]["severityText"].as_str()
            ),
            (Some(21), Some("FATAL"))
        );
        assert!(records[5]["body"]["stringValue"]
            .as_str()
            .unwrap()
            .contains("watchdog"));

        // No time given: no time fields, and no host or mismatch attribute.
        let out = export(&items, &names, false, &Config::default());
        let data: Json = serde_json::from_str(&out).unwrap();
        let resource = &data["resourceLogs"][0];
        assert_eq!(
            resource["resource"]["attributes"],
            json!([{"key": "service.name", "value": {"stringValue": "rtlog"}}])
        );
        let r = &resource["scopeLogs"][0]["logRecords"][0];
        assert!(r.get("timeUnixNano").is_none() && r.get("observedTimeUnixNano").is_none());

        // The capture's first event (the span opening at 100 ns) at a known time.
        let cfg = Config {
            start_ns: Some(1_000_000_000),
            ..Config::default()
        };
        let data: Json = serde_json::from_str(&export(&items, &names, false, &cfg)).unwrap();
        assert_eq!(
            data["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["timeUnixNano"],
            "1000411900"
        );
    }

    #[test]
    fn records_are_split_into_batches() {
        let info = site(Level::Info, "m");
        let items: Vec<Item<'_>> = (0..BATCH as u64 + 1)
            .map(|i| {
                Item::Record(crate::Record {
                    ts_ns: i,
                    ctx: 0,
                    site: &info,
                    args: vec![Value::U(i)],
                })
            })
            .collect();
        let out = export(&items, &HashMap::new(), false, &Config::default());
        let counts: Vec<usize> = out
            .lines()
            .map(|l| {
                let d: Json = serde_json::from_str(l).unwrap();
                d["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
                    .as_array()
                    .unwrap()
                    .len()
            })
            .collect();
        assert_eq!(counts, [BATCH, 1]);
        assert_eq!(export(&[], &HashMap::new(), false, &Config::default()), "");
    }

    #[test]
    fn decode_takes_the_configuration() {
        let c = Contract::from_jsonl(
            r#"{"id":7,"level":"warn","module":"m","file":"f.rs","line":1,"column":1,"fmt":"x={}","nargs":1}"#,
        )
        .unwrap();
        let mut bytes = rtlog::wire::Header::new(c.hash(), 1_000_000_000)
            .encode()
            .to_vec();
        bytes.extend(7u32.to_le_bytes());
        bytes.extend([10, 0, rtlog::arg::Tag::U8 as u8, 5]); // delta 10 ticks, ctx 0, u8 5
        let cfg = Config {
            service_name: "pump".into(),
            ..Config::default()
        };
        let d = decode(&c, &bytes, &Options::default(), &cfg).unwrap();
        assert_eq!(d.records, 1);
        assert!(
            d.text
                .contains(r#"{"key":"service.name","value":{"stringValue":"pump"}}"#),
            "{}",
            d.text
        );
        assert!(
            d.text.contains(r#""body":{"stringValue":"x=5"}"#),
            "{}",
            d.text
        );
        let same = crate::decode(&c, &bytes, crate::Output::OtlpJson, false).unwrap();
        assert_eq!(same.text, d.text.replace("pump", "rtlog"));
    }
}
