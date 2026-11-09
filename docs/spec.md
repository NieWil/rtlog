# Real-Time Deferred Logger — Specification (v0.2.x)

Nov 2, 2026 · @Niels

This document describes what rtlog **does**, as released in v0.2.x: the API, how site IDs and contracts are produced, the frame layout, the runtime, the crash log and the stdout mode, and the budgets that CI checks (`scripts/spec_check.py`). Everything that is planned but not built, including the material earlier versions of this spec presented as design, is in the [roadmap](roadmap.md); the [implementation notes](implementation.md) list where the code deviates from the first design.

## 0. Developer experience and API

A developer writes `rtlog::info!("pid out={} err={}", out, err)` and reads the line on a terminal; everything between is automatic, and the logger has no measurable effect on the system it observes, on an MCU or on Linux.

**Zero-impact principle** (what "without affecting the system" means)

- No syscalls, no heap, no locks, no formatting on the log call: one counter read, one store per argument, one release store.
- No background activity unless asked: on `std` the drain thread is opt-in (`spawn_drain`); the alternative is polling a `Drain` from the application's own idle point.
- Bounded memory: rings are sized at build time and never grow; memory per context is known before deployment.
- Nothing on the log path touches stdout, stderr or signals. The stdout mode and the crash handler are opt-in and live in the drain and in signal handlers, never in a log call. The log call is safe inside hard real-time threads, SCHED\_FIFO loops and ISRs.
- Disabled sites compile to nothing: `RTLOG_LEVEL=off` at build time removes every call path.
- Dropping is the only overload behaviour: a slow transport can lose frames but can never slow the producer.

**Public API** (crate `rtlog`, prefix `rtlog`)

```rust
// Call sites: same shape as println!, every arg implements Log
rtlog::trace!("...", a, b);  rtlog::debug!  rtlog::info!  rtlog::warn!  rtlog::error!
rtlog::info!(to: &mut producer, "...", a);    // explicit producer (no_std, static rings)
let _s = rtlog::span!("db.query");            // span: a slice in Perfetto, closes on drop
let s = rtlog::span!(to: &mut producer, "pid.update"); producer.span_exit(s);

// Setup
static MOTOR: Ring<1024> = Ring::new();       // no_std: one static ring per task
let (producer, consumer) = MOTOR.split(ctx);  // once; ctx 0 = ISR by convention
rtlog::set_clock(dwt_cycles, 168_000_000);    // no_std clock; std uses TSC / CNTVCT
rtlog::set_level(Level::Info);                // runtime level, one atomic store
rtlog::set_spans(false);                      // spans on or off, independent of the level
rtlog::set_context_name("control");           // std: name the calling thread's track

// Draining: pick one
rtlog::spawn_drain(transport, period);        // std: dedicated thread, opt-in
rtlog::Drain::new(transport).poll();          // std: from your own loop
rtlog::drain::drain_ring(&mut consumer, &mut transport, &mut scratch); // no_std idle loop
rtlog_decode::stdout::init();                 // std: text on stdout, level from RTLOG (section 8b)

// Crash log (section 8b)
rtlog::crash::install("crashes/");            // panic, SIGSEGV, SIGFPE, SIGBUS, SIGILL, SIGABRT
rtlog::crash::snapshot("deadlock suspected"); // same dump, without crashing

rtlog::contract::write(path)                  // std: this binary's contract as JSON lines
rtlog::stats() -> Stats                       // frames, bytes, drops, live contexts
```

C and C++ use the same model through `c/include/rtlog.h`: `RTLOG_INFO("x={}", x)`, `RTLOG_SPAN_BEGIN` / `RTLOG_SPAN` (RAII in C++), `rtlog_drain(write, user)`, `rtlog_set_spans`, `rtlog_crash_install(dir)`.

**Usage by target**

| Target | Setup lines | Reading logs |
| --- | --- | --- |
| Cortex-M, RTOS, bare metal | `static RING: Ring<N>`; `split` per task; `drain_ring` in the idle loop to RTT, UART or flash; `set_clock` | `rtlog contract --elf firmware.elf`, then `rtlog decode` the capture |
| Embedded Linux daemon, service | `spawn_drain(IoTransport(file))`; `crash::install(dir)`; `contract::write` or `rtlog contract --elf` | `rtlog decode app.rtlog`, `--json` for pipelines, `--perfetto` for a timeline |
| Desktop app, `cargo run` | `rtlog_decode::stdout::init()` | text on the terminal, `RTLOG=debug` |
| Library crate | only call sites; no init, no transport | inherits the host application's setup; silent if none (the last 16 exited threads' rings are kept without a drain, then released) |

