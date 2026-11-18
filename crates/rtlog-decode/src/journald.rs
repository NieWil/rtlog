//! journald exporter: a decoded capture as systemd journal entries, in the
//! [Journal Export Format], one entry per record, finished span, drop
//! report, stream gap and crash. `systemd-journal-remote` reads this format
//! from a file or a pipe; [`send`] gives the same entries to a running
//! `systemd-journald` over its native socket.
//!
//! ```text
//! __REALTIME_TIMESTAMP=1794907800000412
//! MESSAGE=pid out=0.42 err=-3
//! PRIORITY=6
//! SYSLOG_IDENTIFIER=motor
//! RTLOG_KIND=log
//! RTLOG_TS_NS=412000
//! CODE_FILE=src/main.rs
//! CODE_LINE=42
//! RTLOG_LEVEL=info
//! RTLOG_SITE=2f6c01aa
//! RTLOG_TEMPLATE=pid out={} err={}
//! RTLOG_MODULE=app
//! RTLOG_ARG_0=0.42
//! RTLOG_ARG_1=-3
//! RTLOG_CTX=1
//! RTLOG_CTX_NAME=main
//! ```
//!
//! | Field | Value |
//! | --- | --- |
//! | `__REALTIME_TIMESTAMP` | wall-clock time in microseconds, when [`Config::epoch_ns`] or [`Config::start_ns`] is given |
//! | `_HOSTNAME` | [`Config::hostname`], when not empty |
//! | `MESSAGE` | the formatted message, control characters escaped so a field is one line |
//! | `PRIORITY` | as the syslog severity: see [`crate::syslog::severity`] |
//! | `SYSLOG_IDENTIFIER` | [`Config::identifier`] |
//! | `CODE_FILE`, `CODE_LINE` | the call site |
//! | `RTLOG_KIND` | `log`, `span`, `dropped`, `gap`, `crash` or `snapshot` |
//! | `RTLOG_*` | level, site ID, format string, module, context ID and name, each argument, time on the target's clock; for a span its name, ID, start and duration |
//!
//! The two fields that start with an underscore are set by the journal
//! itself and cannot be given by a client: [`send`] leaves them out, and
//! `systemd-journald` stamps each entry with the time it receives it.
//!
//! [Journal Export Format]: https://systemd.io/JOURNAL_EXPORT_FORMATS/

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::{human_ns, render, syslog, Contract, DecodeError, Decoded, Item, Options, Value};

/// The socket `systemd-journald` listens on for native entries.
pub const SOCKET: &str = "/run/systemd/journal/socket";

/// How the journal entries are filled in; see the [module documentation](self).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// `SYSLOG_IDENTIFIER`: the program that logged, what `journalctl -t`
    /// selects. Default `rtlog`.
    pub identifier: String,
    /// `_HOSTNAME`: the machine the capture came from. Left out when empty
    /// (the default).
    pub hostname: String,
    /// Wall-clock time of the target's time zero, in nanoseconds since
    /// 1970-01-01T00:00:00Z.
    pub epoch_ns: Option<u64>,
    /// Wall-clock time of the first event of the capture, for when the
    /// target's time zero is not known. Used when `epoch_ns` is `None`. With
    /// neither, the entries carry no `__REALTIME_TIMESTAMP`.
    pub start_ns: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            identifier: "rtlog".to_owned(),
            hostname: String::new(),
            epoch_ns: None,
            start_ns: None,
        }
    }
}

/// One entry being written: `NAME=value` lines, then a blank line.
struct Entry<'a> {
    out: &'a mut String,
}

impl Entry<'_> {
    /// A field. Control characters in the value are escaped (`\n`, `\t`):
    /// the text form of a field is one line.
    fn field(&mut self, name: &str, value: &str) {
        self.out.push_str(name);
        self.out.push('=');
        for c in value.chars() {
            if c.is_control() {
                self.out.extend(c.escape_default());
            } else {
                self.out.push(c);
            }
        }
        self.out.push('\n');
    }

    fn num(&mut self, name: &str, n: u64) {
        writeln!(self.out, "{name}={n}").unwrap();
    }
}

