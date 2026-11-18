# Send logs to OpenTelemetry

`rtlog decode --out otlp-json` writes a capture as OpenTelemetry log records in the [OTLP](https://opentelemetry.io/docs/specs/otlp/) JSON encoding. The conversion runs on the host, in the decoder: the program that logs links no OpenTelemetry SDK, and its log call costs what it cost before.

```sh
rtlog decode --contract app.contract.jsonl app.rtlog --out otlp-json \
    --start 2026-11-17T09:30:00Z --host gw1 --app motor -o app.otlp.jsonl
```

Each output line is one `LogsData` object with up to 1000 log records. Shown here indented, with one record:

```json
{
  "resourceLogs": [{
    "resource": { "attributes": [
      { "key": "service.name", "value": { "stringValue": "motor" } },
      { "key": "host.name", "value": { "stringValue": "gw1" } }
    ] },
    "scopeLogs": [{
      "scope": { "name": "rtlog", "version": "0.3.0" },
      "logRecords": [{
        "timeUnixNano": "1794907800000412000",
        "observedTimeUnixNano": "1794907800000412000",
        "severityNumber": 9,
        "severityText": "INFO",
        "body": { "stringValue": "pid out=0.42 err=-3" },
        "attributes": [
          { "key": "code.file.path", "value": { "stringValue": "src/main.rs" } },
          { "key": "code.line.number", "value": { "intValue": "42" } },
          { "key": "rtlog.site", "value": { "stringValue": "2f6c01aa" } },
          { "key": "rtlog.template", "value": { "stringValue": "pid out={} err={}" } },
          { "key": "rtlog.module", "value": { "stringValue": "app" } },
          { "key": "thread.name", "value": { "stringValue": "main" } },
          { "key": "thread.id", "value": { "intValue": "1" } },
          { "key": "rtlog.arg.0", "value": { "doubleValue": 0.42 } },
          { "key": "rtlog.arg.1", "value": { "intValue": "-3" } },
          { "key": "rtlog.ts_ns", "value": { "intValue": "324397190928" } }
        ]
      }]
    }]
  }]
}
```

## What is in a record

| OTLP field | Value |
| --- | --- |
| `timeUnixNano`, `observedTimeUnixNano` | The wall-clock time of the record, in nanoseconds. Both are left out when no time is given (see [Time of day](#time-of-day)) |
| `severityNumber`, `severityText` | From the level: see the next table |
| `body` | The formatted message |
| `eventName` | Only on records that are not log calls: `rtlog.dropped`, `rtlog.gap`, `rtlog.crash`, `rtlog.snapshot` |
| `code.file.path`, `code.line.number` | The call site |
| `thread.name`, `thread.id` | The name of the thread or task that logged (`ctx3` if it has none) and its context ID |
| `rtlog.site` | The site ID in hexadecimal: the same for every record of one call site |
| `rtlog.template` | The format string, so records of one kind can be grouped whatever their values |
| `rtlog.module` | The module path (Rust) or function (C), when the contract has one |
| `rtlog.arg.<n>` | Each argument with its type: integer, floating point, boolean, string, bytes (base64) or array |
| `rtlog.ts_ns` | The time on the target's clock, in nanoseconds: exact spacing between records even without a time of day |
| Resource | `service.name` from `--app` (default `rtlog`); `host.name` from `--host`; `rtlog.contract` = `mismatch` when decoding was forced with a contract that does not match |

| rtlog | `severityNumber` | `severityText` |
| --- | ---: | --- |
| `trace` | 1 | `TRACE` |
| `debug` | 5 | `DEBUG` |
| `info` | 9 | `INFO` |
| `warn` | 13 | `WARN` |
| `error` | 17 | `ERROR` |
| Dropped frames (`rtlog.dropped`, with `rtlog.dropped` = count) | 13 | `WARN` |
| A gap in a damaged stream (`rtlog.gap`, with `rtlog.gap.offset`, `rtlog.gap.skipped`) | 17 | `ERROR` |
| A crash: panic or fatal signal (`rtlog.crash`) | 21 | `FATAL` |
| A snapshot (`rtlog.snapshot`) | 9 | `INFO` |

64-bit integers are strings, as the OTLP JSON encoding requires. An unsigned argument above 9 223 372 036 854 775 807 does not fit OTLP's signed integer and is written as a string value.

Spans are not exported: in OpenTelemetry they are traces, a different signal, and that export is on the [roadmap](roadmap.md) (section 10). Use `--out perfetto` to look at spans today.

## Time of day

A target's clock counts from an arbitrary start: boot, or a CPU counter. The capture holds those times and not the time of day. Give one of:

| Option | Meaning |
| --- | --- |
| `--start <time>` | The wall-clock time of the first event of the capture: about when the program started logging |
| `--epoch <time>` | The wall-clock time of the target's time zero, when you know it |

If the target's clock already counts from 1970-01-01, use `--epoch 0`. `<time>` is Unix seconds or an RFC 3339 time (`2026-11-17T09:30:00Z`). With neither option the records have no `timeUnixNano`; a backend then shows them at the time it received them, or at 1970.

## Into a collector

The OpenTelemetry Collector (contrib distribution) reads the file with its `otlpjsonfile` receiver:

```yaml
receivers:
  otlpjsonfile:
    include: ["/var/log/app.otlp.jsonl"]
exporters:
  debug: {}
service:
  pipelines:
    logs:
      receivers: [otlpjsonfile]
      exporters: [debug]
```

Each line is also a valid body for an OTLP/HTTP request (`POST /v1/logs`, `Content-Type: application/json`). `rtlog` does not send it itself yet; that, and following a capture while it grows, are on the [roadmap](roadmap.md) (sections 10 and 12d).

The output is checked in CI field by field against the OTLP JSON encoding. It has not been run through a collector yet; if one refuses a line, please open an issue with the line.

## From Rust

```rust
use rtlog_decode::{otlp, Contract, Options};

fn to_otlp(contract_jsonl: &str, capture: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    let contract = Contract::from_jsonl(contract_jsonl)?;
    let cfg = otlp::Config {
        service_name: "motor".into(),
        host_name: "gw1".into(),
        start_ns: rtlog_decode::syslog::parse_epoch("2026-11-17T09:30:00Z"),
        ..otlp::Config::default()
    };
    Ok(otlp::decode(&contract, capture, &Options::default(), &cfg)?.text)
}
```

`rtlog_decode::decode` with `Output::OtlpJson` uses `Config::default()`.