**Build and retrieval flow**

1. `cargo build`: each macro call emits a `static` descriptor with its ID, level, location, format string and argument count into a link section (`linkme`).
2. The contract is produced from the binary (`rtlog contract --elf <image>`, works for firmware and C) or by the running program (`rtlog::contract::write`). The stream header carries the contract hash.
3. A capture decodes against the contract whose hash matches; CI should archive the contract next to each shipped image. Pairing is by hand (path on the command line); `--force` decodes with a mismatched contract and marks every line.

**What it buys over printf**

- Flash: formatting code is gone (5.6× less flash for 10 messages on Cortex-M4F, section 10c). Format strings stay in the image until stripping lands (roadmap, section 4).
- Speed: tens of nanoseconds per call; never blocks an ISR or a control loop.
- Archive: a `.rtlog` capture is 2–10× smaller than text and keeps typed fields.
- One format for Rust, C, C++ and Zephyr, one decoder, one viewer.

**Why an existing solution is not enough**

| Solution | Inconvenience |
| --- | --- |
| printf / semihosting / RTT text | Strings in flash, slow and blocking, no structure, no per-task context |
| `log` + `env_logger`, `tracing-subscriber` | Formats on device, global mutex, strings in binary, `std`-centric |
| defmt | Best MCU model, but ELF-only (no macOS, Windows or dynamic binaries), one global buffer under a critical section, tied to probe-rs, no `std` story |
| NanoLog | C++17 and preprocessor, Linux x86 only, \~1 MB RAM per thread, a dedicated emitter core |
| delog | Formats on device, single global ring |
| SEGGER SystemView, Tracealyzer | Closed formats, licences, vendor viewers, MCU-only |
| LTTng, ftrace | Linux only, daemon required, no MCU |

None covers MCU and desktop builds with one call site, one contract and one viewer while leaving the observed system untouched.

## 1. Goals and non-goals

The logger makes every log call bounded, lock-free and allocation-free, and renders human-readable text only on the host. One core crate serves bare-metal MCUs, embedded Linux and desktop.

**Goals**

- Log call cost: tens of nanoseconds, constant per site, no formatting, no heap, no locks.
- Portable: `no_std` core, `std` feature gate, same wire format on every target.
- Per-task context: each task, thread or ISR owns its own ring buffer.
- Human-readable output on the host, plus structured output (JSON lines) and a timeline (Perfetto) for tooling.
- Lossy by design: overflow drops frames and counts them; the call never blocks.

**Non-goals**

- Log querying, indexing or analytics engine.
- Log rotation, retention or encryption (left to the transport sink).
- Replacing a full tracer; spans and Perfetto cover the common case, the rest goes through bridges (roadmap, section 10).

## 2. Study of existing solutions

Deferred formatting is proven on both ends of the spectrum: defmt on MCUs and NanoLog on servers. None of them is portable across both, which is the gap this project fills.

