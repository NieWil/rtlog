# Roadmap

What rtlog does not do yet. The [specification](spec.md) describes only what is implemented; everything that was planned in earlier spec versions lives here, grouped by the spec section it belongs to, so a reader of the spec never mistakes a plan for a feature. Items are unordered within a section; the release plan at the end says what comes first.

## 0. API and developer experience

- `#[derive(Log)]` for structs and enums, with field names in the contract so the host prints `Header { src: 2, dst: 3 }`.
- A `Display` / `Debug` escape hatch (`{:display}`, `{:debug}`, `std` only): format into a stack buffer, send as a capped dynamic string, lint-warned.
- Interned static strings (`intern!`, `&'static str` literals as a 4-byte ID).
- Per-module runtime levels (`RTLOG=mycrate::net=debug`), like `env_logger`.
- `log` and `tracing` backends (`rtlog-log`, `rtlog-tracing`) so dependencies' logs flow into the same stream; a `defmt::Logger` for defmt-instrumented dependencies.
- An `rtlog::init()` convenience for `std` applications (drain plus crash handler plus contract file in one call).
- Contract delivery: a contract store keyed by hash (`~/.rtlog/contracts/`), optional contract embedded in the stream for `std` targets where size does not matter, and `--follow` live decoding.

## 4. Contract and site IDs

