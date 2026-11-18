# Changelog

## Unreleased

## v0.3.0 — macOS and Windows, stripped builds, syslog, journald and OpenTelemetry exporters

The minor version changes because two public types of `rtlog-decode` grew: `Output` has three new variants and `Options` and `stdout::Config` a new field `fields`. Code that matches on `Output` without a wildcard arm, or builds `Options` or `Config` listing every field, needs a small change. The wire format is unchanged: captures from 0.2 decode with 0.3, and the decoder accepts a contract hash of 0 in the stream header as "not stated" (stripped builds write it).

- Release workflow: both macOS binaries (Apple silicon and Intel) are built by one job on an Apple-silicon runner, which cross-compiles the Intel one. A release no longer waits for two macOS runners at the same time; the two separate macOS jobs had queued for 15 minutes without a runner. The Intel binary was already built but not run in the release, as before.
- CI: the C++ build for the MSVC ABI with Clang on Windows defines `_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH`. The Windows runner image now has Visual Studio's C++ library 14.52, which requires Clang 22, next to Clang 20, and stopped compiling with error STL1000.
- **Choose what a text line shows:** a list of fields from `time`, `ctx`, `level`, `module`, `file`, `line` (plus `default`, `all`, `none`) selects what comes before the message, in a fixed order: `rtlog decode --fields default,line`, `RTLOG_FIELDS=default,line` for the stdout mode, or `Fields` in `rtlog_decode::Options` and `stdout::Config`. The default line is unchanged (`time`, `ctx`, `level`), and the log call writes the same bytes whatever is shown. `Options` and `stdout::Config` have a new field `fields`: a struct literal that lists every field needs `..Default::default()` or the new field. [Reference](docs/reference.md#choose-what-a-text-line-shows).
- **journald exporter:** `rtlog decode --out journald` writes a capture as systemd journal entries in the Journal Export Format (what `systemd-journal-remote` imports), and `--journald-socket <path>` sends them to a running `systemd-journald` over its native socket (Unix only). Each record, finished span, drop report, stream gap and crash is one entry with `MESSAGE`, `PRIORITY`, `SYSLOG_IDENTIFIER`, `CODE_FILE`, `CODE_LINE` and `RTLOG_*` fields (level, site ID, format string, context, each argument, target time), all usable in `journalctl` matches. `--start`, `--epoch`, `--host` and `--app` are shared with the syslog and OpenTelemetry outputs. Library: `Output::Journald`, `rtlog_decode::journald` (`Config`, `decode`, `datagrams`, `send`, `SOCKET`). Guide: [docs/journald.md](docs/journald.md).
- README: links and images that point into the repository are absolute URLs. On crates.io, where the README is the page of `rtlog` and `rtlog-decode`, every relative link resolved under `crates/<name>/` and was dead (34 links and 2 images). `scripts/spec_check.py` now fails on a relative link in the README and checks that each absolute one points to an existing file and heading. The crates.io pages change with the next release.
- **OpenTelemetry exporter:** `rtlog decode --out otlp-json` writes a capture as OpenTelemetry log records in the OTLP JSON encoding, one `LogsData` object per line with up to 1000 records: the file format of the OpenTelemetry Collector's `otlpjsonfile` receiver. Severity number and text come from the level, the body is the formatted message, and the call site (`code.file.path`, `code.line.number`), context (`thread.name`, `thread.id`), site ID, format string and each typed argument are attributes. Dropped frames, stream gaps and crashes are records with an `eventName`. Spans are not exported yet. `--start`, `--epoch`, `--host` and `--app` are shared with the syslog output. Library: `Output::OtlpJson`, `rtlog_decode::otlp` (`Config`, `decode`, `severity`). Guide: [docs/opentelemetry.md](docs/opentelemetry.md).
- **Syslog exporter:** `rtlog decode --out syslog` writes a capture as RFC 5424 lines, one per record, finished span, drop report, stream gap and crash. Severity comes from the level, the process ID field is the context name, the message ID is the site ID, and level, file, line, module and the target time are structured data. `--start` (wall-clock time of the capture's first event) or `--epoch` (of the target's time zero) gives the lines a timestamp; `--host`, `--app`, `--syslog-facility` and `--syslog-sd-id` set the other fields. Library: `Output::Syslog`, `rtlog_decode::syslog` (`Config`, `decode`, `parse_epoch`, `facility`, `severity`). Guide: [docs/syslog.md](docs/syslog.md). `Output` has a new variant: a `match` on it without a wildcard arm needs one more arm.
- Roadmap: exporters planned step by step (section 10: a common decoded record, syslog RFC 5424, journald, OpenTelemetry Logs as OTLP JSON and over HTTP, spans as OpenTelemetry traces), and Linux packaging (12g: package names per distribution rules, RPM packages, apt and dnf repositories, AUR, a Yocto layer; submission to the distributions after 1.0). The README's "How it works" states the design as four stages and lists the exporters, built and planned.
- CI: the `bench` workflow runs only when a push or pull request changes the measured sources, the measuring scripts or the build settings (`crates/`, `examples/`, `size/`, `reference/pingora/`, `scripts/size_report.py`, `scripts/icount.py`, `Cargo.toml`, `Cargo.lock`, `.cargo/`, `rust-toolchain*`, the workflow file). Other changes add no point to Bencher and use no runner time for it.
- Pingora reference application reworked: two more baselines (`env_logger` through a `BufWriter`, and `tracing` with `tracing-appender`'s non-blocking writer), the instructions of the access-log call counted with callgrind in a single-threaded mode (same count on every run), the distribution of the call's time, and peak memory. The text loggers execute 9 351 to 9 965 instructions per access-log call whether the write is buffered, asynchronous or direct; rtlog executes 313. 99 % of the rtlog calls take under 0.7 µs inside the running proxy, against 74 to 164 µs. Peak RSS is 13 to 15 MiB with no log, `log` or rtlog and about 17 MiB with `tracing`. A control run (`PINGORA_IDLE_DRAIN=1`) shows that the throughput gain rtlog appears to give comes from its drain thread's wake-ups on a shared VM, not from logging. The instruction counts go to Bencher and gate pull requests at 5 % (`bench` workflow, job `Bencher (pingora)`; the count differs by up to 2 % between CPU models), which replaces the `reference application (Pingora)` job of `ci.yml`.
- CI: instructions per log call are counted with Valgrind's callgrind (`scripts/icount.py`, `examples/icount.rs`: six workloads) and sent to Bencher; a pull request that grows one by more than 2 % over `main` fails. Unlike timings, the count is the same on every run of the same binary. On x86-64 with rustc 1.97.0: 73.5 instructions for a two-argument call through an explicit producer, 77.5 into the thread-local ring, 6 for a call below the runtime level.
- CI: sizes on Cortex-M4F and Criterion timings are tracked over time with Bencher (`.github/workflows/bench.yml`; project from the repository variable `BENCHER_PROJECT`). A pull request that grows a size by more than 2 % over `main` fails; timings from the shared runner are reported only. `scripts/size_report.py --bmf` writes the sizes as Bencher Metric Format JSON.
- Docs: rtlog's range is stated as "from microcontrollers to multi-core servers" instead of "from Cortex-M to Linux", and the README has a "Where it runs" table (targets, languages, what CI checks on each).
- **Reference application: Pingora** (`reference/pingora`, roadmap 12a). Cloudflare's proxy at a pinned revision, with its per-request access log written through `log` + `env_logger` or through rtlog, an upstream and a load generator in one binary. On a 4-vCPU Xeon at 2.30 GHz the access-log call takes about 9.7 µs with `log` and about 270 ns with rtlog; throughput drops 7 to 11 % with `log` and by nothing measurable with rtlog; the log is 54.9 bytes per request against 139.7. The README there has the method, three rounds of numbers and a survey of Pingora's 552 call sites (40 % port with a macro rename). CI builds it and runs a short round.
- Roadmap: a new section 12 (adoption) with incremental steps for a ported real application as the cost reference, logger comparisons, bridges, live decoding, installation and first impression; the release plan says which step lands in which version.
- **Stripped builds: format strings and descriptors out of the shipped image** (ELF: firmware and Linux). Rust: the new `strip` feature puts each site's strings into an `rtlog_strs` section; C and C++: `-DRTLOG_STRIP`; Zephyr: `CONFIG_RTLOG_STRIP=y`. A linker script marks `rtlog_strs` and the descriptor section as not loaded, so they stay in the ELF file for `rtlog contract --elf` and are absent from the flashed image. Measured: Cortex-M4F firmware with 10 call sites 4536 → 3796 B of flash (`size/mcu`); Zephyr `hello` on `qemu_cortex_m3` 10108 → 9896 B of `zephyr.bin`. On Linux, `c/rtlog_strip.ld` (GNU ld) plus `objcopy -R` does the same for a program. With `strip`, Rust writes contract hash 0 in the stream header (the decoder now reads 0 as "not stated"), `site::sites()` is empty and `contract::write` returns an error; C contracts have no function names. Guide: [docs/strip.md](docs/strip.md). This closes threat V2 of the specification.
- **`rtlog verify --contract <contract> <image>...`** fails if an image still holds a format string of the contract: a release gate for stripped builds. Library: `Contract::verify_stripped`, `Contract::sites`.
- Decoder: a string pointer of a descriptor is resolved in `rtlog_strs` first, then in loaded sections only. A null or small bogus pointer no longer resolves into a non-loaded section at address 0 (`.shstrtab`, debug data).
- Size report: a `strip` variant of the Cortex-M4F firmware, which must have no descriptor in flash. `size/mcu/memory.x` moved to `size/mcu/layout/` (`build.rs` assembles the script).
- CI: `scripts/strip_e2e.sh` (Rust and C stripped builds on Linux: contract, `objcopy`, `rtlog verify`, run, decode) and a second Zephyr QEMU run with `CONFIG_RTLOG_STRIP=y`.
- **C and C++ on macOS and Windows.** `rtlog.h` now builds for Mach-O (Apple Clang) and PE (MinGW-w64 GCC, or Clang for the MinGW or the MSVC ABI; not `cl.exe`). Descriptors go into `__DATA,rtlog_sites` on macOS and into an `rtlog` section on Windows. The default clock is `CLOCK_MONOTONIC` on macOS and the performance counter on Windows; a thread's ring slot is released at thread exit on both (pthread destructor, fiber-local-storage callback). CI runs `scripts/c_e2e.sh` on `macos-latest` (Apple Clang) and `windows-latest` (MinGW-w64 GCC), and the C++ demo with Clang for the MSVC ABI.
- **`rtlog contract` reads Mach-O and PE binaries**, not only ELF, for Rust and C sites: `rtlog contract --elf <binary>` (also spelled `--binary`) and `rtlog_decode::elf::contract_from_binary`. Mach-O rebases and chained fixups are applied; of a universal binary the first architecture is read. On Mach-O and PE, Rust descriptors are found by checking each entry's stored ID against the hash of its fields (`rtlog::site::span_id` is new for that), because the section name is a hash on Mach-O and the linker merges all `linkme` sections on PE. `contract_from_elf` still accepts ELF only.
- **Crash log on Windows**, in Rust (`rtlog::crash`) and C (`rtlog_crash_install`): panics, unhandled exceptions (access violation, integer division by zero, illegal instruction) through `SetUnhandledExceptionFilter`, the C runtime's `abort()`, and snapshots. The crash record carries the exception code, the faulting address and the program counter; the decoder prints `CRASH EXCEPTION_ACCESS_VIOLATION: invalid memory access (exception 0xc0000005) addr=0x0 pc=0x...`. Not dumped on Windows: `std::process::abort` and stack overflow (fail-fast, no handler runs).
- **Zephyr: the `hello` sample and the guide set `CONFIG_BOOT_BANNER=n`.** The banner selects `PRINTK`, so `CONFIG_PRINTK=n` alone had no effect and the banner text preceded the stream header on the UART (`not an rtlog stream`). Found by the new CI job that builds the sample for `qemu_cortex_m3`, runs it in QEMU and decodes the UART capture (`scripts/zephyr_qemu.sh`).
- C: `crash.c` no longer needs `pthread_barrier_t` (absent on macOS); `scripts/c_e2e.sh` runs on macOS and in Git Bash.

## v0.2.5 — Debian/Ubuntu packages, C thread-name fix

Each release now ships `.deb` packages for the CLI and the C header, and `rtlog_set_thread_name` no longer loses the name on a thread's first use.

- **C: `rtlog_set_thread_name` on a thread's first use could lose the name.** The call first claimed the ring under the default `thread-<ctx>` name and made it live, then renamed it; a drain running in between announced the default name, and records logged before the next announcement were labelled `[thread-<ctx>]`. The first call now claims the ring under the given name. In a stress test (200 threads, a drain spinning in parallel), 143 of 4000 records were mislabelled before and 0 after. This was the `C / C++ (gcc)` CI failure on `main` (`missing in demo: [sensor] ...`).
- **Debian/Ubuntu packages on each release:** `rtlog_<version>-1_<arch>.deb` (amd64, i386, arm64, armhf) installs the `rtlog` command and a man page; `rtlog-dev_<version>-1_all.deb` installs `rtlog.h` and a pkg-config file (`pkg-config --cflags --libs rtlog`). Built by `scripts/build_deb.sh` in the release workflow, which installs, runs and lints them (lintian) before publishing; they are covered by `SHA256SUMS`.

## v0.2.4 — crash dumps on every thread, quality gates, board results

Crash dumps of debug builds are complete whichever thread crashes (x86-64 and aarch64); CI enforces coverage, dependency, spelling, workflow and C static checks; users can share timing results from their boards.

- **macOS: a per-thread signal stack could be unmapped while still installed.** `SS_DISABLE` with `ss_size` 0 fails there (`ENOMEM` below `MINSIGSTKSZ`), so a thread's 64 KiB signal stack stayed registered after rtlog unmapped it at thread exit. It is now disabled with a valid size, and kept rather than unmapped if disabling still fails. Found by the new tests on the macOS runner.
- **`rtlog contract --elf` dropped sites silently** when a site section did not hold a whole number of descriptors (a binary with another descriptor layout, or a damaged section): the partial last descriptor was skipped and decoding later failed with "unknown site". It is now an error naming the section and both sizes. Found by the new coverage tests.
- **Test coverage 85.9 % → 98.7 % of lines** (CI minimum raised from 85 % to 97 %): tests for the stdout mode, every CLI option and error, malformed ELF and contract input, Perfetto and stream edge cases; crash-test children killed by a signal now record their coverage (`cfg(coverage)` only), and the core crate's clock, contract, drain, ring, runtime, site and crash paths are tested. No code is excluded from measurement; what stays uncovered is platform code the Linux x86-64 coverage job cannot run and syscall failures that need fault injection.
- `cargo doc --workspace` no longer races: the `rtlog` CLI binary's docs (`doc = false`) went to the same `target/doc/rtlog/` directory as the `rtlog` library's (cargo issue 6313), which failed the docs check now and then.
- **Board results:** the timing report starts with the board, OS, rtlog and Rust versions (`--board NAME` or `RTLOG_BOARD`; otherwise the device-tree or DMI model). Users share it in `results/<version>/<board>.md` by pull request, or with the new board-results issue form. Microcontroller results come from the Zephyr benchmark firmware through `scripts/results.py mcu`; the first entry is the STM32 NUCLEO-L432KC measurement (v0.2.2). `scripts/results.py` checks each file and regenerates the comparison table in `results/README.md`, and CI runs it.
- **Quality checks in CI:** line coverage reported to Codecov with an 85 % minimum; `cargo-deny` (advisories, licences, sources), `typos`, `actionlint`, `shellcheck` and `clang-tidy` on the C header; docs build with `-D warnings`, and every public item is documented (`#![warn(missing_docs)]`). `AGENTS.md`, `CLAUDE.md` and `CONTRIBUTING.md` describe the checks and the rules.
- **Crash dumps of debug builds were lost on many x86-64 CPUs.** On a fatal signal the handler ran on the alternate signal stack Rust gives each thread, `max(SIGSTKSZ, AT_MINSIGSTKSZ)`, which is 8 KiB on CPUs without AMX. A debug build's dump needs more once the CPU's saved state is on it, so it overflowed: the file held only the stream header and the process died of `SIGSEGV` instead of the original signal. Release builds fit, with little margin. The fatal dump now runs on its own 64 KiB stack with a guard page (`crash::SIGNAL_STACK`), mapped by `crash::install`: the handler claims the dump, then switches to that stack (x86-64 and aarch64), so it no longer depends on which thread crashed, including a thread that never logged. Elsewhere the per-thread signal stack is enlarged to 64 KiB by `install` and each thread's first log call. New tests force an 8 KiB default stack, on the installing thread and on a thread that never logged. This was the crash-test failure seen now and then in CI.
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
