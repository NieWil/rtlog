# Send logs to syslog

`rtlog decode --out syslog` writes a capture as [RFC 5424](https://www.rfc-editor.org/rfc/rfc5424) lines, one per record, for `rsyslog`, `syslog-ng` or a log collector that reads syslog. The conversion runs on the host, in the decoder: the program that logs is unchanged, and its log call costs what it cost before.

```sh
rtlog decode --contract app.contract.jsonl app.rtlog --out syslog \
    --syslog-start 2026-11-17T09:30:00Z --syslog-host gw1 --syslog-app motor
```

```text
<14>1 2026-11-17T09:30:00.000412Z gw1 motor main 2f6c01aa [rtlog@32473 level="info" file="src/main.rs" line="42" module="app" ts_ns="324397190928" ctx="1"] pid out=0.42 err=-3
<12>1 2026-11-17T09:30:00.001093Z gw1 motor main 953928b6 [rtlog@32473 level="warn" file="src/main.rs" line="57" module="app" ts_ns="324397871112" ctx="1"] temperature=81.5 C
```

## What is in a line

| Field | Value |
| --- | --- |
| `<PRI>` | facility × 8 + severity. Facility: `--syslog-facility`, default `user` (1). Severity: see the next table |
| Version | `1` |
| Timestamp | UTC, microseconds; `-` when no time is given (see [Time of day](#time-of-day)) |
| Host name | `--syslog-host`; `-` by default |
| Application | `--syslog-app`; `rtlog` by default |
| Process ID field | the name of the thread or task that logged (`ctx3` if it has none) |
| Message ID | the site ID in hexadecimal for a log record: the same for every record of one call site. Otherwise `span`, `dropped`, `gap` or `crash` |
| Structured data | one element, `rtlog@32473`: `level`, `file`, `line`, `module`, `ts_ns` (the time on the target's clock, in nanoseconds), `ctx` (context ID) |
| Message | the formatted text. A control character is written as an escape (`\n`, `\t`), so one record is one line. No byte-order mark |

| rtlog | Severity | Structured data `level` |
| --- | --- | --- |
| `error` | 3, error | `error` |
| `warn` | 4, warning | `warn` |
| `info` | 6, informational | `info` |
| `debug` | 7, debug | `debug` |
| `trace` | 7, debug (syslog has nothing lower) | `trace` |
| A finished span | 7, debug | `span`, with `span` (name), `id`, `start_ns`, `dur_ns` |
| Dropped frames | 4, warning | `warn`, with `count` |
| A gap in a damaged stream | 3, error | `error`, with `offset`, `skipped` |
| A crash (panic, fatal signal) | 2, critical | `crash` |
| A snapshot (`crash::snapshot`) | 5, notice | `snapshot` |

A span gives one line when it closes, with its duration; `--no-spans` leaves them out. When the contract does not match the stream (`--force`), every line has `contract="mismatch"`.

The element name `rtlog@32473` uses the enterprise number that RFC 5424 reserves for examples. If your organisation has a number, set your own name with `--syslog-sd-id name@number`.

## Time of day

A target's clock counts from an arbitrary start: boot, or a CPU counter. The capture holds those times and not the time of day. Give one of:

| Option | Meaning |
| --- | --- |
| `--syslog-start <time>` | The wall-clock time of the first event of the capture: about when the program started logging |
| `--syslog-epoch <time>` | The wall-clock time of the target's time zero, when you know it (a firmware whose clock starts at reset, and the time of the reset) |

If the target's clock already counts from 1970-01-01, use `--syslog-epoch 0`. `<time>` is Unix seconds (`1794907800`, `1794907800.25`) or an RFC 3339 time (`2026-11-17T09:30:00Z`, `2026-11-17T10:30:00+01:00`). With neither option the timestamp field is `-` and a syslog daemon stamps each line with the time it receives it; `ts_ns` in the structured data keeps the exact spacing between records.

## Where the lines go

To stdout, or to a file with `-o`:

```sh
rtlog decode --contract app.contract.jsonl app.rtlog --out syslog -o app.syslog
```

Each line is a complete RFC 5424 message, so a tool that reads such messages from a file or a pipe can take them as they are. `rtlog` does not yet open a connection to a syslog daemon itself: sending over UDP, TCP or a Unix socket, and following a capture while it grows, are on the [roadmap](roadmap.md) (sections 10 and 12d).

The format is checked in CI against the grammar of RFC 5424 on the fields rtlog fills. It has not been run against rsyslog or syslog-ng yet; if a daemon refuses a line, please open an issue with the line.

## From Rust

```rust
use rtlog_decode::{syslog, Contract, Options};

fn to_syslog(contract_jsonl: &str, capture: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    let contract = Contract::from_jsonl(contract_jsonl)?;
    let cfg = syslog::Config {
        hostname: "gw1".into(),
        app_name: "motor".into(),
        start_ns: syslog::parse_epoch("2026-11-17T09:30:00Z"),
        ..syslog::Config::default()
    };
    Ok(syslog::decode(&contract, capture, &Options::default(), &cfg)?.text)
}
```

`rtlog_decode::decode` with `Output::Syslog` uses `Config::default()`.
