# Configuration and CLI reference

What you can set, where, and what each `rtlog` command does. The design behind it is in the [specification](spec.md).

## Levels

Levels, lowest to highest: `trace`, `debug`, `info`, `warn`, `error`. A call is recorded when its level is at or above both filters:

| Filter | Set with | Default | Cost of a filtered call |
| --- | --- | --- | --- |
| Build time | `RTLOG_LEVEL=info cargo build` (`trace` … `error`, or `off`) | `trace`: everything compiled in | none: the call is compiled out |
| Run time | `rtlog::set_level(rtlog::Level::Warn)`; `rtlog::set_off()` stops everything | `trace`: everything recorded | one relaxed atomic load and a compare |

`rtlog::level::level()` returns the current runtime level (`None` when off). Arguments of a filtered call are not evaluated.

**Stdout mode** (`rtlog_decode::stdout::init()`) sets the runtime level from the `RTLOG` environment variable when it starts, and defaults to **`info`**, not `trace`, so `debug!` lines stay quiet until you ask for them: `RTLOG=debug cargo run`. `RTLOG=off` turns logging off.

**Spans** have their own switch: `rtlog::set_spans(false)` stops recording spans and leaves log lines alone. `set_off()` stops both.

C and C++: `rtlog_set_level(RTLOG_LEVEL_WARN)` with `RTLOG_LEVEL_TRACE` … `RTLOG_LEVEL_OFF`, and `rtlog_set_spans(0)`. See [C and C++](c-cpp.md).

## Environment variables

