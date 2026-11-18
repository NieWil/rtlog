//! Syslog exporter: a decoded capture as [RFC 5424] lines, one per record,
//! for `rsyslog`, `syslog-ng`, a log collector or any tool that reads them.
//!
//! ```text
//! <14>1 2026-11-17T09:30:00.000412Z gw1 motor main 2f6c01aa [rtlog@32473 level="info" file="src/main.rs" line="42" ts_ns="412000" ctx="1"] pid out=0.42 err=-3
//! ```
//!
//! | Field | Value |
//! | --- | --- |
//! | `PRI` | facility × 8 + severity; severity from the level, see [`severity`] |
//! | `TIMESTAMP` | [`Config::epoch_ns`] plus the record's time, UTC, microseconds; `-` without an epoch. A span's line carries the time it closed |
//! | `HOSTNAME`, `APP-NAME` | [`Config::hostname`], [`Config::app_name`] |
//! | `PROCID` | the context (thread or task) name |
//! | `MSGID` | the site ID in hexadecimal for a log record; `span`, `dropped`, `gap`, `crash` otherwise |
//! | structured data | one element named [`Config::sd_id`]: level, file, line, module, target time, context ID |
//! | `MSG` | the formatted message, control characters escaped, no byte-order mark |
//!
//! The target's clock counts from an arbitrary start (boot, or the start of
//! the process), so a capture alone does not say what time of day a record
//! was written. Give the wall-clock time of the target's time zero in
//! [`Config::epoch_ns`] (`--epoch` on the command line) to get real
//! timestamps, or the time the capture began in [`Config::start_ns`]
//! (`--start`); without either the field is `-` and the receiver
//! stamps the line.
//!
//! [RFC 5424]: https://www.rfc-editor.org/rfc/rfc5424

use std::collections::HashMap;
use std::fmt::Write as _;

use rtlog::Level;

use crate::{human_ns, render, Contract, DecodeError, Decoded, Item, Options};

/// How the syslog lines are filled in; see the [module documentation](self).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Syslog facility, 0 to 23. Default 1 (`user`).
    pub facility: u8,
    /// `HOSTNAME`: the machine the capture came from. Default `-` (not stated).
    pub hostname: String,
    /// `APP-NAME`: the program that logged. Default `rtlog`.
    pub app_name: String,
    /// Name of the structured-data element. A name of one's own must end in
    /// `@<private enterprise number>`; the default, `rtlog@32473`, uses the
    /// number RFC 5424 reserves for examples.
    pub sd_id: String,
    /// Wall-clock time of the target's time zero, in nanoseconds since
    /// 1970-01-01T00:00:00Z. With neither this nor [`Config::start_ns`] the
    /// `TIMESTAMP` field is `-`.
    pub epoch_ns: Option<u64>,
    /// Wall-clock time of the first event of the capture, in nanoseconds
    /// since 1970-01-01T00:00:00Z: for when the target's time zero is not
    /// known but the moment the capture began is. Used when
    /// [`Config::epoch_ns`] is `None`.
    pub start_ns: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            facility: 1,
            hostname: "-".to_owned(),
            app_name: "rtlog".to_owned(),
            sd_id: "rtlog@32473".to_owned(),
            epoch_ns: None,
            start_ns: None,
        }
    }
}

/// The facility number for a keyword (`user`, `daemon`, `local0` … `local7`,
/// and the other names of RFC 5424 table 1) or a number from 0 to 23.
pub fn facility(name: &str) -> Option<u8> {
    const NAMES: [&str; 24] = [
        "kern", "user", "mail", "daemon", "auth", "syslog", "lpr", "news", "uucp", "cron",
        "authpriv", "ftp", "ntp", "audit", "alert", "clock", "local0", "local1", "local2",
        "local3", "local4", "local5", "local6", "local7",
    ];
    match name.parse::<u8>() {
        Ok(n) => (n < 24).then_some(n),
        Err(_) => NAMES.iter().position(|n| *n == name).map(|i| i as u8),
    }
}

/// The syslog severity of a level: `error` 3, `warn` 4, `info` 6, `debug`
/// and `trace` 7 (syslog has nothing below debug; the structured data keeps
/// the level's name).
pub fn severity(level: Level) -> u8 {
    match level {
        Level::Error => 3,
        Level::Warn => 4,
        Level::Info => 6,
        Level::Debug | Level::Trace => 7,
    }
}