| Solution | Target | Format strings | Hot path | Per-context buffers | Transport | Gap for us |
| --- | --- | --- | --- | --- | --- | --- |
| [defmt](https://defmt.ferrous-systems.com/) (Rust) | MCU, `no_std` | Interned at compile time, IDs from ELF linker section | Binary args, fixed width | One global RTT buffer (critical section) | RTT, probe-rs | ELF-only, single buffer, no std story |
| [Postform](https://lib.rs/crates/postform_persist) (C++) | 32-bit MCU | Same idea as defmt | Binary args | Single | RTT, TTY | C++, MCU-only |
| [NanoLog](https://github.com/platformlab/nanolog) (C++) | Linux x86 | Extracted by preprocessor/constexpr | Binary args, leading-zero compression | Per-thread staging buffers, lock-free, \~1 MB each | Background thread to file | Linux-only, heavy RAM, one core for the emitter |
| [delog](https://docs.rs/delog) (Rust) | MCU | Kept in binary | Formats on device into a static ring | Single global ring | Flush in idle loop | Formatting still on device |
| LTTng / ftrace | Linux kernel + user | Tracepoint metadata in CTF | Binary, per-CPU lock-free ring | Per-CPU | Shared memory to consumer daemon | Linux-only, no MCU |
| SEGGER SystemView | MCU (RTOS) | Event IDs, description file | Binary events | Single RTT buffer | RTT | Closed format, vendor tool |

Takeaways carried into the design:

- Interned format strings + binary args (defmt, NanoLog) are the right hot path.
- Per-context single-producer rings (NanoLog, ftrace) remove contention without CAS.
- The contract must be a standalone artifact, not only an ELF section, to work on macOS, Windows and dynamic binaries.
- Compression belongs to the drain, not the log call.

## 3. Architecture overview

Three stages: at build time each log site gets a `static` descriptor and a content-hashed ID; at runtime each task writes raw binary frames into its own ring; the host joins stream and contract to render text, JSON or a trace.

The contract never travels through the target: the device sends only IDs, timestamps and raw argument bytes, and the host supplies the strings. See [Architecture](architecture.md) for the diagrams.

## 4. Contract

A site ID is a 32-bit content hash, so the scheme works on every target and object format.

**Site ID**

- Rust: `id = fnv1a32(module | file | line | column | format | level | nargs)`, computed at compile time by the macro; IDs that fall in the control range (`0xFFFF_FF00..`) have their top bit flipped. Argument types travel on the wire as tags (section 5) instead of in the hash, because a declarative macro cannot see them.
- C and C++: the descriptor's index in the `rtlog_sites` section plus one. C has no compile-time string hashing; adding a site renumbers the ones after it, which the contract hash catches.
- Birthday bound at 32 bits: the chance that two of n sites collide is about n²/2³³, so about 1 in 8 600 at 1 000 sites and 1 in 950 at 3 000 per build; a crate's own sites are checked for collisions in its tests (a whole-program check is on the roadmap).
- Span sites use the same scheme with a distinct mix, and `kind: "span"`.

**Contract entry** (one JSON line per site, as `rtlog contract --elf` and `rtlog::contract::write` produce it)

```json
{"id":438670313,"level":"warn","module":"basic","file":"examples/basic.rs","line":38,"column":25,"fmt":"motor {} near limit temp={}","nargs":2}
```

**Generation**

1. Each macro call emits one `static` descriptor into a link section collected by `linkme` (`linkme_SITES` on ELF, the equivalent Mach-O and PE sections). `rtlog contract --elf` reads only the ELF section; on macOS and Windows the program writes its own contract with `contract::write`. C and C++ emit `struct rtlog_site` into `rtlog_sites`. No files are written at expansion time, so parallel crate builds cannot race.
2. `rtlog contract --elf <image>` reads the descriptors with the `object` crate, applies `RELATIVE` relocations for position-independent executables, validates them and writes the JSON-lines contract. On `std`, `rtlog::contract::write` writes the same file from the running program.
3. The descriptor section ships in the image: binaries are self-describing, and format strings are present in them. Stripping is on the roadmap.

**Binding stream to contract**

- The stream header carries `contract_hash` (hash of every site ID) and `scheme` (0 = hash32; 1 is reserved).
- The decoder refuses a mismatch unless run with `--force`, then marks every line decoded under a mismatched contract.
- Two independent version numbers: `wire_version` in the stream header changes only for frame-grammar changes; the contract's field set changes for descriptor-format changes. New control frames take new IDs; old ones stay decodable.

## 5. Wire format

Arguments are written fixed-width so every site has a compile-time maximum frame size; only the small metadata fields are varint. Everything is little-endian.

**Stream header** (17 bytes, once per stream and at the start of every crash dump)

| Field | Width | Content |
| --- | --- | --- |
| magic | 4 B | `RTL1` |
| wire\_version | u8 | frame grammar version (1) |
| scheme | u8 | 0 = hash32 IDs (1 reserved for linker-assigned IDs) |
| contract\_hash | u32 | hash of the contract's site IDs |
| clock\_hz | u32 | timestamp tick rate |
| ptr\_width | u8 | 4 or 8, drives `usize` width |
| flags | u8 | 0; bits reserved for drain compression and authenticated or encrypted blocks |
| key\_id | u8 | 0; reserved |

**Log frame**

| Field | Width | Content |
| --- | --- | --- |
| site\_id | u32 | content hash (Rust) or descriptor index + 1 (C) |
| ts\_delta | varint ≤ 10 B | ticks since the previous frame in this ring |
| ctx | varint ≤ 5 B | context ID (0 = ISR by convention) |
| args | per argument: 1 B type tag + value | raw little-endian values in call order; strings and byte slices as length + bytes |

- Frame length is not transmitted; the decoder reads the contract's argument count and walks the tagged values.
- Inside a ring every frame is prefixed by a `u16` length so the drain hands whole frames to the transport; the prefix never goes on the wire.
- On hosts the producer reserves a worst-case slot (header + 240 B of arguments) and, when it fits before the wrap point, encodes the frame in place and publishes it with one release store; otherwise it encodes into a stack buffer and copies across the wrap. On bare metal it always encodes into a stack buffer and calls one shared push, to keep per-call-site code small (section 10c).

**Control frames** (site\_id reserved range `0xFFFF_FF00..`)

| ID | Name | Payload |
| --- | --- | --- |
| `..00` | resync | a copy of the 17-byte stream header, so the frame starts with the 8-byte marker `00 FF FF FF 'R' 'T' 'L' '1'`; the drain writes one every 64 KiB of frames, followed by a `ctx_register` and a `ctx_time` per live context; section 9 |
| `..01` | dropped | ctx, u32 count of frames dropped since the last report (ctx 0 in C: calls that found no ring slot) |
| `..02` | ctx\_register | ctx ID, u8 length + UTF-8 name |
| `..03` | ctx\_close | ctx ID |
| `..04` | span\_open | timed like a log frame (ts delta, ctx), then u32 span site ID, u64 span ID, u64 parent ID (0 = root) |
| `..05` | span\_close | timed like a log frame, then u64 span ID |
| `..06` | time\_sync | reserved (roadmap) |
| `..07` | crash | timed like a log frame, then u8 kind (1 panic, 2 signal, 3 snapshot), i32 signal, i32 code, u64 fault address, u64 pc, message (u8 length, bit 7 = truncated, + UTF-8); section 8b |
| `..08` | block\_tag | reserved (roadmap) |
| `..09` | ctx\_time | ctx, u64 absolute ticks: sets the context's clock, so a crash dump that holds only the tail of a ring still decodes to absolute times |

- Span IDs: high 32 bits = context ID, low 32 bits = per-context counter, so IDs are unique with no atomics and stay unique however many threads a process creates. The frame's `ctx` says which thread executed it.
- Resync: a corrupted byte loses only the frames up to the next resync frame (section 9); decoding continues after it. The Rust `std` drain and the C `rtlog_drain` write them; on `no_std` call `drain::write_resync` yourself about every 64 KiB (`PassStats.bytes` counts them) and `write_ctx_time` per context if exact times after a gap matter.

## 6. Argument model

Every argument implements the `Log` trait and is written as raw bytes, one tag byte then the value.

```rust
pub trait Log {
    const TAG: Tag;      // wire tag of this type
    const MAX: usize;    // upper bound on wire bytes, tag included
    fn write(&self, w: &mut Writer<'_>);
}
```

Tags: `U8 U16 U32 U64 I8 I16 I32 I64 F32 F64 Bool Char Str Bytes Option Array` (1–16). `Option` is one presence byte then the inner tagged value; `Array` is a u16 count then that many tagged values.

| Tier | Form | Wire | Bound |
| --- | --- | --- | --- |
| 1 Binary | ints, floats, `bool`, `char`, `usize` / `isize` at the target's pointer width, `Option<T>`, `[T; N]`, `&T` | raw fields in order | `MAX` const |
| 3 Dynamic | `&str`, `String`, `[u8]` | u8 length + bytes, cut at 127 bytes on a UTF-8 boundary; bit 7 of the length flags truncation, rendered as `…` | 128 B |

Rules:

- `{}` with a type that does not implement `Log` is a compile error. Interned strings, `#[derive(Log)]` and a `Display` escape hatch are on the roadmap.
- A call holds at most 240 bytes of arguments and at most 32 arguments; a call that does not fit is dropped and counted.
- Forbidden on the log call: `format!`, `ToString`, any heap allocation, any formatting trait.

**Format grammar** (checked at compile time)

| Syntax | Meaning |
| --- | --- |
| `{}` | the next argument, rendered by its tag |
| `{:?}` | same as `{}` |
| `{:x}` `{:X}` `{:hex}` `{:b}` `{:bin}` | integers and bytes in hex or binary |
| `{:.N}` | float precision |
| `{:str}` | same as `{}` on a string |
| `{{` `}}` | literal braces |
| `{name}` (inline capture) | compile error with a message to write `"x={}", x`: the format string must be a plain literal that goes into the contract |

The placeholder count must equal the argument count; other specs render as `{}`.

## 7. Runtime

Each context owns a single-producer ring; a single consumer per ring drains it, so the hot path needs two atomic loads, one store per argument and one release store.

**Rings**

| Backing | Feature | Allocation | Size | Registration |
| --- | --- | --- | --- | --- |
| `Ring<N>` | all | `static`, in `.bss` | power of two ≥ 64 (1 KiB typical on MCU) | `RING.split(ctx)` once, giving a `Producer` and a `Consumer`; the task logs with `to: &mut producer` |
| thread ring | `std` | heap, `THREAD_RING` = 64 KiB | fixed | created on a thread's first log call; context named after the thread (`set_context_name` renames) |

- Sizing rule: `N ≥ burst_rate × avg_frame × max_drain_latency`.
- `std` registry: new rings queue in `PENDING`; the drain moves them to its own `REGISTRY` on each pass and retires a ring once its thread has exited and it is empty. A separate `RINGS` list, held only for a push or a removal, is what the crash dump reads. While no drain is alive, the rings of the last `KEEP_WITHOUT_DRAIN` (16) threads to finish are kept for a dump and older finished ones released (in the order the threads ended), so a library's log calls in a program with no drain cost at most 1 MiB. This applies again if the drain is dropped.
- C: a fixed table of `RTLOG_MAX_THREADS` (16) rings of `RTLOG_RING_SIZE` (16 KiB); a thread claims a slot on its first call and a pthread destructor closes it when the thread exits (`rtlog_thread_exit()` elsewhere); the next drain pass frees it. Calls that find no slot are counted and reported as drops on context 0.

**Producer / consumer contract**

1. Producer loads `tail` (Acquire), computes free space, and on overflow counts a drop and returns.
2. Producer writes the frame at `head`, then stores `head + len` (Release). The frame is invisible until that store, so the drain never sees a partial frame.
3. Consumer loads `head` (Acquire), hands one whole frame to the transport, stores `tail` (Release). A transport that refuses the frame keeps it in the ring for the next pass.
4. Invariant: exactly one producer and one consumer per ring. No CAS, no spin. (A `Log` implementation that itself logs breaks this on its own ring; a guard is on the roadmap.)

**Overflow policy**: `DropNewest`: the new frame is discarded, cheapest; the per-ring drop counter is reported with a `dropped` control frame on the next drain pass.

**Clock**: `set_clock(fn() -> u64, hz)`. Defaults: on `std`, the invariant TSC halved on x86-64 and `CNTVCT_EL0` on aarch64, else `Instant` in nanoseconds (`RTLOG_CLOCK=instant` forces it); on `no_std`, 0 until `set_clock`. The producer stores the delta from the ring's last timestamp; `ctx_time` frames carry absolute values where needed.

**Levels and spans**

- Levels `trace, debug, info, warn, error`; `RTLOG_LEVEL` at build time removes the call path of sites below the floor; `set_level` is a global runtime filter (one relaxed load per call). Per-module levels are on the roadmap.
- Spans have their own switch, `set_spans`, independent of the level: a `warn`-level build keeps its timeline. `set_off` stops both logs and spans; the crash handler uses it to freeze a consistent snapshot.

## 8. Transport and drain

The drain is the only consumer; it round-robins the rings in registration order and pushes whole frames through a `Transport`. It is the only place allowed to block or batch.

```rust
pub trait Transport {
    fn write(&mut self, frame: &[u8]) -> Result<(), Full>;  // one whole frame, all or nothing
    fn flush(&mut self) {}
}
```

| Transport | Target | Notes |
| --- | --- | --- |
| `SliceTransport` | all | a fixed byte slice; `no_std` and tests |
| `Vec<u8>` | `std` | in-memory capture |
| `IoTransport<W: Write>` | `std` | file, socket, pipe; flushed after each pass |
| `rtlog_decode::stdout::TextTransport` | `std` | decodes in the drain thread and prints text (section 8b) |
| Zephyr module | Zephyr | console UART by default; `rtlog_zephyr_write()` can be pointed at RTT, USB CDC or flash |
| your own | any | implement `Transport` (Rust) or an `rtlog_write_fn` (C): RTT, DMA UART, UDP, shared memory |

**Drain scheduling**: `spawn_drain(transport, period)` runs a thread that polls every `period` (`park_timeout`); `Drain::poll` from an idle loop or a timer does the same by hand; on `no_std`, `drain_ring` per consumer.

**Backpressure**: a full transport never propagates to producers; the drain leaves the frame in the ring and retries next pass. If the ring then overflows, the producer drops and counts. Writes are whole frames, so a transport boundary is always a frame boundary.

## 8b. Debug stdout and crash log

In debug builds a developer sees text on stdout with no tooling; in any build a crash freezes the rings and saves them with the fault address, so the last log lines before a crash are recoverable.

**Debug stdout** (`rtlog_decode::stdout::init()`)

- The host decoder runs in the drain thread behind a text transport; frames are decoded there and printed, so `cargo run` reads like `env_logger`. The level comes from `RTLOG` (`trace` … `error`, `off`); colours when the output is a terminal and `NO_COLOR` is unset; stdout or stderr.
- Only the drain formats; call sites keep the binary path, so timings measured with it on are representative.
- The contract comes from the running binary's own descriptors.

**Crash log** (Unix with `std`: `rtlog::crash::install(dir)`; C on POSIX: `rtlog_crash_install(dir)`)

On a panic, `SIGSEGV`, `SIGBUS`, `SIGFPE`, `SIGILL` or `SIGABRT` (so `abort()` and `panic = "abort"`), the handler:

1. For fatal signals, freezes logging (`set_off`), so the snapshot is consistent. A panic does not freeze: the program may survive it.
2. Opens a new file `<dir>/crash-<pid>-<n>.rtlog` with `O_EXCL` (the pid is read when the dump is written; `n` counts up and skips names that exist; the name is built without `snprintf`) and writes a stream header. Nothing is ever overwritten: not by a caught panic or a snapshot, not by a restart even where containers reuse pid 1, not by a forked child. Panics and snapshots stop after 16 files per process (`MAX_SOFT_DUMPS`); fatal signals are always dumped.
3. Tries, without blocking, to keep the drain out for the duration (`try_lock` on the registry, a bounded spin); goes on without it if the drain is mid-pass. Takes the ring list under its own short-lived lock; for a panic or snapshot it copies the handles out and releases the lock before writing, so a slow disk never stalls a thread that is starting or exiting.
4. For every ring, without consuming anything: its name, any drop count, a `ctx_time` frame (newest timestamp minus the deltas of the unread frames), then every unread frame, oldest first. A frame the drain released meanwhile is skipped; after a fatal freeze a frame can therefore appear in both the main stream and the dump, never in neither.
5. Writes the `crash` frame on the crashing thread's context: kind, signal and code, fault address (`si_addr`), program counter (from `ucontext` on Linux x86-64 and aarch64), and the panic message or signal name.
6. Lets the normal handling continue: the previous panic hook, or the signal's default action (core dump, exit status naming the signal). The handler restores the default action itself after the dump instead of using `SA_RESETHAND`, which would kill a second thread that faults during the dump before it could wait; that second thread waits (two seconds at most) for the first dump to finish.

`snapshot(reason)` writes the same dump without crashing (kind 3), for watchdogs that detect a deadlock or a missed deadline. `dumps()` returns how many dumps the process wrote.

The handler allocates nothing, formats nothing on the signal path, never sleeps, takes no blocking lock, and uses `open` / `write` / `close` only. Since the release after 0.2.3, it runs on an alternate signal stack of 64 KiB (`SIGNAL_STACK`) with a guard page, which `install` sets up for the calling thread and every thread gets when it first logs after that; a thread that never logs keeps its default one. The default Rust gives a thread is `max(SIGSTKSZ, AT_MINSIGSTKSZ)`, often 8 KiB, and the CPU's saved state takes a few KiB of it: too little for a debug build's dump, which then overflowed, wrote only the stream header and died of `SIGSEGV`. C threads run the handler on their own stack (`SA_ONSTACK` with no alternate stack installed). What a dump cannot contain: frames already handed to a transport but still in its buffer, frames dropped because a ring was full (counted), and anything after `SIGKILL` or power loss. Tests crash real child processes (Rust: panic on division by zero, null write, hardware divide by zero, `abort`, a segfault while the drain is blocked in a transport write, two caught panics; C: the same crashes plus snapshot) and check that every record logged before the crash is in the dump.

MCU crash capture (exception frame into flash or `.noinit`) is on the roadmap.

## 9. Host decoder

The decoder (`rtlog-decode`, a library plus the `rtlog` CLI) turns a stream and a contract into text, JSON lines or a Perfetto trace, frame by frame.

**Pipeline**

1. Read the stream header; check `wire_version` and `contract_hash` against the contract given on the command line (`--force` overrides). The contract loader validates before use: level names, `nargs` ≤ 32, format string length, placeholder count equal to `nargs`, no IDs in the control range.
2. Parse frames per context using the contract's argument count and the wire tags, treating the stream as untrusted input: every length is capped, allocations are bounded, and a mutation test over corrupted and truncated streams checks that nothing panics. After a parse error the decoder looks for the next resync marker; if there is one it resumes there (the output gets a `GAP` line, a `gap` JSON record and a Perfetto marker, and `Decoded::gaps` / `skipped_bytes` count them), and the `ctx_time` frames that follow a resync make times exact again. If there is none, decoding ends and reports the byte offset; records before it are kept. `Decoder::recovering(false)` turns recovery off.
3. Reconstruct absolute timestamps per ring from deltas and `ctx_time` frames; sort every item by timestamp for the whole-capture outputs (the stdout mode prints in arrival order).
4. Render each record, span and crash.

**Output modes**

| Mode | Flag | Use |
| --- | --- | --- |
| Text | default | `12.345678 [motor] INFO  pid out=0.42 err=-3`, colours with `--color` |
| JSON lines | `--json` | one object per record, with `type` (`log`, `span`, `dropped`, `gap`, `crash`); a `log` record has `ts_ns`, `ctx`, `ctx_name`, `level`, `site`, `module`, `file`, `line`, `message`, `args` ([all fields](reference.md#json-lines-output)) |
| Perfetto / Chrome trace | `--perfetto` | contexts to tracks, spans to slices, records to instant events, crashes to markers; opens in ui.perfetto.dev |
| Frame by frame | library (`stream::Session::decode_frame`) | for in-process consumers such as the stdout mode |

## 10. Integration with open logging ecosystems

Implemented today: the Perfetto / Chrome trace output (section 9), which also opens in Speedscope and `chrome://tracing`, and JSON lines for any log pipeline. The span model shared by every output is `span_open{id, parent, site}` and `span_close{id}` with 64-bit IDs (high 32 bits = context, low 32 = counter) and the parent carried explicitly, so nesting survives.

Backends for the `log`, `tracing` and `defmt` facades, and outputs for syslog, OTLP, CTF and SystemView, are on the roadmap.

## 10b. Threat model

The design removes the classic log attack surface (format-string injection) and adds two assets to protect, the contract and the stream. Status reflects the code.

| # | Threat | Severity | Mitigation | Status |
| --- | --- | --- | --- | --- |
| V1 | Contract leak: anyone with it reads every capture | High | Store it with the ELF under build-pipeline access control | Practice |
| V2 | Format strings ship in the image | High | Strip the descriptor section post-build | Open (roadmap) |
| V3, V4 | Stream capture or tampering on an untrusted transport | Medium | AEAD blocks | Open (roadmap) |
| V5 | Malformed stream crashes the decoder | Medium | Untrusted parsing, capped lengths, mutation test in CI | Implemented |
| V6 | Malicious contract JSON | Medium | Loader validates every entry; rejects, never partially applies | Implemented |
| V7 | Sensitive values logged in typed args | Medium | `#[rtlog(redact)]` and a lint | Open (roadmap) |
| V9 | Timing side channel from the log call | Low | Fixed-width writes; dynamic strings vary with length | Documented |
| V11 | Log flooding from a compromised task | Low | Per-ring overflow drops only that ring's frames | Implemented |

Residual risks accepted: a debugger with full access reads rings in RAM; key provisioning and encryption are not designed yet.

## 10c. Benchmark model

The benchmark answers: cost per call, sustainable throughput before drops, binary size saved, and how it compares with the loggers people would otherwise use. Every figure comes from a public harness so anyone can rerun it.

**Metrics**

| Metric | Definition | How measured |
| --- | --- | --- |
| Call latency | Cycles from macro entry to return, hot cache, ring not full | `examples/timing.rs`: 2 000 batches of 256 calls, ring drained between batches, p50 / p99 of the per-batch mean; the core clock is measured with a dependent add chain |
| Throughput ceiling | Frames per second through ring and drain | same report |
| Wire bytes per frame | Bytes on the transport | same report |
| Binary size | Code, read-only data, descriptors, RAM for a firmware with 0, 1 and 10 call sites, and with `core::fmt` | `size/mcu`, `scripts/size_report.py` |
| Contended | 8 threads logging at once, per-thread mean | same report |

**Workloads** (`examples/timing.rs`, `benches/`)

1. `W1 scalar`: `info!("x={} y={}", u32, u32)`, the common case.
2. `W2 struct`: 6 mixed fields.
3. `W3 string`: a 32-byte dynamic string.
4. `W4 burst`: calls back-to-back from one thread (throughput).
5. `W5 contended`: 8 threads logging at once.

Baselines in the same report: `write!` of the same text into a `String`, and a timestamped `writeln!` line into a buffer (what `env_logger`-style loggers do before I/O).

**Budgets** (`scripts/spec_check.py` reads them from this section)

- Speed: ≤ 50 cycles per W1 call on x86-64 and aarch64 application cores, best p50 of the timing report. CI reports the figure on every run and warns when it is over; it is not a hard gate, because shared runners vary by 10–20 cycles between runs (`--strict-timing` gates it on a dedicated machine).
- Size on Cortex-M4F (`thumbv7em-none-eabihf`, `opt-level = "s"`, LTO, `size/mcu`): fixed runtime code ≤ 2560 bytes, and ≤ 64 bytes of code per additional call site. Deterministic; gated.
- RAM: the rings the application declares plus a fixed per-ring state; nothing grows at runtime.

Budgets are stated in CPU cycles, because a call's cost is a cycle count and its time is cycles ÷ clock: 50 cycles is 15 ns at 3.3 GHz but 500 ns at 100 MHz. Nanoseconds are quoted only for a named clock. The Cortex-M cycle budget (≤ 150 per W1 call) is still a placeholder. The first hardware measurement (an STM32L432KC at 80 MHz, the C/Zephyr path, [report](stm32-comparison.md)) is 447 median cycles for one `u32` and 520 for three, about three times over it; those cases include the timing wrapper and are not the W1 shape (two arguments). The budget stays until that path is optimised and a W1-shaped case is measured.

**Current figures** are in [Performance and size](performance.md), republished by CI on every run.

## 11. Decisions

| # | Decision | Chosen |
| --- | --- | --- |
| 1 | Argument encoding | Fixed width, one tag byte per argument; varint only for site/ctx/ts metadata |
| 2 | Site IDs | 32-bit content hash in Rust; descriptor index in C; `elf16` reserved |
| 3 | Strings | Binary first; dynamic strings capped at 127 bytes with a truncation flag |
| 4 | Buffers | Per-context SPSC rings, static on `no_std`, heap on `std`, bounded without a drain |
| 5 | Contract emission | Link-section descriptors read from the image or written by the program |
| 6 | `Log` trait | `TAG`, `MAX`, `write`; types travel as tags |
| 7 | Format grammar | `std::fmt` subset: `{}` `{:?}` `{:x}` `{:X}` `{:b}` `{:.N}` `{:str}`; inline captures rejected |
| 8 | Spans | Two control frames, 64-bit IDs (32-bit context + 32-bit counter), explicit parent, own on/off switch |
| 9 | Multi-core | Ring per thread or core; `ctx` names the executing thread |
| 10 | Versioning | `wire_version` in the header; contract fields versioned separately |
| 11 | Tests | Miri, ThreadSanitizer, AddressSanitizer, 2-minute soak, QEMU on 4 architectures, child-process crash tests, mutation test of the decoder |
| 12 | Debug stdout | `rtlog_decode::stdout`: decoder in the drain thread, level from `RTLOG` |
| 13 | Naming | `rtlog` everywhere: crates `rtlog` and `rtlog-decode`, macros `rtlog::`, CLI `rtlog`, files `.rtlog`, env `RTLOG_*` |
| 14 | Licence | MIT OR Apache-2.0, at the user's option |
| 15 | Log hot path | Encode in place in the ring on hosts; stack buffer + one shared push on bare metal (code size) |
| 16 | Crash log | Read-only dump of every ring's unread tail + `crash` frame, one numbered file per dump, from a panic hook and async-signal-safe handlers, never waiting for the drain |
| 17 | C / C++ | Header-only producer; site ID = descriptor index; contract read from the ELF; pthread destructor frees thread slots |
| 18 | Budgets in CI | Sizes gated; cycles reported |

**Sources**

- [defmt book](https://defmt.ferrous-systems.com/)
- [NanoLog repository](https://github.com/platformlab/nanolog) and [NanoLog paper](https://purl.stanford.edu/pd423tg1235)
- [Postform crate](https://lib.rs/crates/postform_persist)
- [delog crate](https://docs.rs/delog)
