# Send logs to the systemd journal

`rtlog decode --out journald` writes a capture as systemd journal entries, and `--journald-socket` gives them to a running `systemd-journald`. The conversion runs on the host, in the decoder: the program that logs links no systemd library, and its log call costs what it cost before.

```sh
# Into the journal of this machine
rtlog decode --contract app.contract.jsonl app.rtlog --app motor \
    --journald-socket /run/systemd/journal/socket
journalctl -t motor -o verbose

# As a file in the Journal Export Format
rtlog decode --contract app.contract.jsonl app.rtlog --out journald \
    --start 2026-11-17T09:30:00Z --host gw1 --app motor -o app.journal-export
```

One entry of the file:

```text
__REALTIME_TIMESTAMP=1794907800000412
_HOSTNAME=gw1
MESSAGE=pid out=0.42 err=-3
PRIORITY=6
SYSLOG_IDENTIFIER=motor
RTLOG_KIND=log
RTLOG_TS_NS=324397190928
CODE_FILE=src/main.rs
CODE_LINE=42
RTLOG_LEVEL=info
RTLOG_SITE=2f6c01aa
RTLOG_TEMPLATE=pid out={} err={}
RTLOG_MODULE=app
RTLOG_ARG_0=0.42
RTLOG_ARG_1=-3
RTLOG_CTX=1
RTLOG_CTX_NAME=main
```

## What is in an entry

| Field | Value |
| --- | --- |
| `MESSAGE` | The formatted text. A control character is written as an escape (`\n`, `\t`), so a field is one line |
| `PRIORITY` | 0 to 7, as in syslog: see the next table |
| `SYSLOG_IDENTIFIER` | `--app`; `rtlog` by default. `journalctl -t <name>` selects on it |
| `CODE_FILE`, `CODE_LINE` | The call site |
| `RTLOG_KIND` | `log`, `span`, `dropped`, `gap`, `crash` or `snapshot` |
| `RTLOG_LEVEL` | The rtlog level, which tells `debug` from `trace` |
| `RTLOG_SITE` | The site ID in hexadecimal: the same for every record of one call site |
| `RTLOG_TEMPLATE` | The format string |
| `RTLOG_MODULE` | The module path (Rust) or function (C), when the contract has one |
| `RTLOG_ARG_<n>` | Each argument, as text |
| `RTLOG_CTX`, `RTLOG_CTX_NAME` | The context ID and the name of the thread or task that logged (`ctx3` if it has none) |
| `RTLOG_TS_NS` | The time on the target's clock, in nanoseconds |
| `RTLOG_CONTRACT` | `mismatch`, only when decoding was forced with a contract that does not match |
| `__REALTIME_TIMESTAMP` | File only: the wall-clock time in microseconds, when `--start` or `--epoch` is given |
| `_HOSTNAME` | File only: `--host`, when given |

| rtlog | `PRIORITY` | Other fields |
| --- | ---: | --- |
| `error` | 3 | |
| `warn` | 4 | |
| `info` | 6 | |
| `debug`, `trace` | 7 | |
| A finished span | 7 | `RTLOG_SPAN` (name), `RTLOG_SPAN_ID`, `RTLOG_START_NS`, `RTLOG_DUR_NS` |
| Dropped frames | 4 | `RTLOG_COUNT` |
| A gap in a damaged stream | 3 | `RTLOG_OFFSET`, `RTLOG_SKIPPED` |
| A crash (panic, fatal signal) | 2 | |
| A snapshot (`crash::snapshot`) | 5 | |

A span gives one entry when it closes; `--no-spans` leaves them out.

Every field can be used to select entries:

```sh
journalctl -t motor PRIORITY=4 RTLOG_CTX_NAME=control
journalctl -t motor RTLOG_SITE=2f6c01aa -o json
```

## Time of day and host name

The journal sets the fields whose name starts with an underscore itself; a program that writes to its socket cannot. So the two ways differ:

| | Time of an entry | Host name |
| --- | --- | --- |
| `--journald-socket` | When the journal receives it. `RTLOG_TS_NS` keeps the time on the target's clock, with the exact spacing between records | The machine running the journal |
| `--out journald` (file) | `--start <time>`, the wall-clock time of the capture's first event, or `--epoch <time>`, that of the target's time zero; without either, no time | `--host` |

`<time>` is Unix seconds or an RFC 3339 time (`2026-11-17T09:30:00Z`). If the target's clock already counts from 1970-01-01, use `--epoch 0`.

So to put a capture from yesterday, or from another machine, into a journal with its own times, write the file and import it with `systemd-journal-remote`, which reads the Journal Export Format:

```sh
/usr/lib/systemd/systemd-journal-remote --output=/var/log/journal/remote/app.journal app.journal-export
journalctl --file /var/log/journal/remote/app.journal
```

Give `--start` or `--epoch` for an import: an entry without a time may be refused.

The format is checked in CI field by field, and sending is checked against a socket the test opens. Neither has been run against `systemd-journald` or `systemd-journal-remote` yet; if one refuses an entry, please open an issue with the entry.

## From Rust

```rust
use rtlog_decode::{journald, Contract, Options};

fn to_journal(contract_jsonl: &str, capture: &[u8]) -> Result<usize, Box<dyn std::error::Error>> {
    let contract = Contract::from_jsonl(contract_jsonl)?;
    let cfg = journald::Config {
        identifier: "motor".into(),
        ..journald::Config::default()
    };
    let export = journald::decode(&contract, capture, &Options::default(), &cfg)?.text;
    // Unix only: one datagram per entry to the running journal.
    Ok(journald::send(&export, journald::SOCKET)?)
}
```

`rtlog_decode::decode` with `Output::Journald` uses `Config::default()`.
