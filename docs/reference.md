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
| Crash dump | `rtlog::crash::install(dir)` on Unix | not yet ([roadmap](roadmap.md)) |

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
| `--out text\|json\|perfetto` | Output format. Default `text`. `--json` and `--perfetto` are short forms; `chrome` is accepted for `perfetto`. |
| `-o <file>`, `--output <file>` | Write to a file instead of stdout. |
| `--color auto\|always\|never` | Colours in text output. `auto`: on a terminal unless `NO_COLOR` is set, never with `-o`. |
| `--no-spans` | Leave span open/close records out. |
| `--force` | Decode even if the stream's contract hash does not match the contract. |

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

Reads the site descriptors from an **ELF** binary (Linux programs, embedded firmware) and writes the contract. Mach-O (macOS) and PE (Windows) binaries are not read; there, have the program call `rtlog::contract::write(path)` at startup.

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
