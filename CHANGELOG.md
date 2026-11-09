# Changelog

## Unreleased

- **Board results:** the timing report starts with the board, OS, rtlog and Rust versions (`--board NAME` or `RTLOG_BOARD`; otherwise the device-tree or DMI model). Users share it in `results/<version>/<board>.md` by pull request, or with the new board-results issue form. Microcontroller results come from the Zephyr benchmark firmware through `scripts/results.py mcu`; the first entry is the STM32 NUCLEO-L432KC measurement (v0.2.2). `scripts/results.py` checks each file and regenerates the comparison table in `results/README.md`, and CI runs it.
- **Quality checks in CI:** line coverage reported to Codecov with an 85 % minimum; `cargo-deny` (advisories, licences, sources), `typos`, `actionlint`, `shellcheck` and `clang-tidy` on the C header; docs build with `-D warnings`, and every public item is documented (`#![warn(missing_docs)]`). `AGENTS.md`, `CLAUDE.md` and `CONTRIBUTING.md` describe the checks and the rules.
- **Crash dumps of debug builds were lost on many x86-64 CPUs.** On a fatal signal the handler ran on the alternate signal stack Rust gives each thread, `max(SIGSTKSZ, AT_MINSIGSTKSZ)`, which is 8 KiB on CPUs without AMX. A debug build's dump needs more once the CPU's saved state is on it, so it overflowed: the file held only the stream header and the process died of `SIGSEGV` instead of the original signal. Release builds fit, with little margin. The handler now runs on its own 64 KiB stack with a guard page, set up by `crash::install` for the calling thread and for every thread when it first logs after that (`crash::SIGNAL_STACK`). A new test forces an 8 KiB default stack. This was the crash-test failure seen now and then in CI.
- Release: crates.io publishing uses Trusted Publishing; the `CARGO_REGISTRY_TOKEN` secret is no longer needed.

## v0.2.3 — on crates.io

The first release published to crates.io: `cargo add rtlog` for the library, `cargo install rtlog-decode` for the CLI.

- The core crate is renamed from `rtlog-core` to `rtlog`, the name it was already imported as (`use rtlog::…`); its folder is now `crates/rtlog`.
- Both crates can be published to crates.io: `rtlog-decode` pins `rtlog` by version, and both have a readme, keywords and categories. The release workflow publishes them after the GitHub release (with the `CARGO_REGISTRY_TOKEN` secret) and checks the packages with `cargo publish --dry-run` on every run.
- README: install with `cargo add rtlog` and `cargo install rtlog-decode`.
- Tests: the `no_drain` tests run one at a time; run in parallel they shared the global ring list and failed now and then on macOS.

## v0.2.2 — review fixes, decoder resync, STM32/Zephyr benchmarks

Fixes from an independent review of v0.2.1, and the decoder resync that was the top open item.

- **Decoder resync.** The drain (Rust `std` and C) writes a resync frame every 64 KiB; after a bad frame the decoder skips to the next one instead of ending, so one corrupted byte on a UART or UDP link costs a stretch of frames, not the capture. The output shows a `GAP` line, a `gap` JSON record and a Perfetto marker; `Decoded::gaps` and `skipped_bytes` count them. Each resync is followed by every context's name and exact clock, so times after a gap are exact. `Decoder::recovering(false)` restores stop-at-first-error. On `no_std`, call `drain::write_resync`.
- **C: a stale slot could be written by two threads.** The pthread destructor now clears the thread's own pointer; before, a later destructor that logged could write into a slot the drain had freed and given to a new thread. A failed `pthread_key_create` no longer clobbers another library's key 0.
- **Crash dump scan** loaded `head` before `tail`, so a running drain could leave `tail` past `head` and the dump walked bogus lengths. Now `tail` first, frames shorter than the smallest real one are rejected, and a fence orders the data reads before the re-check (Rust and C).
- **Crash files are never overwritten**: `O_EXCL`, pid read at dump time (containers that reuse pid 1, forked children). Panics and snapshots stop after 16 files per process so a service that catches a panic per request cannot fill the disk; fatal signals are always dumped.
- **Two threads faulting at once** no longer cut the first dump short: `SA_RESETHAND` made the kernel kill the second thread on the spot; the handler now restores the default action itself, and the second thread waits for the first dump (Rust and C).
- A panic or snapshot dump no longer holds the ring-list lock while writing to disk, so threads starting or exiting are not stalled by a slow disk.
- **Rings kept without a drain**: the most recently *finished* threads are kept (it was the oldest registered), and the bound applies again if a drain is dropped.
- Stdout mode: a drop report arriving first no longer made every later timestamp print as absolute ticks.
- C++: `{x}` in a format string is a `static_assert`, as in Rust. `scripts/spec_check.py` compares the resync frame ID across Rust, C and the decoder.
- Add a shared STM32 hardware stress comparison for rtlog and Zephyr deferred
  logging: overload, paused consumers, dynamic payloads, timer interference,
  and drop accounting/recovery checks. Record three runs per logger in the
  [stress report](docs/stm32-stress.md).
- Build the STM32L432KC benchmark with Zephyr 4.2 / SDK 0.17.2, add a deferred
  Zephyr logger comparator and matched ten-site footprint builds, and record
  measured static flash/RAM sizes and three hardware cycle runs per logger in the
  [STM32 comparison report](docs/stm32-comparison.md).
- Add a Zephyr Cortex-M cycle benchmark with serial CSV reporting, explicit
  successful-write and full-ring checks, and NUCLEO-L432KC build/flash instructions
  in [the Zephyr guide](docs/zephyr.md).