/// Parse a wall-clock time into nanoseconds since the Unix epoch: seconds
/// (`1794907800`, `1794907800.25`) or an RFC 3339 time
/// (`2026-11-17T09:30:00Z`, `2026-11-17T10:30:00.5+01:00`).
pub fn parse_epoch(s: &str) -> Option<u64> {
    fn frac_ns(f: &str) -> Option<u64> {
        if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let digits: String = f.chars().chain("00000000".chars()).take(9).collect();
        digits.parse().ok()
    }
    fn secs_and_frac(s: &str) -> Option<(u64, u64)> {
        match s.split_once('.') {
            Some((a, f)) => Some((a.parse().ok()?, frac_ns(f)?)),
            None => Some((s.parse().ok()?, 0)),
        }
    }
    let b = s.as_bytes();
    if !s.contains(['T', 't']) {
        if b.first().is_some_and(|c| !c.is_ascii_digit()) {
            return None;
        }
        let (secs, ns) = secs_and_frac(s)?;
        return secs.checked_mul(1_000_000_000)?.checked_add(ns);
    }
    // YYYY-MM-DDThh:mm:ss[.frac](Z|±hh:mm)
    if b.len() < 20 || !s.is_ascii() || b[4] != b'-' || b[7] != b'-' || b[13] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute) = (num(11..13)?, num(14..16)?);
    if b[16] != b':' {
        return None;
    }
    let zone_at = 19 + s[19..].find(['Z', 'z', '+', '-'])?;
    let (sec, ns) = secs_and_frac(&s[17..zone_at])?;
    let offset_s = match &s[zone_at..] {
        "Z" | "z" => 0,
        z if z.len() == 6 && z.as_bytes()[3] == b':' => {
            let sign = if z.starts_with('-') { -1 } else { 1 };
            sign * (z[1..3].parse::<i64>().ok()? * 3600 + z[4..6].parse::<i64>().ok()? * 60)
        }
        _ => return None,
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || sec > 60
    {
        return None;
    }
    // Days since 1970-01-01 (proleptic Gregorian calendar).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    let secs = days * 86_400 + hour * 3600 + minute * 60 + sec as i64 - offset_s;
    u64::try_from(secs)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(ns)
}

/// `YYYY-MM-DDThh:mm:ss.uuuuuuZ` for nanoseconds since the Unix epoch.
fn rfc3339(ns: u64) -> String {
    let secs = ns / 1_000_000_000;
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:06}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ns % 1_000_000_000 / 1_000
    )
}

/// A header field: printable ASCII without spaces, at most `max` bytes;
/// anything else becomes `_`, and an empty field is `-`.
fn field(s: &str, max: usize) -> String {
    let out: String = s
        .chars()
        .take(max)
        .map(|c| if c.is_ascii_graphic() { c } else { '_' })
        .collect();
    if out.is_empty() {
        "-".to_owned()
    } else {
        out
    }
}

/// A structured-data name (element or parameter): as [`field`], and without
/// `=`, `]` and `"`. At most 32 bytes.
fn sd_name(s: &str) -> String {
    field(s, 32).replace(['=', ']', '"'], "_")
}

