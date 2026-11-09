# rtlog

**Fast, tiny, structured logging and tracing for real-time code, from Cortex-M to Linux.**

You write `println!`-style log lines. The device writes a few binary bytes into a lock-free ring: no formatting, no allocation, no lock. You read text, JSON or a [Perfetto](https://ui.perfetto.dev) timeline on the host.

[![ci](https://github.com/NieWil/rtlog/actions/workflows/ci.yml/badge.svg)](https://github.com/NieWil/rtlog/actions/workflows/ci.yml)
[![coverage](https://codecov.io/gh/NieWil/rtlog/graph/badge.svg)](https://codecov.io/gh/NieWil/rtlog)
[![crates.io](https://img.shields.io/crates/v/rtlog.svg)](https://crates.io/crates/rtlog)
[![docs.rs](https://img.shields.io/docsrs/rtlog)](https://docs.rs/rtlog)
![no_std](https://img.shields.io/badge/no__std-yes-2ea44f)
![languages](https://img.shields.io/badge/Rust%20%C2%B7%20C%20%C2%B7%20C%2B%2B%20%C2%B7%20Zephyr-informational)
![status](https://img.shields.io/badge/status-v0.2%20preview-orange)
![licence](https://img.shields.io/badge/licence-MIT%20%2F%20Apache--2.0-blue)

![What you write, what goes on the wire, what you read](docs/images/hero.svg)

## Why

- **Cheap on the device.** A log call takes 36 cycles (11 ns) on x86-64 and 46 on arm64, 6–8× less than formatting the line. On Cortex-M4F, 10 messages take 3.3 KB of flash against 18.3 KB with `core::fmt`. [Numbers →](docs/performance.md)
- **Never in the way.** Each thread or task owns its ring. A full ring drops and counts; it never blocks an ISR or a control loop.
- **More than text.** Spans, typed fields and per-thread timelines export straight to Perfetto. [See a real app traced →](docs/use-cases/perfetto.md)
- **One format everywhere.** Rust (`no_std` and `std`), C, C++ and Zephyr all write the same stream and use the same decoder.

## Try it

`cargo add rtlog`, then:

```rust
use std::{fs::File, io::BufWriter, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The format strings, written once; the decoder needs them.
    rtlog::contract::write("app.contract.jsonl")?;
    // A background thread moves frames from the rings to a file.
    let file = BufWriter::new(File::create("app.rtlog")?);
    let drain = rtlog::spawn_drain(rtlog::IoTransport(file), Duration::from_millis(10));

    for rpm in [1200u32, 1450, 1710] {
        rtlog::info!("motor speed={} rpm", rpm);
    }
    rtlog::warn!("temperature={} C", 81.5f32);

    // Flush what is still in the rings before exiting.
    let mut out = drain.stop();
    rtlog::Transport::flush(&mut out);
    Ok(())
}
```

Run it, then decode the capture with the CLI (`cargo install rtlog-decode`):

```text
$ rtlog decode --contract app.contract.jsonl app.rtlog
0.000000 [main] INFO  motor speed=1200 rpm
0.000002 [main] INFO  motor speed=1450 rpm
0.000002 [main] INFO  motor speed=1710 rpm
0.000002 [main] WARN  temperature=81.5 C
4 records, 0 spans, 1 contexts, 0 dropped
```

Without a drain, log calls fill the rings and nothing leaves the process. Two shortcuts while developing:

```rust
// Text on stdout while the program runs, like env_logger (cargo add rtlog-decode; RTLOG=debug).
let _log = rtlog_decode::stdout::init();
// Unix: keep the last records if the process panics or crashes.
rtlog::crash::install("crashes/")?;
```

Spans give each request or control-loop step a slice in [Perfetto](docs/use-cases/perfetto.md):

```rust
fn handle(req: &Request) {
    let _span = rtlog::span!("request");
    rtlog::debug!("route={} bytes={}", req.route, req.len);
}
```

The examples live in this repository; clone it first:

```sh
git clone https://github.com/NieWil/rtlog && cd rtlog
cargo run --release -p rtlog-examples --example perfetto_quickstart
# open target/quickstart.perfetto.json in https://ui.perfetto.dev
```

| Example | Shows |
| --- | --- |
| [`basic`](examples/basic.rs) | The flow above with several threads: drain to a file, contract, decode |
| [`perfetto_quickstart`](examples/perfetto_quickstart.rs) | Threads, spans and counters to a Perfetto trace in about 60 lines |
| [`app_trace`](examples/app_trace.rs) | A traced app: 1 kHz control loop, worker pool, a lock-order deadlock caught by a watchdog, heap counters |
| [`static_rings`](examples/static_rings.rs) | The `no_std` API: static rings, explicit producers, idle-loop drain |
| [`c/examples`](c/examples) | The same from C and C++ |
| [`stdout_mode`](examples/stdout_mode.rs) | Read logs as text on stdout, like `env_logger`, with binary log calls |
| [`timing`](examples/timing.rs) | Cost per call in ns and cycles on your machine |

## How it works

![Threads write into their own rings; the drain sends frames to the host, which decodes them](docs/images/architecture.svg)

Each call site gets an ID at build time; its format string goes into a contract. The program can write the contract itself (`rtlog::contract::write`, any OS), or the host reads it from an ELF binary (`rtlog contract --elf`, Linux and firmware). [Architecture →](docs/architecture.md)

## Guides

| | |
| --- | --- |
| [Perfetto tracing](docs/use-cases/perfetto.md) | Follow an application thread by thread, down to the microsecond |
| [When the process crashes](docs/crash.md) | Panics, segfaults, division by zero, abort: what is kept and what to do |
| [Embedded Rust](docs/embedded.md) | `no_std` setup, linker snippet, getting the contract from firmware |
| [C and C++](docs/c-cpp.md) | Header-only producer, build, capture, decode |
| [Zephyr](docs/zephyr.md) | Module, Kconfig, transports |
| [Configuration and CLI](docs/reference.md) | Levels, environment variables, features, every `rtlog` command and option, the JSON output |
| [Performance and size](docs/performance.md) | Cycles per call, flash and RAM, measuring on your board |
| [Results from users' boards](results/README.md) | Timing reports from real hardware; [add yours](CONTRIBUTING.md#share-your-boards-results) |
| [Demo on your device](docs/demo.md) | Prebuilt per architecture: copy to a board, run one script, open the trace on your computer |
| [Specification](docs/spec.md) · [Roadmap](docs/roadmap.md) · [Implementation notes](docs/implementation.md) | What is built, what is planned, and where the code deviates |

## Get it

- **CLI** (`rtlog decode`, `rtlog contract`): prebuilt for Linux (x86-64, x86, arm64, ARMv7, ARMv6, RISC-V 64), macOS and Windows on the [releases page](https://github.com/NieWil/rtlog/releases). Or build it: `cargo install rtlog-decode`.
- **Debian / Ubuntu** (amd64, i386, arm64, armhf), from the releases page (since 0.2.5):

  ```sh
  sudo apt install ./rtlog_<version>-1_amd64.deb      # the rtlog command and its man page
  sudo apt install ./rtlog-dev_<version>-1_all.deb    # rtlog.h for C/C++, with pkg-config
  ```

- **Rust**: `cargo add rtlog` (for bare metal: `cargo add rtlog --no-default-features`).
- **Demo**: `rtlog-demo-<version>-<target>` on the releases page runs the timing report and a traced app on any of those machines. [How →](docs/demo.md)
- **C / C++ / Zephyr**: the `rtlog-c` package on the releases page, or [`c/include/rtlog.h`](c/include/rtlog.h) and the [Zephyr module](docs/zephyr.md).

## Status

v0.2 preview. Every CI run tests it on Linux, macOS, Windows, and on arm64, ARMv7, RISC-V and x86 under QEMU. Each run also includes a 2-minute soak test, Miri, and Thread and Address sanitizers. Not yet there: `derive(Log)`, the `log` / `tracing` / `defmt` bridges, per-module levels, and stripping format strings from shipped binaries. [Roadmap →](docs/roadmap.md)

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT licence](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in rtlog by you, as defined in the Apache-2.0 licence, shall be dual licensed as above, without any additional terms or conditions.