| Variable | Read | Effect |
| --- | --- | --- |
| `RTLOG_LEVEL` | when **building** your program | Lowest level compiled in (`trace` … `error`, `off`). |
| `RTLOG` | at `stdout::init()` | Runtime level for stdout mode. Default `info`. |
| `RTLOG_FIELDS` | at `stdout::init()` | What a line shows before the message: see [Choose what a text line shows](#choose-what-a-text-line-shows). |
| `RTLOG_CLOCK=instant` | first log call | Use `std::time::Instant` (nanoseconds) instead of the CPU counter (TSC ÷ 2 on x86-64, `CNTVCT_EL0` on aarch64). For machines whose counter is unreliable. |
| `NO_COLOR` | stdout mode, `rtlog decode` | No colours, even on a terminal. |
| `RTLOG_BOARD` | `timing` example, demo `run.sh` | Board name in the timing report. |

## Cargo features and platforms

| | `std` feature (default) | `--no-default-features` (bare metal) |
| --- | --- | --- |
| Rings | One per thread, created on its first log call | Static `Ring<N>`s you declare; `split(ctx)` gives a producer and a consumer |
| Drain | `rtlog::spawn_drain(transport, period)` thread, or `rtlog::Drain::new(t).poll()` where you choose | `rtlog::drain::drain_ring(&mut consumer, &mut transport, &mut scratch)` from your idle loop or a DMA callback, after `drain::write_header` |
| Clock | CPU counter or `Instant` | `rtlog::set_clock(now_fn, hz)` |
| Contract | `rtlog::contract::write(path)` at startup, or `rtlog contract --elf` | `rtlog contract --elf firmware.elf` |
| Crash dump | `rtlog::crash::install(dir)` on Unix and Windows | not yet ([roadmap](roadmap.md)) |

The `strip` feature (with or without `std`, ELF targets) keeps descriptors and format strings out of the shipped image; the contract then comes from `rtlog contract --elf` only. See [Ship without the format strings](strip.md).

Other runtime calls (`std`): `rtlog::set_context_name("motor")` names the calling thread's context in the output; `rtlog::stats()` returns live contexts and drop counts; `rtlog::set_clock(now, hz)` installs your own clock.

## The `rtlog` command

`cargo install rtlog-decode`, or a prebuilt binary from the [releases page](https://github.com/NieWil/rtlog/releases). `rtlog --help` and `rtlog <command> --help` print the usage.

### `rtlog decode`

```text
rtlog decode --contract <contract.jsonl> [options] <stream.rtlog>
```

| Option | Meaning |
| --- | --- |
| `--contract <file>` | The contract of the program that wrote the stream (required). |
| `--out text\|json\|perfetto\|syslog\|otlp-json\|journald` | Output format. Default `text`. `--json` and `--perfetto` are short forms; `chrome` is accepted for `perfetto`. `syslog`: RFC 5424 lines, see [Send logs to syslog](syslog.md). `otlp-json`: OpenTelemetry log records, see [Send logs to OpenTelemetry](opentelemetry.md). `journald`: systemd journal entries, see [Send logs to the systemd journal](journald.md). |
| `-o <file>`, `--output <file>` | Write to a file instead of stdout. |
| `--color auto\|always\|never` | Colours in text output. `auto`: on a terminal unless `NO_COLOR` is set, never with `-o`. |
| `--no-spans` | Leave span open/close records out. |
| `--fields <list>` | What a text line shows before the message: see [Choose what a text line shows](#choose-what-a-text-line-shows). |
| `--force` | Decode even if the stream's contract hash does not match the contract. |
| `--start <time>` | With `--out syslog`, `otlp-json` or `journald`: the wall-clock time of the capture's first event, as Unix seconds or an RFC 3339 time (`2026-11-17T09:30:00Z`). |
| `--epoch <time>` | With `--out syslog`, `otlp-json` or `journald`: the wall-clock time of the target's time zero. Takes precedence over `--start`. With neither, the records carry no time of day. |
| `--host <name>`, `--app <name>` | With `--out syslog`, `otlp-json` or `journald`: the machine and the program the capture came from (syslog host name and application; OpenTelemetry `host.name` and `service.name`; journal `_HOSTNAME` and `SYSLOG_IDENTIFIER`). Default: no host, `rtlog`. |
| `--syslog-facility <name\|0-23>` | With `--out syslog`: the facility (`user`, `daemon`, `local0` to `local7`, …). Default `user`. |
| `--syslog-sd-id <name>` | With `--out syslog`: the name of the structured-data element. Default `rtlog@32473`. |
| `--journald-socket <path>` | Send the journal entries to `systemd-journald` on this Unix socket (normally `/run/systemd/journal/socket`) instead of writing them; implies `--out journald`. Unix only. |

A summary goes to stderr: records, spans, contexts, drops and stream gaps. After a corrupted byte the decoder skips to the next resync frame (written every 64 KiB) and reports a gap instead of stopping. The exit status is non-zero when decoding stopped early.

### `rtlog check`

```text
rtlog check <contract.jsonl>
```

Validates a contract and prints its site count and hash, the same hash the stream header carries.

### `rtlog contract`

```text
rtlog contract --elf <binary> [-o <contract.jsonl>]
```

Reads the site descriptors from the binary that was built and writes the contract: **ELF** (Linux programs, embedded firmware), **Mach-O** (macOS; of a universal binary, the first architecture) or **PE** (Windows `.exe` and `.dll`). `--binary` is another spelling of `--elf`. Rust and C/C++ sites are both read. A Rust program can instead call `rtlog::contract::write(path)` at startup.

### `rtlog verify`

```text
rtlog verify --contract <contract.jsonl> <image>...
```

Checks an image that ships (a flashed `.bin`, an executable after `objcopy`) against the contract of its build: exits with an error if the image contains a format string or span name of the contract, and names each one found. Strings shorter than 6 bytes are not searched for. Use it as a release gate for [stripped builds](strip.md).

## Choose what a text line shows

You do not need this to start: a text line shows the time, the thread and the level, then the message.

```text
0.000412 [motor] INFO  pid out=0.42 err=-3
```

To show something else, give a list of fields. The order on the line is fixed; the list only says which fields appear.

| Field | Shows | In the default line |
| --- | --- | --- |
| `time` | Seconds since the first event, to the microsecond | yes |
| `ctx` | The name of the thread or task, in brackets | yes |
| `level` | The level | yes |
| `module` | The module path of the call (Rust) or its function (C) | no |
| `file` | The source file of the call | no |
| `line` | The source file and line of the call | no |

`default` stands for `time,ctx,level`, `all` for every field and `none` for the message alone.

| List | Line |
| --- | --- |
| `default` | `0.000412 [motor] INFO  pid out=0.42 err=-3` |
| `default,module` | `0.000412 [motor] INFO  app::motor pid out=0.42 err=-3` |
| `default,line` | `0.000412 [motor] INFO  src/motor.rs:42: pid out=0.42 err=-3` |
| `all` | `0.000412 [motor] INFO  app::motor src/motor.rs:42: pid out=0.42 err=-3` |
| `level,line` | `INFO  src/motor.rs:42: pid out=0.42 err=-3` |
| `none` | `pid out=0.42 err=-3` |

Where to give the list:

| Where | How |
| --- | --- |
| `rtlog decode` | `--fields default,line` |
| Stdout mode, without rebuilding | `RTLOG_FIELDS=default,line cargo run` |
| Stdout mode, in the program | `stdout::init_with(Config { fields: Some(Fields::parse("default,line").unwrap()), ..Config::default() })` |
| `rtlog_decode::decode_with` | `Options { fields: Fields::ALL, ..Options::default() }` |

The fields cost the program nothing: module, file and line come from the contract on the host, and the log call writes the same bytes whatever is shown. The choice applies to text output only; JSON lines, syslog, journald and OpenTelemetry always carry module, file and line. `rtlog decode` refuses a name it does not know; the stdout mode falls back to the default line, so a typing mistake in `RTLOG_FIELDS` cannot stop the program.

## JSON lines output

`rtlog decode --json` writes one JSON object per line. Every object has `type` and `ts_ns` (nanoseconds since the stream's time origin).

| `type` | Other fields |
| --- | --- |
| `log` | `level`, `message` (formatted), `args` (typed values), `ctx`, `ctx_name`, `site`, `module`, `file`, `line` |
| `span` | `name`, `dur_ns`, `id`, `parent` (0 at the top), `ctx`, `ctx_name` |
| `dropped` | `count` (calls lost since the last report, ring full or arguments too large), `ctx`, `ctx_name` |
| `gap` | `offset`, `skipped` (bytes), `reason`: the stream was damaged here and decoding resumed at the next resync frame |
| `crash` | `kind` (`panic`, `signal`, `snapshot`), `signal`, `code`, `addr`, `pc`, `message`, `description`, `ctx`, `ctx_name` |

Example:

```json
{"args":[1200],"ctx":1,"ctx_name":"main","file":"src/main.rs","level":"info","line":11,"message":"motor speed=1200 rpm","module":"app","site":2966966485,"ts_ns":1051015433825,"type":"log"}
```