/// Render sorted timeline items as journal entries.
pub(crate) fn export(
    items: &[Item<'_>],
    names: &HashMap<u32, String>,
    mismatched: bool,
    spans: bool,
    cfg: &Config,
) -> String {
    let clock = crate::WallClock::new(items, cfg.epoch_ns, cfg.start_ns);
    let mut out = String::new();
    let mut open: HashMap<u64, (u64, &str)> = HashMap::new();
    for item in items {
        // What every entry starts with; `ctx` is absent for a gap.
        let mut begin = |ts_ns: u64, priority: u8, kind: &str, message: &str| {
            if let Some(c) = clock {
                writeln!(out, "__REALTIME_TIMESTAMP={}", c.at(ts_ns) / 1_000).unwrap();
            }
            let mut e = Entry { out: &mut out };
            if !cfg.hostname.is_empty() {
                e.field("_HOSTNAME", &cfg.hostname);
            }
            e.field("MESSAGE", message);
            e.num("PRIORITY", u64::from(priority));
            e.field("SYSLOG_IDENTIFIER", &cfg.identifier);
            e.field("RTLOG_KIND", kind);
            e.num("RTLOG_TS_NS", ts_ns);
            if mismatched {
                e.field("RTLOG_CONTRACT", "mismatch");
            }
        };
        let context = |out: &mut String, ctx: u32| {
            let mut e = Entry { out };
            e.num("RTLOG_CTX", u64::from(ctx));
            match names.get(&ctx) {
                Some(n) => e.field("RTLOG_CTX_NAME", n),
                None => e.field("RTLOG_CTX_NAME", &format!("ctx{ctx}")),
            }
        };
        match item {
            Item::Record(r) => {
                let site = r.site;
                begin(
                    r.ts_ns,
                    syslog::severity(site.level),
                    "log",
                    &render::format(&site.fmt, &r.args),
                );
                let mut e = Entry { out: &mut out };
                e.field("CODE_FILE", &site.file);
                e.num("CODE_LINE", u64::from(site.line));
                e.field("RTLOG_LEVEL", site.level.as_str());
                e.field("RTLOG_SITE", &format!("{:08x}", site.id));
                e.field("RTLOG_TEMPLATE", &site.fmt);
                if !site.module.is_empty() {
                    e.field("RTLOG_MODULE", &site.module);
                }
                for (i, a) in r.args.iter().enumerate() {
                    e.field(&format!("RTLOG_ARG_{i}"), &argument(a));
                }
                context(&mut out, r.ctx);
            }
            Item::SpanOpen {
                ts_ns, id, name, ..
            } => {
                open.insert(*id, (*ts_ns, name));
                continue;
            }
            Item::SpanClose { ts_ns, ctx, id } => {
                // One entry per finished span, when it closes.
                let Some((start, name)) = open.remove(id) else {
                    continue;
                };
                if !spans {
                    continue;
                }
                let dur = ts_ns.saturating_sub(start);
                begin(
                    *ts_ns,
                    7,
                    "span",
                    &format!("span {name} took {}", human_ns(dur)),
                );
                let mut e = Entry { out: &mut out };
                e.field("RTLOG_SPAN", name);
                e.num("RTLOG_SPAN_ID", *id);
                e.num("RTLOG_START_NS", start);
                e.num("RTLOG_DUR_NS", dur);
                context(&mut out, *ctx);
            }
            Item::Dropped { ts_ns, ctx, count } => {
                begin(*ts_ns, 4, "dropped", &format!("{count} frames dropped"));
                Entry { out: &mut out }.num("RTLOG_COUNT", u64::from(*count));
                context(&mut out, *ctx);
            }
            Item::Gap {
                ts_ns,
                offset,
                reason,
                skipped,
            } => {
                begin(
                    *ts_ns,
                    3,
                    "gap",
                    &format!("bad data at byte {offset} ({reason}); {skipped} bytes skipped, resumed at the next resync frame"),
                );
                let mut e = Entry { out: &mut out };
                e.num("RTLOG_OFFSET", *offset as u64);
                e.num("RTLOG_SKIPPED", *skipped as u64);
            }
            Item::Crash { ts_ns, ctx, crash } => {
                // A snapshot is a notice; a panic or a fatal signal is critical.
                let (priority, kind) = if crash.kind == 3 {
                    (5, "snapshot")
                } else {
                    (2, "crash")
                };
                begin(*ts_ns, priority, kind, &crash.describe());
                context(&mut out, *ctx);
            }
        }
        out.push('\n');
    }
    out
}

/// An argument as the text a `{}` placeholder gives.
fn argument(v: &Value) -> String {
    render::format("{}", std::slice::from_ref(v))
}

/// Decode a whole stream to journal entries with `cfg`; as
/// [`decode_with`](crate::decode_with), whose `Output::Journald` uses
/// [`Config::default`]. `opts.output` and `opts.color` are not used.
pub fn decode(
    contract: &Contract,
    bytes: &[u8],
    opts: &Options,
    cfg: &Config,
) -> Result<Decoded, DecodeError> {
    let opts = Options {
        output: crate::Output::Journald,
        ..*opts
    };
    crate::decode_inner(
        contract,
        bytes,
        &opts,
        &syslog::Config::default(),
        &crate::otlp::Config::default(),
        cfg,
    )
}

/// The entries of `export` (the text [`decode`] returns) as the datagrams
/// of journald's native protocol: one per entry, without the fields whose
/// name starts with an underscore, which a client may not set.
pub fn datagrams(export: &str) -> Vec<String> {
    export
        .split("\n\n")
        .map(|entry| {
            entry
                .lines()
                .filter(|l| !l.is_empty() && !l.starts_with('_'))
                .fold(String::new(), |mut d, l| {
                    d.push_str(l);
                    d.push('\n');
                    d
                })
        })
        .filter(|d| !d.is_empty())
        .collect()
}

/// Send the entries of `export` (the text [`decode`] returns) to
/// `systemd-journald` listening on the Unix datagram socket `socket`
/// (normally [`SOCKET`]). Returns the number of entries sent. journald
/// stamps each entry with the time it arrives; `RTLOG_TS_NS` keeps the time
/// on the target's clock.
#[cfg(unix)]
pub fn send(export: &str, socket: impl AsRef<std::path::Path>) -> std::io::Result<usize> {
    let sock = std::os::unix::net::UnixDatagram::unbound()?;
    let entries = datagrams(export);
    for entry in &entries {
        sock.send_to(entry.as_bytes(), socket.as_ref())?;
    }
    Ok(entries.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtlog::Level;

    fn site(level: Level, module: &str, fmt: &str) -> crate::SiteInfo {
        crate::SiteInfo {
            id: 0x2f6c_01aa,
            kind: crate::Kind::Log,
            level,
            module: module.to_owned(),
            file: "src/main.rs".to_owned(),
            line: 42,
            column: 5,
            fmt: fmt.to_owned(),
            nargs: 2,
        }
    }

    fn items<'a>(info: &'a crate::SiteInfo, trace: &'a crate::SiteInfo) -> Vec<Item<'a>> {
        let crash = |kind| crate::Crash {
            kind,
            signal: 0,
            code: 0,
            addr: 0,
            pc: 0,
            message: "watchdog".into(),
        };
        vec![
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
                site: info,
                args: vec![Value::F(0.42), Value::I(-3)],
            }),
            Item::Record(crate::Record {
                ts_ns: 500_000,
                ctx: 2,
                site: trace,
                args: vec![],
            }),
            Item::SpanClose {
                ts_ns: 2_100,
                ctx: 1,
                id: 9,
            },
            // A close whose open was lost writes nothing.
            Item::SpanClose {
                ts_ns: 2_200,
                ctx: 1,
                id: 77,
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
        ]
    }

    /// The fields of each entry, in order.
    fn entries(export: &str) -> Vec<Vec<(&str, &str)>> {
        assert!(export.ends_with("\n\n"), "{export:?}");
        export
            .trim_end()
            .split("\n\n")
            .map(|e| e.lines().map(|l| l.split_once('=').unwrap()).collect())
            .collect()
    }

    #[test]
    fn every_kind_of_item_becomes_one_entry() {
        let info = site(Level::Info, "app", "pid out={} err={}");
        let trace = site(Level::Trace, "", "line one\nline two");
        let items = items(&info, &trace);
        let names = HashMap::from([(1, "main".to_owned())]);
        let cfg = Config {
            identifier: "motor".into(),
            hostname: "gw1".into(),
            epoch_ns: Some(1_794_907_800_000_000_000),
            start_ns: None,
        };
        let out = export(&items, &names, false, true, &cfg);
        let all = entries(&out);
        assert_eq!(all.len(), 7, "{out}");
        assert_eq!(
            all[0],
            [
                ("__REALTIME_TIMESTAMP", "1794907800000412"),
                ("_HOSTNAME", "gw1"),
                ("MESSAGE", "pid out=0.42 err=-3"),
                ("PRIORITY", "6"),
                ("SYSLOG_IDENTIFIER", "motor"),
                ("RTLOG_KIND", "log"),
                ("RTLOG_TS_NS", "412000"),
                ("CODE_FILE", "src/main.rs"),
                ("CODE_LINE", "42"),
                ("RTLOG_LEVEL", "info"),
                ("RTLOG_SITE", "2f6c01aa"),
                ("RTLOG_TEMPLATE", "pid out={} err={}"),
                ("RTLOG_MODULE", "app"),
                ("RTLOG_ARG_0", "0.42"),
                ("RTLOG_ARG_1", "-3"),
                ("RTLOG_CTX", "1"),
                ("RTLOG_CTX_NAME", "main"),
            ]
        );
        let field = |e: &[(&str, &str)], name: &str| {
            e.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| (*v).to_owned())
        };
        // A newline in a message is escaped: a field is one line.
        assert_eq!(field(&all[1], "MESSAGE").unwrap(), "line one\\nline two");
        assert_eq!(field(&all[1], "PRIORITY").unwrap(), "7");
        assert_eq!(field(&all[1], "RTLOG_LEVEL").unwrap(), "trace");
        assert_eq!(field(&all[1], "RTLOG_CTX_NAME").unwrap(), "ctx2");
        assert_eq!(field(&all[1], "RTLOG_MODULE"), None);

        assert_eq!(field(&all[2], "RTLOG_KIND").unwrap(), "span");
        assert_eq!(field(&all[2], "MESSAGE").unwrap(), "span step took 2000 ns");
        assert_eq!(field(&all[2], "RTLOG_SPAN").unwrap(), "step");
        assert_eq!(field(&all[2], "RTLOG_SPAN_ID").unwrap(), "9");
        assert_eq!(field(&all[2], "RTLOG_START_NS").unwrap(), "100");
        assert_eq!(field(&all[2], "RTLOG_DUR_NS").unwrap(), "2000");
        assert_eq!(
            field(&all[2], "__REALTIME_TIMESTAMP").unwrap(),
            "1794907800000002"
        );

        assert_eq!(field(&all[3], "RTLOG_KIND").unwrap(), "dropped");
        assert_eq!(field(&all[3], "PRIORITY").unwrap(), "4");
        assert_eq!(field(&all[3], "RTLOG_COUNT").unwrap(), "3");
        assert_eq!(field(&all[4], "RTLOG_KIND").unwrap(), "gap");
        assert_eq!(field(&all[4], "PRIORITY").unwrap(), "3");
        assert_eq!(field(&all[4], "RTLOG_OFFSET").unwrap(), "17");
        assert_eq!(field(&all[4], "RTLOG_SKIPPED").unwrap(), "4");
        assert_eq!(field(&all[4], "RTLOG_CTX"), None);
        assert_eq!(field(&all[5], "RTLOG_KIND").unwrap(), "snapshot");
        assert_eq!(field(&all[5], "PRIORITY").unwrap(), "5");
        assert_eq!(field(&all[6], "RTLOG_KIND").unwrap(), "crash");
        assert_eq!(field(&all[6], "PRIORITY").unwrap(), "2");
        assert!(field(&all[6], "MESSAGE").unwrap().contains("watchdog"));

        // Field names are what journald accepts: capitals, digits, underscores.
        for e in &all {
            for (name, _) in e {
                assert!(
                    name.bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'),
                    "{name}"
                );
            }
        }

        // No time, no host, spans off, a contract that does not match.
        let out = export(&items, &names, true, false, &Config::default());
        let all = entries(&out);
        assert_eq!(all.len(), 6, "{out}");
        assert_eq!(all[0][0], ("MESSAGE", "pid out=0.42 err=-3"));
        assert_eq!(field(&all[0], "SYSLOG_IDENTIFIER").unwrap(), "rtlog");
        assert!(all
            .iter()
            .all(|e| field(e, "RTLOG_CONTRACT").as_deref() == Some("mismatch")));

        // The capture's first event (the span opening at 100 ns) at a known time.
        let cfg = Config {
            start_ns: Some(1_000_000_000),
            ..Config::default()
        };
        let out = export(&items, &names, false, true, &cfg);
        assert!(out.starts_with("__REALTIME_TIMESTAMP=1000411\n"), "{out}");
        assert_eq!(export(&[], &names, false, true, &cfg), "");
    }

    #[test]
    fn datagrams_leave_out_the_fields_only_the_journal_sets() {
        let info = site(Level::Info, "app", "pid out={} err={}");
        let trace = site(Level::Trace, "", "x");
        let items = items(&info, &trace);
        let cfg = Config {
            hostname: "gw1".into(),
            epoch_ns: Some(0),
            ..Config::default()
        };
        let out = export(&items, &HashMap::new(), false, true, &cfg);
        let grams = datagrams(&out);
        assert_eq!(grams.len(), 7);
        for g in &grams {
            assert!(g.starts_with("MESSAGE=") && g.ends_with('\n'), "{g}");
            assert!(!g.contains("\n\n") && !g.contains("_HOSTNAME") && !g.contains("__REALTIME"));
            assert!(g.contains("\nSYSLOG_IDENTIFIER=rtlog\n"), "{g}");
        }
        assert!(datagrams("").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn send_delivers_one_datagram_per_entry() {
        let dir = std::env::temp_dir().join(format!("rtlog-journald-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("socket");
        let _ = std::fs::remove_file(&path);
        let server = std::os::unix::net::UnixDatagram::bind(&path).unwrap();
        let export =
            "__REALTIME_TIMESTAMP=1\nMESSAGE=one\nPRIORITY=6\n\nMESSAGE=two\nPRIORITY=4\n\n";
        assert_eq!(send(export, &path).unwrap(), 2);
        let mut buf = [0u8; 256];
        let n = server.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"MESSAGE=one\nPRIORITY=6\n");
        let n = server.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"MESSAGE=two\nPRIORITY=4\n");
        drop(server);
        std::fs::remove_dir_all(&dir).unwrap();
        // No journal there: an error, not a silent loss.
        assert!(send(export, &path).is_err());
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
            identifier: "pump".into(),
            ..Config::default()
        };
        let d = decode(&c, &bytes, &Options::default(), &cfg).unwrap();
        assert_eq!(d.records, 1);
        assert!(
            d.text
                .starts_with("MESSAGE=x=5\nPRIORITY=4\nSYSLOG_IDENTIFIER=pump\n"),
            "{}",
            d.text
        );
        let same = crate::decode(&c, &bytes, crate::Output::Journald, false).unwrap();
        assert_eq!(same.text, d.text.replace("=pump\n", "=rtlog\n"));
    }
}