- Stripping for Mach-O and PE binaries, and for Linux programs linked with LLD (built: ELF firmware with either linker, Linux programs with GNU ld; [guide](strip.md)).
- A contract hash in the header of a stripped Rust build (today 0, "not stated"): patch it into the image after linking, or keep the 4-byte IDs loaded.
- A build-time collision check across the whole program (today a unit test checks the crate's own sites).
- `elf16`: linker-assigned 16-bit IDs for single-image MCU builds (header `scheme = 1`, reserved).
- Argument types in the site hash, once a proc-macro can see them.
- `cargo rtlog contract`: merged contract with `kind: "type"` entries for derived types.

## 5. Wire format

- A time-based resync (at least once a second) on top of the byte-based one, so a slow, sparse stream also recovers quickly; resync with the full 64-bit timestamp of the next frame instead of per-context `ctx_time` frames.
- Resync from the `no_std` drain helpers automatically (today the application calls `write_resync`).
- `time_sync` (`..06`, reserved): ticks to external time (RTC, GPS, NTP).
- Drain-side compression (`flags` bit 0): per-frame leading-zero stripping or a block codec.
- Authenticated and encrypted blocks (`flags` bits 1–2, `block_tag` `..08`, `key_id`): ChaCha20-Poly1305 per block between two resyncs, nonce from a persisted boot counter plus a block counter, keys from a `KeyStore` trait over a secure element or a flash slot. Threats V3, V4, V8 and V12 depend on this.

## 6. Argument model

- Tier 2 (interned) and tier 4 (`Display`) as above.
- `#[rtlog(redact)]`: a 32-bit hash instead of the value (threat V7), and a lint on `{:str}` / `{:display}` applied to identifiers named `*key*`, `*token*`, `*secret*`, `*password*`.
- A per-site string cap override (`{:str<N>}`); today every dynamic string is capped at 127 bytes.
- Truncate the arguments of an oversized call instead of dropping the whole record.

## 7. Runtime

- `DropOldest` overflow policy (the ring already has the per-frame length it needs).
- An ISR ring convention and example on bare metal (context 0).
- Embassy and Tokio integrations: a span per task so a task can be followed across threads.
- A runtime-configurable `std` ring size (today `THREAD_RING` is 64 KiB, fixed at build time).
- A re-entrancy guard on the producer, so a `Log` impl that logs cannot corrupt its own ring.
- A producer-side high-water mark that wakes the drain early (`AtomicBool` + `unpark`).

## 8. Transports

- UDP (one datagram per batch, resync every datagram), shared memory (`memfd`, producer-owned `0600` segment, read-only consumer mapping; threat V10), DMA-driven UART on MCU.
- RTT transport for Rust `no_std` (the Zephyr module already writes to the console UART and can be pointed at RTT).
- Drain policy "most pending bytes first" (today: registration order).

## 8b. Crash log on MCU

- Capture the fault context from the exception frame on Cortex-M (HardFault, UsageFault, panic; `pc`, `lr`, `MMFAR` / `BFAR`, `CFSR`, `SP` plus the top 64 words of stack) into a reserved flash sector or a `.noinit` RAM region, read back after reset.
- `rtlog crash dump.rtlog --elf firmware.elf`: print the last lines before the fault and symbolise `pc`, `lr` and the stack with the ELF's debug info.
- `cargo rtlog run`: flash, attach, decode live, and offer the previous crash found in `.noinit` at attach time.
- On Linux: a stack of return addresses via frame pointers in the crash record; the crash sink encrypted when the stream is (threat V12).
- Stdout mode on MCU through RTT or semihosting from the drain.

## 9. Host decoder

- A reorder window when merging contexts (today items are sorted by timestamp after decoding the whole capture; the stdout mode prints in arrival order).
- Live mode: `rtlog decode --follow` on RTT, a serial port, UDP or a growing file.
- Contract auto-location by hash.
- `cargo fuzz` targets for the decoder in CI (today: a mutation test over corrupted and truncated streams, `tests/malformed.rs`).

## 10. Ecosystem bridges

Host-side outputs beyond text, JSON lines and Perfetto: syslog / journald, OpenTelemetry Logs (`--out otlp`, with a live pipe into a collector), CTF for Trace Compass, SEGGER SystemView, `defmt-print` text. Target-side: the `log`, `tracing` and `defmt` backends listed under section 0.

## 10b. Threats still open

| # | Threat | Blocked on |
| --- | --- | --- |
| V3, V4 | Capture on an untrusted transport; tampering in archives | AEAD blocks (section 5) |
| V7 | Sensitive values in typed args | `#[rtlog(redact)]` and the lint (section 6) |
| V8 | Nonce reuse after reset | boot counter in the AEAD design (section 5) |
| V10 | Shared-memory ring written by another process | the shared-memory transport (section 8) |
| V12 | `.noinit` crash sink readable later | MCU crash capture and encryption (section 8b) |

## 10c. Benchmarks

- A cycle-count test on a Cortex-M4 board in CI through `probe-rs`. A first manual measurement exists ([STM32L432KC at 80 MHz](stm32-comparison.md): 447 median cycles for one `u32` through the C/Zephyr path, about three times the spec's ≤ 150-cycle placeholder); the budget stays until the path is optimised.
- Optimise the C and bare-metal log path: it costs hundreds of cycles on a Cortex-M4 where the Rust host path costs 36–46 on x86-64 and arm64. Measure a W1-shaped case (two arguments) and the Rust `no_std` path on the same board.
- Comparisons against defmt, delog, NanoLog, spdlog, `tracing` and `env_logger` on the same machines, with one harness per workload (W1–W7; W6 `isr` and W7 `control loop` are not written yet).
- A report script collecting results into one table per release, with the commit hash and board revision.

## 12. Adoption: a representative application, and being easy to find and try

The numbers rtlog publishes today come from micro-benchmarks (`examples/timing.rs`) and a size probe with 10 call sites (`size/mcu`). Neither tells a reader what rtlog costs in a program like theirs. Each topic below advances in small steps; every step is one pull request and leaves something usable.

**12a. A real application, ported** (what it costs in flash, RAM, cycles and bytes on the wire)

Take existing open-source programs that already log, and port their log calls, so the baseline is code rtlog's authors did not write. The port is at source level: a call site's descriptor is created by the compiler, so an already built binary cannot be converted.

1. Pick the references and record why. The best reference is a program where the log is essential (operators or engineers cannot do without it) and that has a performance requirement the logger must not break; it must also log and trace a lot, because that is where a logger's cost shows. One per scale, from a microcontroller to a Linux server. Criteria: at its verbose level the program emits thousands of records per second (or fills its log buffer and drops); more than 100 call sites; several threads or tasks; a licence that allows a fork; builds in CI.

   Candidates in order of popularity (GitHub stars on 2026-11-16), to be confirmed against those criteria when step 1 is done: record rates and call-site counts have not been measured yet.

   | Stars | Candidate | Scale, language | Logger today | Log essential? | Performance requirement |
   | ---: | --- | --- | --- | --- | --- |
   | 51 k | ClickHouse | Linux server, C++ | Poco-based, per query and per part | Yes: query and server logs are how it is operated | Query latency; very large build |
   | 50 k | Bevy (`trace` feature) | Linux application, Rust | `tracing` spans | No: a profiling aid | Frame time; tens of thousands of spans per second |
   | 36 k | Qdrant | Linux server, Rust | `log` | Yes | Search latency |
   | 29 k | Envoy | Linux proxy, C++ | spdlog, access log per request | Yes: the access log is a product feature | Per-request latency across worker threads |
   | 28 k | Pingora | Linux proxy, Rust | `log`, access log per request | Yes: same as Envoy | Per-request latency on a multi-threaded runtime |
   | 28 k | Valkey | Linux server, C | Own `serverLog` | Partly: sparse at default level | Single-threaded command latency |
   | 23 k | Vector | Linux pipeline, Rust | `tracing` | Yes | Throughput in events per second |
   | 17 k | TiKV | Linux database, Rust | slog with an async drain | Yes: a distributed database is diagnosed from its logs | Write and read latency |
   | 17 k | Zephyr: network `echo_server` or a Bluetooth sample with the stack at debug level | MCU, C | `LOG_*`, deferred | Yes during bring-up and field diagnosis | Radio and interrupt timing; small flash and RAM |
   | 13 k | PX4 autopilot | Flight controller (NuttX), C++ | `PX4_INFO`, uORB flight log | Yes: the flight log is the black box | Control loops at 250 Hz to 1 kHz, hard real time |
   | 10 k | Embassy examples with `defmt` at trace level | MCU, Rust | defmt over RTT | During development | Interrupt latency; one global buffer |
   | 5 k | Nav2 (ROS 2 navigation) | Linux robot, C++ | `RCLCPP_*` | Yes | Control and planning rates |
   | 5 k | `tokio-rs/mini-redis` | Linux service, Rust | `tracing` | No: a teaching project | None stated; small enough to port by hand |

   Start with three where the log is essential and performance is a requirement, one per scale: Pingora on Linux (Rust, every request writes an access log on the latency path), a Zephyr sample with its network or Bluetooth stack at debug level on a microcontroller (runs in the QEMU job), and PX4 for hard real time (its log is the flight recorder). Envoy is the C++ counterpart of Pingora. `mini-redis` stays useful as a first, small port to settle the method before the large ones.
2. Measure each reference as it is, with its own logger: flash by section, static RAM, cycles per call for its three most frequent call sites, bytes per second on its transport. Commit the method and the numbers (`docs/reference-apps.md`).
3. Port the Zephyr reference to `rtlog.h`. Report the same four numbers for three builds: original logger, rtlog, rtlog with `CONFIG_RTLOG_STRIP=y`. Keep the port as a patch series in the repository, applied in CI on a pinned upstream revision, so the comparison is reproducible and does not rot silently.
4. Port the defmt firmware the same way (Rust `no_std`, `strip` feature).
5. Port the Linux service once the `tracing` bridge exists (section 0); until then, port by replacing macros.
6. Publish one table per release from these builds, next to the micro-benchmarks, with compiler, board and commit.

What makes step 3 cheap: printf-style placeholders (`%d`, `%s`, `%08x`) accepted in C format strings and rendered by the decoder, so a `LOG_INF("x=%d", x)` becomes `RTLOG_INFO("x=%d", x)` with a search and replace. Today the C macros take `{}` only.

A bridge for the `log` crate does not show the saving: `log` hands the logger an already formatted `fmt::Arguments`, so the formatting cost stays on the device. It is useful to collect dependencies' logs in one stream, not as a benchmark.

**12b. Comparisons** (extends section 10c)

1. One harness, one workload (W1), three loggers on the host: rtlog, `tracing` with a non-blocking writer, spdlog async. Cycles per call and bytes per record.
2. The same on a Cortex-M4 board against defmt and Zephyr deferred logging (the STM32L432KC setup of [stm32-comparison.md](stm32-comparison.md)).
3. The remaining workloads (W2–W7), then the table in the README, regenerated by CI.

**12c. Zero-rewrite adoption** (sections 0 and 10)

1. `rtlog-log`: a `log::Log` backend. 2. `rtlog-tracing`: a `tracing` subscriber with spans mapped to rtlog spans. 3. A `defmt::Logger`. 4. printf-style placeholders in C (see 12a).

**12d. Live workflow** (sections 8b and 9)

1. `rtlog decode --follow` on a growing file and on stdin. 2. On a serial port. 3. On RTT through probe-rs. 4. `cargo rtlog run`: flash, attach, decode live.

**12e. Installation**

1. Homebrew tap for the CLI (macOS, Linux). 2. winget or scoop manifest (Windows). 3. vcpkg and Conan recipes for `rtlog.h`. 4. A west manifest snippet and a listing among Zephyr's external modules. 5. PlatformIO and ESP-IDF component registry entries.

**12f. First impression and trust**

1. A Perfetto screenshot of `examples/app_trace.rs` at the top of the README. 2. Repository topics and a social preview image. 3. `SECURITY.md`, a minimum supported Rust version stated and checked in CI. 4. Issues labelled for first contributions, taken from this roadmap. 5. A written stability promise for the wire format and the contract schema ahead of 1.0.

## Release plan

1. **v0.3**: `derive(Log)` and the `Display` escape hatch; the `log` bridge; inline-capture support or a `{name}` rewrite; `--follow` on files and stdin (12d.1); printf-style placeholders in C and the Zephyr reference application measured and ported (12a.1–3); README screenshot and package-manager installs for the CLI (12e.1–2, 12f.1–2).
2. **v0.4**: `tracing` bridge; per-module levels; contract store by hash; `DropOldest`; the defmt firmware and the Linux service ported (12a.4–5); host comparison harness (12b.1); `--follow` on serial and RTT (12d.2–3).
3. **v0.5**: MCU crash capture; UDP and shared-memory transports; `elf16` IDs; Cortex-M cycle test on hardware; board comparison against defmt and Zephyr logging (12b.2); `cargo rtlog run` (12d.4); vcpkg, Conan and Zephyr module listing (12e.3–4).
4. **v1.0**: wire format frozen, contract schema versioned and the stability promise written (12f.5), bench suite published with a hardware-measured Cortex-M budget and the reference-application table per release (12a.6, 12b.3).

Security (AEAD, key provisioning) comes after 1.0 unless a user needs it first.
