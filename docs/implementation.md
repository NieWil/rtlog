# Implementation notes — v0.2

This file tracks how the code in `crates/` matches [the specification](spec.md): what is built, where it deviates, and what is left.

## What is built

| Spec section | Status | Where |
| --- | --- | --- |
| 0 API: `trace!` … `error!`, `set_level`, `stats`, `spawn_drain`, `poll` | Built | `rtlog` `lib.rs`, `runtime.rs` |
| 4 Site IDs, link-section descriptors, contract file | Built (see deviations 1–2) | `site.rs`, `contract.rs` |
| 5 Stream header `RTL1`, log frames, `dropped` / `ctx_register` / `ctx_close` | Built | `wire.rs`, `drain.rs` |
| 6 Argument model, tiers 1 and 3 | Built (see deviations 3–4) | `arg.rs` |
| 7 SPSC rings, `DropNewest`, clock trait, levels | Built | `ring.rs`, `clock.rs`, `level.rs` |
| 8 `Transport`, drain scheduling, backpressure | Built | `drain.rs`, `runtime.rs` |
| 9 Decoder: validation, untrusted parsing, text and JSON lines, CLI | Built | `rtlog-decode` |
| 5 / 10 Spans: `span_open` / `span_close`, 64-bit IDs, explicit parent | Built (see deviation 13) | `span!`, `Producer::span_enter` |
| 10 Perfetto / Chrome trace output | Built | `rtlog decode --perfetto`, `rtlog-decode/src/perfetto.rs` |
| 10c Benchmark model: workloads W1–W5, timing report, Criterion | Built (host only) | `examples/timing.rs`, `benches/` |
| Verification: soak test, Miri, ThreadSanitizer, AddressSanitizer | Built | `tests/soak.rs`, CI jobs `miri`, `sanitizers`, `soak` |
| 4 Contract from the image (`cargo rtlog contract`) | Built as `rtlog contract --elf`, for Rust and C descriptors, PIE relocations applied | `rtlog-decode/src/elf.rs` |
| 8b Debug stdout, crash log (Unix; C on POSIX) | Built: one numbered file per dump, rings read without waiting for the drain | `rtlog_decode::stdout`, `rtlog::crash`, `rtlog_crash_install` |
| C / C++ producer, Zephyr module | Built (Zephyr not CI-built) | `c/include/rtlog.h`, `c/zephyr`, `zephyr/module.yml` |
| 10c Benchmarks in cycles, size on Cortex-M4F | Built | `examples/timing.rs`, `size/mcu`, `scripts/size_report.py` |
| 5 compression, AEAD; 8b MCU crash capture; 10 `log` / `tracing` / `defmt` bridges and OTLP / CTF outputs; shared memory; contract stripping | Not yet | [Roadmap](roadmap.md) |

## Deviations from the spec

1. **Argument types travel on the wire.** Each argument is one tag byte plus its value, and the contract records the argument count (`nargs`) instead of the type list. A `macro_rules!` macro cannot see argument types at expansion time, and a `static` cannot depend on them. The site ID therefore hashes module, file, line, column, format string, level and `nargs`. Cost: one byte per argument.
2. **The contract comes from the binary's descriptors.** Sites are collected with `linkme` into a link section. `rtlog::contract::write(path)` writes the JSON-lines file at runtime (`std`), and `rtlog contract --elf <image>` reads the same descriptors from any ELF image, firmware included. Stripping them (`cargo rtlog verify`) is not built yet, so **format strings are still present in v0.2 binaries** and threat V2 is open.
3. **Not yet in the argument model:** `#[derive(Log)]`, `intern!`, `{:display}` / `{:debug}` (tier 4), and `#[rtlog(redact)]`.
4. **Dynamic strings and byte slices are capped at 127 bytes.** The length byte uses bit 7 as the truncation flag, so the spec's `{:str<256>}` override cannot be encoded. Truncation keeps strings on a UTF-8 boundary.
5. **Ring frames carry a 2-byte length prefix inside the ring** so the drain always hands whole frames to the transport. It never goes on the wire. Only `DropNewest` is implemented.
6. **One log call holds at most 240 argument bytes** (`ARGS_CAP`). A larger call is dropped and counted as a drop.
7. **The runtime level is global,** not per crate. Spans have their own switch (`set_spans`), independent of the level, so a `warn`-level build keeps its Perfetto timeline.
8. **`RTLOG_LEVEL` at build time** removes the call path of sites below the floor; their descriptors stay in the contract.
9. **On `no_std`, call sites name their producer:** `rtlog::info!(to: &mut producer, ...)`. On `std` the calling thread's ring is used implicitly.
10. **Default `std` clock** is the CPU counter: invariant TSC (halved, so the rate fits the header's `u32`) on x86-64, `CNTVCT_EL0` on aarch64, otherwise `Instant`. `RTLOG_CLOCK=instant` forces `Instant`. The TSC rate is calibrated for 20 ms on the first drain.
11. **Text output shows seconds since the first record** in the stream; JSON keeps absolute `ts_ns`.
12. **Resync is byte-based and per context.** The drain writes a resync frame every 64 KiB of frames (not also every second), followed by `ctx_register` and `ctx_time` frames, rather than one frame with a single timestamp. After a bad frame the decoder resumes at the next resync frame; with none after it, it stops at the bad frame and reports its byte offset. Times inside a gap are lost with its frames; after it they are exact. On `no_std` the application calls `drain::write_resync` itself.
13. **Span frames carry a timestamp delta and context**, like log frames, followed by the spec's payload (span site ID, span ID, parent). Span names are span sites in the contract (`"kind":"span"`), not interned strings. Span IDs are the full 32-bit context plus a 32-bit per-context counter, so IDs stay unique in a process that creates many threads over its life. On `std` the parent is the thread's current span; with an explicit producer, `span_enter` / `span_exit` track it.
14. **Registration does not block the drain.** A thread's first log call registers its ring through a separate pending list, and runtime counters are plain atomics. So a transport may log, call `stats()`, or start threads while the drain is inside `poll`. A regression test covers it.
15. **Text output shows drops and spans** as `DROP` and `SPAN ▶ / ◀` lines, with span durations. Perfetto output turns numeric fields named in the format string (`err={}`) into counter tracks.
16. **C and C++ site IDs are descriptor indices**, not content hashes: C has no compile-time string hashing. The ID is the descriptor's position in the `rtlog_sites` section plus one, so the log path does no hashing; `rtlog contract --elf` numbers the entries the same way.
17. **Bare-metal descriptors** live in `linkme_SITES`; with `cortex-m-rt` a three-line `memory.x` addition keeps them in flash (see the embedded guide). The log path never reads a descriptor: a site's ID is a compile-time constant.

## Performance status

The spec's budgets (section 10c) are checked in CI by `scripts/spec_check.py`: ≤ 50 cycles per W1 call (x86-64 and aarch64 runners; reported and warned on, not gated, since shared runners vary) and, on Cortex-M4F, ≤ 2560 bytes of fixed runtime code and ≤ 64 bytes per call site (gated). The current figures are on the CI job pages and in [Performance and size](performance.md). On hosts the log call encodes its frame in place in the ring (82 instructions for W1 on x86-64); most of the remaining cost is the timestamp read itself (`rdtsc`, `CNTVCT_EL0`).