- Docs: the first Cortex-M hardware measurement is now referenced from the spec, the performance guide and the roadmap. On an STM32L432KC at 80 MHz the C/Zephyr path takes 447 median cycles for one `u32` and 520 for three, about three times the spec's ≤ 150-cycle placeholder, which is unchanged: optimising that path is the next step on the roadmap.

## v0.2.1 — hardening after the v0.2.0 review

Fixes from the v0.2.0 review ([PR #9](https://github.com/NieWil/rtlog/pull/9)). **API change:** `rtlog::crash::install` and `rtlog_crash_install` take a directory, not a file, and write `crash-<pid>-<n>.rtlog` in it.

- **Crash log no longer depends on the drain.** The dump reads the rings through their own short-lived lock, so a crash while the drain is blocked in a transport write still captures every record; a test crashes a child with a stuck drain. Each dump is a new file, `<dir>/crash-<pid>-<n>.rtlog`: `install` now takes a directory, caught panics and snapshots never overwrite an earlier dump, and the panic hook never sleeps. Same for C (`rtlog_crash_install(dir)`).
- **No ring leak without a drain.** A program that never installs a drain keeps the rings of at most the last 16 exited threads (`KEEP_WITHOUT_DRAIN`), not one per thread it ever ran.
- **C: thread slots are freed when threads exit** (pthread TLS destructor), so a thread pool never runs out of `RTLOG_MAX_THREADS`; calls that find no slot are reported as drops on context 0. The C crash dump keeps the drain out while it reads.
- **Spans are independent of the level**: `set_spans(false)` / `rtlog_set_spans(0)` removes them, `set_level(Warn)` no longer does. Span IDs carry the full 32-bit context (was 16), so they stay unique past 65 536 threads.
- `info!("x={x}")` (inline capture) is now a clear compile error; write `"x={}", x`.
- The cycle budget is reported, not gated, in CI (shared runners vary by 10–20 cycles); sizes on Cortex-M4F stay gated. `--strict-timing` gates it on a dedicated machine.
- Spec: sections describing planned work moved to [docs/roadmap.md](https://github.com/NieWil/rtlog/blob/main/docs/roadmap.md); `docs/spec.md` now describes only what is implemented.

## v0.2.0 — faster log call, crash log, stdout mode, device demo

- Faster log call: frames are encoded in place in the ring (82 instructions per W1 call on x86-64, down from 198): 36 cycles on x86-64 and 46 on arm64 in CI, within the spec’s 50-cycle budget; on bare metal one shared push keeps per-call-site code at about 56 bytes.
- Crash log: `rtlog::crash::install` / `rtlog_crash_install` dump every undrained record plus a `CRASH` record on panic, `SIGSEGV`, `SIGFPE`, `SIGBUS`, `SIGILL` and `SIGABRT`; `snapshot()` dumps on demand. New `crash` and `ctx_time` frames.
- Stdout mode: `rtlog_decode::stdout::init()` prints decoded text as the program runs, level from `RTLOG`.
- CI runs once per commit: pull requests on `pull_request` only, `main` on `push`; a new push cancels the run it replaces. The release dry run on pull requests only runs when a manifest or the release workflow changes.
- `scripts/spec_check.py` and the `rtlog-spec-check` skill enforce the spec's real-time, wire, usability, cycle and size rules in CI.
- Demo package per architecture on the releases page (`rtlog-demo-<version>-<target>`): timing report, Perfetto quickstart, traced app, stdout mode and the CLI, with `run.sh` and a guide to run it on a device and read the results on the host ([docs/demo.md](https://github.com/NieWil/rtlog/blob/main/docs/demo.md)).
- Specification v0.2.

## v0.1.0 — first preview

Deferred, lock-free binary logging and tracing for real-time code, from Cortex-M to Linux, decoded on the host to text, JSON lines or a Perfetto trace.

### Logging side

- `rtlog-core` (Rust, `no_std` with an `std` feature):
  - `trace!` … `error!` and `span!`;
  - a typed argument encoding;
  - one lock-free SPSC ring per thread or task;
  - a drain with pluggable transports;
  - a CPU-counter clock: TSC or `CNTVCT_EL0`.
- `c/include/rtlog.h`: a header-only C / C++ producer that writes the same format.
- A Zephyr module (`zephyr/module.yml`, `c/zephyr`), not yet built in CI.

### Host side

- `rtlog-decode` and the `rtlog` CLI:
  - `decode` to text, JSON lines or Perfetto (`--perfetto`);
  - `contract --elf` reads format strings from any ELF image, firmware included;
  - `check` validates a contract.

### Measured

- 53 cycles (14.9 ns) per log call on x86-64 and 63 cycles (18.6 ns) on arm64, about 5× less than formatting the line.
- On Cortex-M4F, 10 messages take 3.1 KB of flash against 16.8 KB with `core::fmt`.

### Verified in CI

- Tests on Linux, macOS and Windows, and on arm64, ARMv7, RISC-V 64 and x86 under QEMU.
- A 2-minute soak test, Miri, ThreadSanitizer and AddressSanitizer.
- C/C++ end-to-end with GCC and Clang.
- `no_std` builds for Cortex-M3/M4F/M33 and RV32IMAC.

### Known limits

- Format strings still ship in binaries; stripping them is planned.
- The ≤ 50-cycle budget per call is not met yet.
- See [implementation notes](https://github.com/NieWil/rtlog/blob/main/docs/implementation.md).

Licensed under MIT OR Apache-2.0.
