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

- Stripping the descriptor section from shipped images (`cargo rtlog verify` as a release gate) so format strings leave the binary. Until then, binaries are self-describing and threat V2 is open.
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
| V2 | Unstripped descriptor section ships format strings | stripping and `cargo rtlog verify` (section 4) |
| V3, V4 | Capture on an untrusted transport; tampering in archives | AEAD blocks (section 5) |
| V7 | Sensitive values in typed args | `#[rtlog(redact)]` and the lint (section 6) |
| V8 | Nonce reuse after reset | boot counter in the AEAD design (section 5) |
| V10 | Shared-memory ring written by another process | the shared-memory transport (section 8) |
| V12 | `.noinit` crash sink readable later | MCU crash capture and encryption (section 8b) |

## 10c. Benchmarks

- A cycle-count test on a Cortex-M4 board in CI through `probe-rs`, with a budget set from the first hardware measurement (the spec's ≤ 150 cycles is a placeholder until then).
- Comparisons against defmt, delog, NanoLog, spdlog, `tracing` and `env_logger` on the same machines, with one harness per workload (W1–W7; W6 `isr` and W7 `control loop` are not written yet).
- A report script collecting results into one table per release, with the commit hash and board revision.

## Release plan

1. **v0.3**: `derive(Log)` and the `Display` escape hatch; the `log` bridge; inline-capture support or a `{name}` rewrite; `--follow`.
2. **v0.4**: `tracing` bridge; per-module levels; contract store by hash; stripping and `cargo rtlog verify`; `DropOldest`.
3. **v0.5**: MCU crash capture; UDP and shared-memory transports; `elf16` IDs; Cortex-M cycle test on hardware.
4. **v1.0**: wire format frozen, contract schema versioned, bench suite published with a hardware-measured Cortex-M budget.

Security (AEAD, key provisioning) comes after 1.0 unless a user needs it first.
