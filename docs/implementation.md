# Implementation notes — v0.1

This file tracks how the code in `crates/` matches [the specification](spec.md): what is built, where it deviates, and what is left.

## What is built

| Spec section | Status | Where |
| --- | --- | --- |
| 0 API: `trace!` … `error!`, `set_level`, `stats`, `spawn_drain`, `poll` | Built | `rtlog-core` `lib.rs`, `runtime.rs` |
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
| 5 resync, compression, AEAD; 8b crash log; 10 `log` / `tracing` / `defmt` bridges and OTLP / CTF outputs; shared memory | Not yet | — |

## Deviations from the spec

1. **Argument types travel on the wire.** Each argument is one tag byte plus its value, and the contract records the argument count (`nargs`) instead of the type list. A `macro_rules!` macro cannot see argument types at expansion time, and a `static` cannot depend on them. The site ID therefore hashes module, file, line, column, format string, level and `nargs`. Cost: one byte per argument.
2. **The contract is read from the running binary.** Sites are collected with `linkme` into a link section, and `rtlog::contract::write(path)` writes the JSON-lines file. Post-build extraction and stripping (`cargo rtlog contract`, `cargo rtlog verify`) are not built yet, so **format strings are still present in v0.1 binaries** and threat V2 is open.
3. **Not yet in the argument model:** `#[derive(Log)]`, `intern!`, `{:display}` / `{:debug}` (tier 4), and `#[rtlog(redact)]`.
4. **Dynamic strings and byte slices are capped at 127 bytes.** The length byte uses bit 7 as the truncation flag, so the spec's `{:str<256>}` override cannot be encoded. Truncation keeps strings on a UTF-8 boundary.
5. **Ring frames carry a 2-byte length prefix inside the ring** so the drain always hands whole frames to the transport. It never goes on the wire. Only `DropNewest` is implemented.
6. **One log call holds at most 240 argument bytes** (`ARGS_CAP`). A larger call is dropped and counted as a drop.
7. **The runtime level is global,** not per crate.
8. **`RTLOG_LEVEL` at build time** removes the call path of sites below the floor; their descriptors stay in the contract.
9. **On `no_std`, call sites name their producer:** `rtlog::info!(to: &mut producer, ...)`. On `std` the calling thread's ring is used implicitly.
10. **Default `std` clock** is the CPU counter: invariant TSC (halved, so the rate fits the header's `u32`) on x86-64, `CNTVCT_EL0` on aarch64, otherwise `Instant`. `RTLOG_CLOCK=instant` forces `Instant`. The TSC rate is calibrated for 20 ms on the first drain.
11. **Text output shows seconds since the first record** in the stream; JSON keeps absolute `ts_ns`.
12. **The decoder stops at the first bad frame** and reports its byte offset, since there are no resync frames yet.
13. **Span frames carry a timestamp delta and context**, like log frames, followed by the spec's payload (span site ID, span ID, parent). Span names are span sites in the contract (`"kind":"span"`), not interned strings. Span IDs are context (16 bits) plus a per-context counter (48 bits). On `std` the parent is the thread's current span; with an explicit producer, `span_enter` / `span_exit` track it.
14. **Registration does not block the drain.** A thread's first log call registers its ring through a separate pending list, and runtime counters are plain atomics. So a transport may log, call `stats()`, or start threads while the drain is inside `poll`. A regression test covers it.
15. **Text output shows drops and spans** as `DROP` and `SPAN ▶ / ◀` lines, with span durations. Perfetto output turns numeric fields named in the format string (`err={}`) into counter tracks.

## Performance status

The CI `timing` job prints the latest numbers to its summary page. On the GitHub runner (x86-64, release build), W1 measured 21.6 ns p50 per call against the spec target of ≤ 15 ns; about 13 ns of that is the TSC read. A timestamped `writeln!` into a buffer measured 67.5 ns, and a disabled site 0.4 ns. Closing the gap is open work: candidates are writing straight into the ring instead of through a stack buffer, and a cheaper thread-local lookup.