/// Append ` name="value"` with `"`, `\` and `]` escaped as RFC 5424 asks.
fn param(out: &mut String, name: &str, value: &str) {
    write!(out, " {name}=\"").unwrap();
    for c in value.chars() {
        match c {
            '"' | '\\' | ']' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The message with control characters escaped (`\n`, `\t`, `\u{1b}`), so
/// one record stays one line.
fn message(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

struct Writer<'a> {
    cfg: &'a Config,
    /// Where the target's clock sits in wall-clock time, if known.
    clock: Option<crate::WallClock>,
    names: &'a HashMap<u32, String>,
    hostname: String,
    app_name: String,
    sd_id: String,
    mismatched: bool,
    out: String,
}

impl Writer<'_> {
    /// One line; `kind` is the severity and the `level` parameter, and
    /// `params` are appended to the common structured data.
    fn line(
        &mut self,
        (severity, level): (u8, &str),
        ts_ns: u64,
        ctx: Option<u32>,
        msgid: &str,
        params: &[(&str, String)],
        msg: &str,
    ) {
        let pri = u32::from(self.cfg.facility) * 8 + u32::from(severity);
        let stamp = match self.clock {
            Some(c) => rfc3339(c.at(ts_ns)),
            None => "-".to_owned(),
        };
        let procid = match ctx {
            Some(c) => match self.names.get(&c) {
                Some(n) => field(n, 128),
                None => format!("ctx{c}"),
            },
            None => "-".to_owned(),
        };
        write!(
            self.out,
            "<{pri}>1 {stamp} {} {} {procid} {} [{}",
            self.hostname,
            self.app_name,
            field(msgid, 32),
            self.sd_id
        )
        .unwrap();
        param(&mut self.out, "level", level);
        for (name, value) in params {
            param(&mut self.out, name, value);
        }
        param(&mut self.out, "ts_ns", &ts_ns.to_string());
        if let Some(c) = ctx {
            param(&mut self.out, "ctx", &c.to_string());
        }
        if self.mismatched {
            param(&mut self.out, "contract", "mismatch");
        }
        writeln!(self.out, "] {}", message(msg)).unwrap();
    }
}

/// Render sorted timeline items as syslog lines.
pub(crate) fn export(
    items: &[Item<'_>],
    names: &HashMap<u32, String>,
    mismatched: bool,
    spans: bool,
    cfg: &Config,
) -> String {
    let mut w = Writer {
        cfg,
        clock: crate::WallClock::new(items, cfg.epoch_ns, cfg.start_ns),
        names,
        hostname: field(&cfg.hostname, 255),
        app_name: field(&cfg.app_name, 48),
        sd_id: sd_name(&cfg.sd_id),
        mismatched,
        out: String::new(),
    };
    let mut open: HashMap<u64, (u64, &str)> = HashMap::new();
    for item in items {
        match item {
            Item::Record(r) => {
                let site = r.site;
                let mut params = vec![("file", site.file.clone()), ("line", site.line.to_string())];
                if !site.module.is_empty() {
                    params.push(("module", site.module.clone()));
                }
                w.line(
                    (severity(site.level), site.level.as_str()),
                    r.ts_ns,
                    Some(r.ctx),
                    &format!("{:08x}", site.id),
                    &params,
                    &render::format(&site.fmt, &r.args),
                );
            }
            Item::SpanOpen {
                ts_ns, id, name, ..
            } => {
                open.insert(*id, (*ts_ns, name));
            }
            Item::SpanClose { ts_ns, ctx, id } => {
                // One line per finished span, when it closes.
                let Some((start, name)) = open.remove(id) else {
                    continue;
                };
                if !spans {
                    continue;
                }
                let dur = ts_ns.saturating_sub(start);
                w.line(
                    (7, "span"),
                    *ts_ns,
                    Some(*ctx),
                    "span",
                    &[
                        ("span", (*name).to_owned()),
                        ("id", id.to_string()),
                        ("start_ns", start.to_string()),
                        ("dur_ns", dur.to_string()),
                    ],
                    &format!("span {name} took {}", human_ns(dur)),
                );
            }
            Item::Dropped { ts_ns, ctx, count } => w.line(
                (4, "warn"),
                *ts_ns,
                Some(*ctx),
                "dropped",
                &[("count", count.to_string())],
                &format!("{count} frames dropped"),
            ),
            Item::Gap {
                ts_ns,
                offset,
                reason,
                skipped,
            } => w.line(
                (3, "error"),
                *ts_ns,
                None,
                "gap",
                &[("offset", offset.to_string()), ("skipped", skipped.to_string())],
                &format!("bad data at byte {offset} ({reason}); {skipped} bytes skipped, resumed at the next resync frame"),
            ),
            Item::Crash { ts_ns, ctx, crash } => {
                // A snapshot is a notice; a panic or a fatal signal is critical.
                let kind = if crash.kind == 3 { (5, "snapshot") } else { (2, "crash") };
                w.line(kind, *ts_ns, Some(*ctx), "crash", &[], &crash.describe());
            }
        }
    }
    w.out
}

/// Decode a whole stream to syslog lines with `cfg`; as
/// [`decode_with`](crate::decode_with), whose `Output::Syslog` uses
/// [`Config::default`]. `opts.output` and `opts.color` are not used.
pub fn decode(
    contract: &Contract,
    bytes: &[u8],
    opts: &Options,
    cfg: &Config,
) -> Result<Decoded, DecodeError> {
    let opts = Options {
        output: crate::Output::Syslog,
        ..*opts
    };
    crate::decode_inner(
        contract,
        bytes,
        &opts,
        cfg,
        &crate::otlp::Config::default(),
        &crate::journald::Config::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_parses_seconds_and_rfc3339() {
        assert_eq!(parse_epoch("0"), Some(0));
        assert_eq!(parse_epoch("1794907800"), Some(1_794_907_800_000_000_000));
        assert_eq!(parse_epoch("1.25"), Some(1_250_000_000));
        assert_eq!(parse_epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_epoch("2026-11-17T09:30:00Z"),
            Some(1_794_907_800_000_000_000)
        );
        assert_eq!(
            parse_epoch("2026-11-17T10:30:00.5+01:00"),
            Some(1_794_907_800_500_000_000)
        );
        assert_eq!(
            parse_epoch("2026-11-17t04:30:00-05:00"),
            Some(1_794_907_800_000_000_000)
        );
        // 2000 was a leap year, 2100 will not be.
        assert_eq!(
            parse_epoch("2000-03-01T00:00:00Z"),
            Some(951_868_800_000_000_000)
        );
        for bad in [
            "",
            "-5",
            "abc",
            "1.",
            "1.x",
            "2026-11-17",
            "2026-11-17T09:30:00",
            "2026-13-17T09:30:00Z",
            "2026-11-17T24:30:00Z",
            "2026-11-17T09:30:00+0100",
            "2026/11/17T09:30:00Z",
            "2026-11-17T09-30:00Z",
            "2026-11-17T09:30-00Z",
            "1969-12-31T23:59:59Z",
            "2026-11-17T09:30:0é0Z",
            "99999999999999999999",
            "999999999999",
        ] {
            assert_eq!(parse_epoch(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn timestamps_round_trip() {
        for s in [
            "1970-01-01T00:00:00.000000Z",
            "2000-02-29T23:59:59.999999Z",
            "2026-11-17T09:30:00.000412Z",
            "2100-03-01T00:00:00.000000Z",
            "2024-12-31T12:00:00.500000Z",
        ] {
            assert_eq!(rfc3339(parse_epoch(s).unwrap()), s);
        }
    }

    #[test]
    fn facilities_by_name_and_number() {
        assert_eq!(facility("kern"), Some(0));
        assert_eq!(facility("user"), Some(1));
        assert_eq!(facility("local0"), Some(16));
        assert_eq!(facility("local7"), Some(23));
        assert_eq!(facility("23"), Some(23));
        assert_eq!(facility("24"), None);
        assert_eq!(facility("nope"), None);
    }

    #[test]
    fn fields_and_values_are_made_safe() {
        assert_eq!(field("", 8), "-");
        assert_eq!(field("motor ctl\té", 32), "motor_ctl__");
        assert_eq!(field("abcdefgh", 4), "abcd");
        assert_eq!(sd_name("a=b]c\"d"), "a_b_c_d");
        let mut s = String::new();
        param(&mut s, "k", "a\"b\\c]d\ne\x07");
        assert_eq!(s, " k=\"a\\\"b\\\\c\\]d\\ne \"");
        assert_eq!(message("a\nb\tc\x1b é"), "a\\nb\\tc\\u{1b} é");
    }

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
            nargs: 1,
        }
    }

    #[test]
    fn every_kind_of_item_becomes_one_line() {
        let info = site(Level::Info, "app::motor", "pid out={}");
        let trace = site(Level::Trace, "", "quote \"{}\"]\nnext");
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
                args: vec![crate::Value::U(7)],
            }),
            Item::Record(crate::Record {
                ts_ns: 500_000,
                ctx: 2,
                site: &trace,
                args: vec![crate::Value::U(1)],
            }),
            Item::SpanClose {
                ts_ns: 2_100,
                ctx: 1,
                id: 9,
            },
            // A close whose open was lost (dropped frame) writes nothing.
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
        ];
        let names = HashMap::from([(1, "main loop".to_owned())]);
        let cfg = Config {
            facility: 16,
            hostname: "gw 1".into(),
            app_name: String::new(),
            epoch_ns: parse_epoch("2026-11-17T09:30:00Z"),
            ..Config::default()
        };
        let out = export(&items, &names, false, true, &cfg);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 7, "{out}");
        assert_eq!(
            lines[0],
            "<134>1 2026-11-17T09:30:00.000412Z gw_1 - main_loop 2f6c01aa [rtlog@32473 level=\"info\" file=\"src/main.rs\" line=\"42\" module=\"app::motor\" ts_ns=\"412000\" ctx=\"1\"] pid out=7"
        );
        assert_eq!(
            lines[1],
            "<135>1 2026-11-17T09:30:00.000500Z gw_1 - ctx2 2f6c01aa [rtlog@32473 level=\"trace\" file=\"src/main.rs\" line=\"42\" ts_ns=\"500000\" ctx=\"2\"] quote \"1\"]\\nnext"
        );
        assert_eq!(
            lines[2],
            "<135>1 2026-11-17T09:30:00.000002Z gw_1 - main_loop span [rtlog@32473 level=\"span\" span=\"step\" id=\"9\" start_ns=\"100\" dur_ns=\"2000\" ts_ns=\"2100\" ctx=\"1\"] span step took 2000 ns"
        );
        assert!(lines[3].starts_with("<132>1 ") && lines[3].ends_with("] 3 frames dropped"));
        assert!(lines[3].contains(" ctx2 dropped [") && lines[3].contains(" count=\"3\""));
        assert!(
            lines[4].starts_with("<131>1 2026-11-17T09:30:00.000700Z gw_1 - - gap [")
                && lines[4].contains(" offset=\"17\" skipped=\"4\" ts_ns=\"700000\"]")
                && lines[4].ends_with("bad data at byte 17 (bad tag); 4 bytes skipped, resumed at the next resync frame"),
            "{}",
            lines[4]
        );
        assert!(
            lines[5].starts_with("<133>1 ")
                && lines[5].contains(" crash [rtlog@32473 level=\"snapshot\"")
        );
        assert!(lines[6].starts_with("<130>1 ") && lines[6].contains("level=\"crash\""));
        assert!(lines[6].contains("watchdog"), "{}", lines[6]);

        // The capture's first event (the span opening at 100 ns) at a known
        // time: the record 411.9 µs later is stamped from it.
        let cfg = Config {
            start_ns: parse_epoch("2026-11-17T09:30:00Z"),
            ..Config::default()
        };
        let out = export(&items, &names, false, true, &cfg);
        assert!(
            out.starts_with("<14>1 2026-11-17T09:30:00.000411Z - rtlog main_loop "),
            "{out}"
        );

        // A target clock that counts from 1970 and reads later than the
        // start time given: still only the difference counts.
        let late = [Item::Dropped {
            ts_ns: 0,
            ctx: 2,
            count: 1,
        }]
        .into_iter()
        .chain((0..2).map(|i| {
            Item::Record(crate::Record {
                ts_ns: 1_900_000_000_000_000_000 + i * 1_000,
                ctx: 1,
                site: &info,
                args: vec![crate::Value::U(i)],
            })
        }))
        .collect::<Vec<_>>();
        let out = export(&late, &names, false, true, &cfg);
        let stamps: Vec<&str> = out.lines().map(|l| l.split(' ').nth(1).unwrap()).collect();
        assert_eq!(
            stamps,
            [
                "2026-11-17T09:30:00.000000Z",
                "2026-11-17T09:30:00.000000Z",
                "2026-11-17T09:30:00.000001Z"
            ]
        );

        // No epoch: no timestamp. Spans off: no span line. A contract that
        // does not match the stream is marked on every line.
        let out = export(&items, &names, true, false, &Config::default());
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 6, "{out}");
        assert!(
            lines[0].starts_with("<14>1 - - rtlog main_loop 2f6c01aa ["),
            "{}",
            lines[0]
        );
        assert!(
            lines.iter().all(|l| l.contains(" contract=\"mismatch\"] ")),
            "{out}"
        );
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
            app_name: "pump".into(),
            ..Config::default()
        };
        let d = decode(&c, &bytes, &Options::default(), &cfg).unwrap();
        assert_eq!(d.records, 1);
        assert!(
            d.text.starts_with("<12>1 - - pump ctx0 00000007 ["),
            "{}",
            d.text
        );
        assert!(d.text.ends_with("] x=5\n"), "{}", d.text);
        let same = crate::decode(&c, &bytes, crate::Output::Syslog, false).unwrap();
        assert_eq!(same.text, d.text.replace(" pump ", " rtlog "));
    }
}
