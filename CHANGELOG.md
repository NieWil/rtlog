# Changelog

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
