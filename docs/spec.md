# Real-Time Deferred Logger — Specification v0.2

Nov 2, 2026 · @Niels

v0.2 (Nov 4, 2026) aligns the spec with the v0.1 implementation: the API that exists, how site IDs and contracts are produced, the frame layout, the crash log and the stdout mode, and budgets in cycles and bytes that CI enforces (`scripts/spec_check.py`). What remains planned is marked as such; [implementation notes](implementation.md) track the details.

## 0. Developer experience and API

A developer writes `rtlog::info!("pid out={} err={}", out, err)` and reads the line on a terminal; everything between is automatic, and the logger has no measurable effect on the system it observes, on an MCU or on Linux.

**Zero-impact principle** (what "without affecting the system" means)

- No syscalls, no heap, no locks, no formatting on the log call: one counter read, one memcpy per argument, one release store.
- No background activity unless asked: on `std` the drain thread is opt-in; the alternatives are `rtlog::poll()` from the application's own idle point, or an external process reading a shared-memory ring so the application spends only the memcpy.
- Bounded memory: rings are sized at init and never grow; memory per context is known before deployment.
- No stdout/stderr, no signal handlers, no `fork` or `exec` interaction; safe inside hard real-time threads, SCHED\_FIFO loops and ISRs.
- Disabled sites compile to nothing: `RTLOG_LEVEL=off` removes every call and every contract entry; a binary built that way is byte-identical to one without logging.
- Dropping is the only overload behaviour: a slow transport can lose frames but can never slow the producer.

**Public API** (crate `rtlog-core`, prefix `rtlog`)

```rust
// Call sites: same shape as println!, every arg implements Log
rtlog::trace!("...", a, b);  rtlog::debug!  rtlog::info!  rtlog::warn!  rtlog::error!
rtlog::info!(to: &mut producer, "...", a);    // explicit producer (no_std, static rings)
let _s = rtlog::span!("db.query");            // span: a slice in Perfetto, closes on drop
let s = rtlog::span!(to: &mut producer, "pid.update"); producer.span_exit(s);

// Setup
static MOTOR: Ring<1024> = Ring::new();       // no_std: one static ring per task
let (producer, consumer) = MOTOR.split(ctx);  // once; ctx 0 = ISR
rtlog::set_clock(dwt_cycles, 168_000_000);    // no_std clock; std uses TSC / CNTVCT
rtlog::set_level(Level::Info);                // runtime level, one atomic store

// Draining: pick one
rtlog::spawn_drain(transport, period);        // std: dedicated thread, opt-in
rtlog::drain::drain_ring(&mut consumer, &mut transport, &mut scratch); // idle loop / DMA
rtlog_decode::stdout::init();                 // std: text on stdout, level from RTLOG (section 8b)

// Crash log (section 8b)
rtlog::crash::install("crash.rtlog");         // panic, SIGSEGV, SIGFPE, SIGBUS, SIGILL, SIGABRT
rtlog::crash::snapshot("deadlock suspected"); // same dump, without crashing

rtlog::stats() -> Stats                       // frames, bytes, drops, live contexts
// Planned: intern!, #[derive(Log)], per-crate levels, shm::export
```

C and C++ use the same model through `c/include/rtlog.h`: `RTLOG_INFO("x={}", x)`, `RTLOG_SPAN_BEGIN` / `RTLOG_SPAN` (RAII in C++), `rtlog_drain(write, user)`, `rtlog_crash_install(path)`.

**Usage by target**

| Target | Setup lines | Reading logs |
| --- | --- | --- |
| Cortex-M, RTOS | `static RING: StaticRing<1024>`; `register` in each task; `poll` in idle hook with `RttTransport` | `cargo rtlog run` flashes, attaches, decodes live |
| Embedded Linux daemon | `rtlog::init`; `shm::export`; nothing else | `rtlog attach /my-app` from a shell or over SSH; `--out otlp` to ship upstream |
| Desktop app or service | `rtlog::init`; `spawn_drain(FileTransport)` or `--features stdout` | `RTLOG=info cargo run` prints text; `rtlog decode app.rtlog` later |
| Library crate | only call sites; no init, no transport | inherits the host application's setup; silent if none |

**Build and retrieval flow**

1. `cargo build`: the macro hashes each site and writes its entry; the build script merges to `target/rtlog.contract.json` and fails on collisions.
2. CI archives the contract next to the firmware image, keyed by contract hash.
3. A stream or capture carries that hash in its header; `rtlog` finds the contract in `~/.rtlog/contracts/` or a team artifact store and decodes. No manual matching of firmware to symbols.
4. Dependencies using `log`, `tracing` or `defmt` route into the same stream through the backends in section 10, so one viewer shows everything.

**What it buys over printf**

- Flash: format strings and formatting code are gone, typically 20–60% of a log-heavy firmware image.
- Speed: tens of nanoseconds per call; never blocks an ISR or a control loop.
- Security: paths, function names, internal states and anything written in a message are absent from the shipped binary; a captured stream is unreadable without the contract, which never leaves the build pipeline.
- Archive: a `.rtlog` capture is 5–10× smaller than text and keeps typed fields.

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

The logger makes every log call bounded, lock-free and allocation-free, keeps format strings out of the binary, and renders human-readable text only on the host. One core crate serves bare-metal MCUs, embedded Linux and desktop.

**Goals**

- Log call cost: tens of nanoseconds, constant per site, no formatting, no heap, no locks.
- Zero format strings in the target binary; a contract file carries them.
- Portable: `no_std` core, `alloc` and `std` feature gates, same wire format on every target.
- Per-task context: each task, thread or ISR owns its own ring buffer.
- Human-readable output on the host, plus structured output (JSON lines) for tooling.
- Lossy by design: overflow drops frames and counts them; the call never blocks.

**Non-goals**

- Log querying, indexing or analytics engine.
- Log rotation, retention or encryption (left to the transport sink).
- Replacing a full tracer (spans, flame graphs); a bridge to those exists in section 10.

## 2. Study of existing solutions

Deferred formatting is proven on both ends of the spectrum: defmt on MCUs and NanoLog on servers. None of them is portable across both, which is the gap this spec fills.

| Solution | Target | Format strings | Hot path | Per-context buffers | Transport | Gap for us |
| --- | --- | --- | --- | --- | --- | --- |
| [defmt](https://defmt.ferrous-systems.com/) (Rust) | MCU, `no_std` | Interned at compile time, IDs from ELF linker section | Binary args, fixed width | One global RTT buffer (critical section) | RTT, probe-rs | ELF-only, single buffer, no std story |
| [Postform](https://lib.rs/crates/postform_persist) (C++) | 32-bit MCU | Same idea as defmt | Binary args | Single | RTT, TTY | C++, MCU-only |
| [NanoLog](https://github.com/platformlab/nanolog) (C++) | Linux x86 | Extracted by preprocessor/constexpr | Binary args, leading-zero compression | Per-thread staging buffers, lock-free, \~1 MB each | Background thread to file | Linux-only, heavy RAM, one core for the emitter |
| [delog](https://docs.rs/delog) (Rust) | MCU | Kept in binary | Formats on device into a static ring | Single global ring | Flush in idle loop | Formatting still on device |
| LTTng / ftrace | Linux kernel + user | Tracepoint metadata in CTF | Binary, per-CPU lock-free ring | Per-CPU | Shared memory to consumer daemon | Linux-only, no MCU |
| SEGGER SystemView | MCU (RTOS) | Event IDs, description file | Binary events | Single RTT buffer | RTT | Closed format, vendor tool |

Takeaways carried into this spec:

- Interned format strings + binary args (defmt, NanoLog) are the right hot path.
- Per-context single-producer rings (NanoLog, ftrace) remove contention without CAS.
- The contract must be a standalone artifact, not an ELF section, to work on macOS, Windows and dynamic binaries.
- Compression belongs to the drain, not the log call (NanoLog pays for it on the hot path; we move it).

## 3. Architecture overview

The system has three stages: a proc-macro assigns each log site an ID and writes its format string to a contract file at build time; at runtime each task writes raw binary frames into its own ring; the host joins stream and contract to render text.

> Diagram (build → target → host) is kept in the [source doc](https://claude.ai/artifact/4s4FKRK2xDaEeV6JvzDmMM).

The contract never travels through the target: the device sends only IDs, timestamps and raw argument bytes, and the host supplies the strings. Section 10 adds adapters on the host output and the target input so the logger plugs into existing ecosystems.

## 4. Contract

A site ID is a 32-bit content hash, so the scheme works on every target and object format; a linker-assigned 16-bit ID is an opt-in optimisation for single-image MCU builds.

**Site ID**

- `id = fnv1a32(crate@version | file | line | column | format | arg-type-list)`.
- Including the arg types means a type change at a site yields a new ID; a stale contract can never mis-decode silently.
- Birthday bound at 32 bits is safe into tens of thousands of sites; 16 bits would collide at \~300.
- Feature `elf-ids`: the macro emits a zero-size symbol in a dedicated section and the linker-assigned index becomes a `u16` ID. The contract records which scheme a build used.
- Implemented (v0.1): Rust IDs hash `module | file | line | column | format | level | nargs`; argument types travel on the wire as tags (section 5) instead of in the hash, because a declarative macro cannot see them. C and C++ IDs are the descriptor's index in the `rtlog_sites` section plus one, since C has no compile-time string hashing; the log path does no hashing either way.

**Contract entry** (one JSON line per site)

```json
{"id":3202914573,"level":"info","crate":"motor@0.4.1","file":"src/pid.rs","line":88,"fmt":"pid out={} err={}","args":["f32","i16"],"max_len":14}
```

- Interned static strings (section 6) use the same ID scheme and live in the same file as `kind: "str"` entries.
- Derived `Log` types add a `kind: "type"` entry with their field names and types, referenced by hash from `args`.

**Generation and merging**

1. The proc-macro emits one `static` descriptor per site (hash, level, crate, file, line, format string, arg schema) into a dedicated link section: `.rtlog.sites` on ELF, `__DATA,__rtlog_sites` on Mach-O, `.rtlog$sites` on PE (the technique `linkme` uses). Derived `Log` types and interned strings emit descriptors into the same section. No files are written at expansion time, so parallel crate builds cannot race.
2. `rtlog contract --elf <image>` reads the descriptors with the `object` crate (Rust `linkme_SITES`, C `rtlog_sites`; `RELATIVE` relocations applied for position-independent executables), validates them and writes the JSON-lines contract. Implemented. Stripping the section from the shipped image (`objcopy --remove-section`, or on MCU a linker script placing it in a non-loaded segment) is planned; until then the binary is self-describing. Descriptors are `repr(C)` with a fixed layout so this works on any ELF, firmware included.
3. `cargo rtlog verify <image>` is the release gate: it fails if the image still contains the descriptor section or any contract format string, so strings can never ship by mistake (threat V2). CI runs it on every release artifact.
4. The merged file is versioned with the firmware; CI archives it with the ELF, keyed by contract hash.

**Binding stream to contract**

- Stream header carries `contract_hash` (hash of the merged contract) and `scheme` (`hash32` or `elf16`).
- The decoder refuses a mismatch unless run with `--force`, then marks every line decoded under a mismatched contract.
- Two independent version numbers: `wire_version` in the stream header changes only for frame-grammar changes; `contract_schema` in the contract file changes for descriptor-format changes. The decoder reads every wire version ≥ 1 forever and the last two contract-schema majors. New control frames take new IDs; old ones stay decodable.

## 5. Wire format

Arguments are written fixed-width so every site has a compile-time maximum frame size; only the small metadata fields are varint, and compression is a drain option, never a log-call cost. Everything is little-endian.

**Stream header** (once per session, repeated on every resync)

| Field | Width | Content |
| --- | --- | --- |
| magic | 4 B | `RTL1` |
| wire\_version | u8 | frame grammar version (1) |
| scheme | u8 | 0 = hash32 IDs, 1 = elf16 IDs |
| contract\_hash | u32 | hash of merged contract |
| clock\_hz | u32 | timestamp tick rate |
| ptr\_width | u8 | 4 or 8, drives `usize` width |
| flags | u8 | bit 0 drain compression, bit 1 authenticated blocks, bit 2 encrypted blocks |
| key\_id | u8 | which provisioned key signs/encrypts (0 = none) |

**Log frame**

| Field | Width | Content |
| --- | --- | --- |
| site\_id | u32 or u16 | per scheme |
| ts\_delta | varint ≤ 5 B | ticks since previous frame in this ring |
| ctx | varint ≤ 2 B | context ID (0 = ISR) |
| args | per argument: 1 B type tag + value | raw little-endian values in call order; tier-3 strings and bytes as length + bytes |

- Frame length is not transmitted; the decoder reads the contract's argument count and walks the tagged values.
- On hosts the producer reserves a worst-case slot (header + 240 B of arguments) and, when it fits before the wrap point, encodes the frame in place and publishes it with one release store; otherwise it encodes into a stack buffer and copies across the wrap. On bare metal it always encodes into a stack buffer and calls one shared push, to keep per-call-site code small (section 10c).

**Control frames** (site\_id reserved range `0xFFFF_FF00..`)

| ID | Name | Payload |
| --- | --- | --- |
| `..00` | resync | full u64 timestamp, repeat of stream header |
| `..01` | dropped | ctx, u32 count of frames dropped since last report |
| `..02` | ctx\_register | ctx ID, interned name ID |
| `..03` | ctx\_close | ctx ID |
| `..04` | span\_open | timed like a log frame (ts delta, ctx), then u32 span site ID, u64 span ID, u64 parent ID (0 = root) |
| `..05` | span\_close | timed like a log frame, then u64 span ID |
| `..06` | time\_sync | u64 ticks, u64 external time in ns (RTC, GPS, NTP) |
| `..07` | crash | timed like a log frame, then u8 kind (1 panic, 2 signal, 3 snapshot), i32 signal, i32 code, u64 fault address, u64 pc, message (len + UTF-8); section 8b |
| `..08` | block\_tag | 16 B AEAD tag for the block that ends here (section 5, security) |
| `..09` | ctx\_time | ctx, u64 absolute ticks: sets the context's clock, so a crash dump that holds only the tail of a ring still decodes to absolute times |

- The drain emits a resync at least every 64 KiB or 1 s, whichever first, so a corrupted length can never desync the stream for long.
- Compression (`flags` bit 0): the drain applies NanoLog-style leading-zero stripping per frame, or a block codec such as LZ4 on `std`; the frame grammar above is what the decoder sees after decompression.
- Span IDs: high 16 bits = context ID, low 48 bits = per-context counter, so IDs are unique with no atomics. An async task keeps its span ID when it migrates; the frame's `ctx` says which thread or core executed it.

**Security** (`flags` bits 1–2, applied by the drain, never by producers)

- A block = the bytes between two resync frames. With bit 1 set, each block ends with a `block_tag` holding the ChaCha20-Poly1305 tag computed over the block with the header as associated data; with bit 2 set, the block body is also encrypted with the same AEAD. Key = the provisioned device key named by `key_id`.
- Nonce = 32-bit boot counter + 64-bit block counter (96 bits). The boot counter is persisted (flash or RTC backup register) and incremented at `init`; a key must be rotated before the boot counter wraps, so a nonce is never reused after a reset (threat V8). The stream header carries the boot counter so the decoder can derive nonces.
- Encryption implies authentication; authentication alone is for cases where the transport is already private (SSH, TLS) but tamper evidence in archives is wanted.
- Cost lands on the drain only: about 1.5 cycles/byte in software on Cortex-M4, or free with a hardware AEAD. Producers see no change.
- Key provisioning: `rtlog-core` exposes `trait KeyStore { fn key(&self, id: u8) -> Option<&[u8; 32]>; }`; integrators implement it over a secure element or a flash slot. The contract file is stored with the ELF under the same access control and is never present on the device in plain form.

## 6. Argument model

Every argument implements the `Log` trait and is written as raw bytes; strings go through three graded, bounded escape hatches so nobody falls back to `println!`.

```rust
pub trait Log {
    const SCHEMA: &'static Schema;     // contributes to the contract
    const MAX: usize;                  // upper bound on wire bytes
    fn write(&self, w: &mut Writer);
}

pub enum Schema {
    U8, U16, U32, U64, I8, I16, I32, I64, F32, F64, Bool,
    Str { max: u16 }, Bytes { max: u16 },
    Option(&'static Schema), Array { of: &'static Schema, n: u16 },
    Struct { hash: u32 }, Enum { hash: u32 },
}
```

- `#[derive(Log)]` emits `Schema::Struct { hash }` and a descriptor with field names and types into the `.rtlog.sites` section (section 4); the hash is the type's path plus field list.
- Generics compose at monomorphisation: `impl<T: Log> Log for Wrapper<T>` builds its `SCHEMA` from `T::SCHEMA`.
- Floats are IEEE-754 little-endian; no alignment padding on the wire.

| Tier | Form | Wire | Bound | Availability |
| --- | --- | --- | --- | --- |
| 1 Binary | ints, floats, bool, `Option<T>`, arrays, `#[derive(Log)]` structs and enums | raw fields in order; enum = discriminant + variant fields | `MAX_SIZE` const | all targets |
| 2 Interned | `&'static str` literal, or `intern!(s)` | u32 ID from contract | 4 B | all targets |
| 3 Dynamic | `{:str}` on `&str`, `{:hex}` on `&[u8]` | `u8 len` + bytes, truncated to cap | 127 B (bit 7 of the length byte flags truncation; UTF-8 kept whole) | all targets |
| 4 Display | `{:display}` / `{:debug}` on any `Display`/`Debug` | formatted into a stack buffer, then tier 3 | same cap | `std` feature only, lint warning |

Rules:

- `#[derive(Log)]` records field names and types in the contract under the type's hash, so the host prints `Header { src: 2, dst: 3 }`.
- `#[rtlog(redact)]` on a field writes a 32-bit hash of the value instead of the value, so equality is still visible in logs but the content is not (threat V7). A lint warns on `{:str}` or `{:display}` applied to identifiers containing `key`, `token`, `secret` or `password`.
- `usize`/`isize` are recorded at the target's pointer width (header `ptr_width`), so the same source compiles to 4 or 8 bytes per target.
- Truncation in tier 3 sets a flag bit in the length byte (bit 7); the decoder renders `…`.
- Forbidden on the log call: `format!`, `ToString`, any heap allocation, any formatting trait on `no_std`.
- `{}` with a non-`Log` type is a compile error with a hint pointing at `derive(Log)` or `{:display}`.

**Format grammar** (a strict subset of `std::fmt`; anything else is a compile error)

| Syntax | Meaning |
| --- | --- |
| `{}` / `{name}` | positional or named arg, rendered by its schema |
| `{:?}` | same as `{}`; kept so `Debug`-style code reads naturally |
| `{:str}` / `{:str<N>}` | tier-3 dynamic string, cap 64 or N bytes |
| `{:hex}` / `{:bin}` | bytes or ints in hex / binary |
| `{:display}` / `{:debug}` | tier 4, `std` only |
| width, precision, alignment, `+`, `#` | accepted, stored verbatim in the contract, applied by the host |

## 7. Runtime

Each context owns a single-producer ring; the one drain is the only consumer, so the hot path needs two atomic loads, one memcpy per argument and one release store.

**Rings**

| Backing | Feature | Allocation | Default size | Registration |
| --- | --- | --- | --- | --- |
| `StaticRing<const N>` | `no_std` | `static` in `.bss` | 1 KiB | `rtlog::register(&RING, "motor")` once per task |
| `HeapRing` | `std` | `Box<[u8]>` at thread start | 64 KiB | `thread_local!` lazily on first log |
| ISR ring | all | static | 256 B | context 0, written only from interrupt context |

- `N` is a power of two and ≥ the largest frame in the contract (checked by `cargo rtlog contract`).
- Sizing rule: `N ≥ burst_rate × avg_frame × max_drain_latency`.
- Registry: `no_std` keeps a fixed table of `AtomicPtr<RingHeader>` (`MAX_CONTEXTS`, default 16); `std` keeps `Mutex<Vec<Arc<RingHeader>>>`. A `std` ring is marked closed on thread exit and freed only after the drain has consumed it.

**Multi-core and async**

- Context = execution unit: one ring per RTOS task, per OS thread, or per core on bare-metal SMP where tasks do not migrate. The ring count is therefore bounded by the runtime, never by the number of async tasks.
- Embassy and Tokio integrations open a span per task (section 5, span frames); frames carry the task's span ID while `ctx` records the executing thread or core, so the host can follow a task across migrations.

**Producer / consumer contract**

1. Producer loads `tail` (Acquire), computes free space, and on overflow applies the ring's policy and returns.
2. Producer writes the frame at `head`, then stores `head + N` (Release). The frame is invisible until that store, so the drain never sees a partial frame.
3. Consumer loads `head` (Acquire), hands the contiguous region to the transport, stores `tail` (Release).
4. Invariant: exactly one producer and one consumer per ring, ever. No CAS, no spin.

**Overflow policy** (fixed at registration)

- `DropNewest` (default): the new frame is discarded; cheapest.
- `DropOldest`: the consumer-side cursor is advanced by whole frames; requires a length-tagged frame layout in the ring, so it costs one extra byte per frame.
- Both increment a per-ring drop counter that the drain reports with a `dropped` control frame.

**Clock**

```rust
pub trait Clock { fn now() -> u64; const HZ: u32; }
```

Implementations: DWT cycle counter (Cortex-M), RTOS tick, `clock_gettime(CLOCK_MONOTONIC)`, TSC. The producer stores the delta from the ring's last timestamp; the drain resync frame carries the absolute value.

**Levels and filtering**

- Levels `trace, debug, info, warn, error`; a compile-time floor removes sites entirely (`RTLOG_LEVEL=info` drops trace/debug from the binary and the contract).
- A runtime per-crate mask (`AtomicU8` lookup, one load) lets the field switch levels without rebuilding.

## 8. Transport and drain

The drain is one low-priority consumer that round-robins the registered rings and pushes bytes through a pluggable `Transport`; it is the only place allowed to block, compress or batch.

```rust
pub trait Transport {
    fn write(&mut self, bytes: &[u8]) -> Result<usize, Full>;  // may be partial
    fn flush(&mut self);
}
```

| Transport | Target | Notes |
| --- | --- | --- |
| RTT | MCU with debugger | probe-rs / J-Link; up-channel, non-blocking mode |
| UART / USB CDC | MCU in the field | DMA-driven; drain runs on DMA completion |
| Shared memory | Linux, multi-process | ring in `memfd` / `shm_open`, segment mode 0600 and producer-owned, readers map it read-only (threat V10); consumer daemon |
| UDP | Linux, desktop | one datagram per batch, resync every datagram |
| File | Linux, desktop | append; rotation left to the sink |
| Stdout bridge | desktop dev loop | host decoder run in-process (section 10) |

**Drain scheduling**

- RTOS: idle hook or lowest-priority task; `no_std` bare metal: main loop or DMA callback.
- `std`: a dedicated thread, `park_timeout(1 ms)`, woken early by a producer when a ring crosses a high-water mark (a single `AtomicBool` + `unpark`).
- Policy: drain the ring with the most pending bytes first, then round-robin; emit `dropped` control frames whenever a counter is non-zero.

**Backpressure**

- A full transport never propagates to producers; the drain leaves bytes in the ring and retries. If the ring then overflows, the producer's own policy applies.
- Batching: the drain writes whole frames only, so a transport write boundary is always a frame boundary.

## 8b. Debug stdout and crash log

In debug builds a developer sees text on stdout with no tooling; in any build a crash freezes the rings and saves them with the fault address, so the last log lines before a crash are always recoverable.

**Debug stdout** (implemented: `rtlog_decode::stdout::init()`)

- The host decoder runs in the drain thread behind a text transport; frames are decoded there and printed, so `cargo run` reads like `env_logger`. The level comes from `RTLOG` (`trace` … `error`, `off`); colours when the output is a terminal and `NO_COLOR` is unset; stdout or stderr.
- Only the drain formats; call sites keep the binary path, so timings measured with it on are representative.
- The contract comes from the running binary's own descriptors.
- Planned: on MCU the same through RTT or semihosting from the drain, for boards without a host tool.

**Crash log** (implemented on Unix with `std`: `rtlog::crash::install(path)`; C: `rtlog_crash_install(path)`)

On a panic, `SIGSEGV`, `SIGBUS`, `SIGFPE`, `SIGILL` or `SIGABRT` (so `abort()` and `panic = "abort"`), the handler:

1. Freezes logging (runtime level off) for fatal signals, so the snapshot is consistent. A panic does not freeze: the program may survive it.
2. Opens the crash file (`open(2)`, path fixed at install) and writes a stream header.
3. For every context, without consuming anything: its name, any drop count, a `ctx_time` frame (newest timestamp minus the deltas of the unread frames), then every unread frame, oldest first.
4. Writes the `crash` frame on the crashing thread's context: kind, signal and code, fault address (`si_addr`), program counter (from `ucontext` on Linux x86-64 and aarch64), and the panic message or signal name.
5. Lets the normal handling continue: the previous panic hook, or the signal's default action (core dump, exit status naming the signal).

`snapshot(reason)` writes the same dump without crashing (kind 3), for watchdogs that detect a deadlock or a missed deadline.

The handler allocates nothing, formats nothing on the signal path, takes no blocking lock (`try_lock` on the ring registry, a few attempts), and uses `open` / `write` / `close` only. What a dump cannot contain: frames already handed to a transport but still in its buffer, frames dropped because a ring was full (counted), and anything after `SIGKILL` or power loss. Tests crash real child processes (Rust: panic on division by zero, null write, hardware divide by zero, `abort`; C: the same plus snapshot) and check that every record logged before the crash is in the dump.

Planned for MCU: the fault context below, captured from the exception frame.

   | Field | MCU (Cortex-M) | Linux / desktop |
   | --- | --- | --- |
   | reason | HardFault, UsageFault, panic | signal number, panic message |
   | pc / lr | from the exception stack frame | from `ucontext` |
   | fault address | `MMFAR` / `BFAR`, `CFSR` bits | `si_addr` |
   | stack | `SP` plus the top 64 words | up to 32 return addresses via frame pointers (planned) |
   | build | contract hash, firmware version ID | contract hash in the dump's header |
   | ctx | the context that faulted, if known | the crashing thread's context |

**Crash sinks**

| Target | Sink | Size |
| --- | --- | --- |
| MCU | a reserved flash sector or non-initialised RAM region (`.noinit`) read back after reset | ring sizes + 512 B |
| Linux | `/var/lib/rtlog/crash-<pid>-<ts>.rtlog`, written with `write(2)` only (async-signal-safe) | same |
| Desktop | next to the binary or `$RTLOG_CRASH_DIR` | same |

When `flags` bit 2 is on, the crash dump is written through the same AEAD as the stream, using a dedicated nonce space (boot counter + block counter `0xFFFF_FFFF_FFFF_FFFF`), and the sink is wiped after a successful upload, so a later firmware or an attached debugger reads nothing useful from `.noinit` (threat V12).

**Reading it**

- `rtlog crash dump.rtlog --elf firmware.elf` prints the last lines before the fault, then symbolises `pc`, `lr` and the stack addresses with the ELF's debug info, so the output ends in `pc = 0x0800_4a1c  pid::update  src/pid.rs:91`.
- On MCU, `cargo rtlog run` checks the `.noinit` region at attach time and offers the previous crash before starting the live session.
- Addresses are only useful with the matching ELF; CI archives the ELF with the contract, keyed by the same hash.

Rules kept from the zero-impact principle: the handler does no allocation, no formatting and no locking; the crash sink is pre-opened or pre-erased at init so the handler only writes.

## 9. Host decoder

The decoder (`rtlog-decode`, a library plus CLI) turns a stream and a contract into text or structured records; it is also the boundary where every ecosystem bridge attaches.

**Pipeline**

1. Read the stream header, check `contract_hash`, load the matching contract (from path, or auto-located by hash in `~/.rtlog/contracts/`). The loader validates the contract before use: struct nesting depth ≤ 8, no cyclic type hashes, `max_len` ≤ 65 535, unique site and type hashes; an invalid contract is rejected, never partially applied (threat V6).
2. Parse frames per context using the contract's arg schema, treating the stream as untrusted input: every length comes from the contract or a capped prefix, allocations are bounded by `max_len`. Recover at the next resync after any parse error, reporting the skipped byte count.
3. Reconstruct absolute timestamps per ring from deltas and resync frames; merge rings in timestamp order with a small reorder window.
4. Render each record.

**Output modes**

| Mode | Flag | Use |
| --- | --- | --- |
| Text | default | `12.345678 [motor] INFO pid out=0.42 err=-3` |
| JSON lines | `--json` | one object per record: ts, ctx, level, site, message, args as typed fields |
| Raw records | library API | `Iterator<Item = Record>` for bridges |

**Record model**

```rust
pub struct Record<'a> {
    pub ts_ns: u64, pub ctx: &'a str, pub level: Level,
    pub site: &'a Site,          // file, line, crate, fmt
    pub args: &'a [Value],       // typed: Int, Float, Bool, Str, Bytes, Struct, Enum
}
```

- Live mode: `rtlog-decode --follow` attaches to RTT, a serial port, UDP or a file and prints as frames arrive.
- Offline mode: decode a captured `.rtlog` file; the same binary file is the long-term archive, far smaller than text.

## 10. Integration with open logging ecosystems

The logger plugs into existing ecosystems at two seams: on the target it can sit behind the `log` and `tracing` facades so existing crates emit through it, and on the host the decoder's `Record` stream feeds every open format. The hot path stays binary either way.

**Target side: being a backend for the facades**

| Facade | How it plugs in | What is lost |
| --- | --- | --- |
| `log` crate | `rtlog-log`: implements `log::Log`. The facade hands over a `fmt::Arguments`, already a formatting request, so the backend renders it into the tier-4 stack buffer and sends it as a dynamic string. `std` only. | Zero-cost path; every `log::info!` from a dependency costs a format on device |
| `log` + `kv` | Same backend reads the key-value pairs and sends them as typed args where the value is an int/float/bool/str, falling back to tier 3 for the rest | Nothing for typed values; message text still formatted |
| `tracing` | `rtlog-tracing`: a `Subscriber`/`Layer`. Events map to log frames; `span.enter/exit` map to two control frames (`span_open`, `span_close`) with the span's interned name and ID, so the host can rebuild the span tree. Field values go through the same typed path as `kv`. | Dynamic span fields that are `Debug`-only are formatted on device |
| `defmt` | `rtlog-defmt`: implements `defmt::Logger` so a `defmt`-instrumented dependency writes into the active `rtlog` ring; the defmt frame is wrapped in a `foreign` control frame and decoded by the host with the ELF. | None on the hot path; the host needs both contract and ELF |

Span model shared by every backend: `span_open{id, parent, name}` and `span_close{id}` control frames with 64-bit IDs (high 16 bits = context, low 48 = counter) and the parent carried explicitly, so nesting survives async yields and thread migration; OTLP `trace_id`/`span_id` and Perfetto slices map directly.

Design rule: facades are for dependencies you do not control. The project's own code calls `rtlog!` macros directly to keep the zero-format path.

**Host side: emitting into open formats**

| Target format | Bridge | Mapping |
| --- | --- | --- |
| syslog / journald | `rtlog-decode --out syslog` | level to severity, ctx to `SYSLOG_IDENTIFIER`, args as journal fields `RTLOG_<NAME>` |
| OpenTelemetry Logs | `rtlog-decode --out otlp` | `Record` to OTLP `LogRecord`: body = rendered message, attributes = typed args, `trace_id`/`span_id` from the tracing bridge when present |
| Perfetto / Chrome trace | `--out perfetto` | contexts to tracks, spans to slices, log records to instant events; opens in ui.perfetto.dev |
| CTF (LTTng, Babeltrace) | `--out ctf` | contract becomes CTF metadata, each site an event class; lets Trace Compass read `.rtlog` captures |
| SEGGER SystemView | `--out sysview` | contexts to tasks, control frames to task switch events; for teams already on that viewer |
| defmt tooling | `--out defmt-print` | emits the text form defmt-print expects, so `probe-rs run` style workflows stay unchanged |

**Live pipe on desktop and Linux**

- `rtlog-decode --follow --out otlp --endpoint http://collector:4318` streams a running process into an existing OpenTelemetry collector; from there Loki, Elasticsearch or Datadog receive it unchanged.
- For a developer loop, `rtlog-host` runs the decoder in-process behind a `Transport` and prints text to stdout, so a desktop build looks like `env_logger` while keeping the binary path.

**Interoperability rules**

- Every bridge consumes `Record`; adding a format never touches the target.
- Span IDs from `tracing` are 64-bit and travel in control frames, so OTLP and Perfetto outputs correlate with other services.
- A contract is published as a build artifact alongside the firmware; CI uploads it where the OTel collector or the decoder service can find it by hash.

## 10b. Threat model and vulnerabilities

The design removes the classic log attack surface (format-string injection, strings leaking internals) but adds two assets to protect, the contract and the device key, and one untrusted input, the stream. Each item below is rated and has a mitigation already in the spec or a required addition.

| # | Threat | Severity | Mitigation | Status |
| --- | --- | --- | --- | --- |
| V1 | Contract leak: anyone with it reads every capture and learns code structure | High | Stored with the ELF under build-pipeline access control; never on device in plain form; stripped from the binary post-build | In spec |
| V2 | Unstripped section shipped: `.rtlog.sites` left in a release image reintroduces all strings | High | `cargo rtlog contract` strips by default; CI check fails a release build whose image still contains the section | In spec (4) |
| V3 | Stream capture on an untrusted transport (UART, UDP) | Medium | AEAD encryption, `flags` bit 2 | In spec |
| V4 | Stream tampering in archives, forged crash evidence | Medium | AEAD authentication per block, `flags` bit 1 | In spec |
| V5 | Malformed stream crashes the decoder (host or CI) | Medium | Decoder treats input as untrusted: bounded allocations, length caps from the contract, fuzzing in CI | In spec |
| V6 | Malicious contract JSON: huge `max_len`, recursive struct hashes | Medium | Contract loader validates: depth limit 8, `max_len` ≤ 65535, hash uniqueness | In spec (9) |
| V7 | Sensitive values logged by developers (keys, tokens, PII) in typed args | Medium | `#[derive(Log)]` field attribute `#[rtlog(redact)]` writes a hash instead of the value; lint for `{:str}` on fields named `*key*`, `*token*`, `*password*` | In spec (6) |
| V8 | Nonce reuse after reset: block counter restarts at 0 with the same key | High if encrypted | Nonce = 32-bit boot counter (persisted) + 64-bit block counter; a key is never used across a boot-counter wrap | In spec (5) |
| V9 | Timing side channel from the log call | Low | Fixed-width writes make call time data-independent except for tier-3 string length; tier 3 is forbidden in security-critical code paths by lint | Document |
| V10 | Shared-memory ring on Linux: another local process writes garbage into it | Medium | Ring segment opened `0600`, producer-owned; readers get a read-only mapping | In spec (8) |
| V11 | Denial of service via log flooding from a compromised task | Low | Per-ring overflow drops only that ring's frames; other contexts and the drain are unaffected | In spec |
| V12 | Crash sink in `.noinit` RAM readable by a later firmware or a debugger | Low | Crash dump encrypted when `flags` bit 2 is on; sink wiped after successful upload | In spec (8b) |

Residual risks accepted for v1: a debugger with full access reads rings in RAM (plaintext until the drain encrypts); key provisioning and secure-element integration are left to the integrator (open question in section 11).

## 10c. Benchmark model

The benchmark answers four questions with numbers: cost per call, sustainable throughput before drops, binary size saved, and how each compares with the loggers people would otherwise use. Every figure is produced by a public harness so the comparison can be rerun by anyone.

**Metrics**

| Metric | Definition | How measured |
| --- | --- | --- |
| Call latency | Cycles from macro entry to return, hot cache, ring not full | Cycle counter around 1 000 calls, median and p99 |
| Worst-case call | Same, with ring full (drop path) and cold cache | Flush caches between calls, report max |
| Throughput ceiling | Frames per second sustained with zero drops | Ramp the producer rate until `stats().drops > 0` |
| Drain cost | CPU share of the drain at a given frame rate | `perf` on `std`; idle-hook cycle count on MCU |
| Wire bytes per frame | Average bytes on the transport | Count over a fixed workload |
| Binary size | `.text` + `.rodata` delta versus no logging | `size`/`cargo bloat` on the release image |
| RAM | Static + ring bytes per context | Linker map |
| Jitter induced | Added latency to a 1 kHz control loop with logging on | Loop period histogram with and without logging |

**Workloads** (identical across loggers, one file per workload)

1. `W1 scalar`: `info!("x={} y={}", u32, u32)`, the common case.
2. `W2 struct`: a 6-field derived struct.
3. `W3 string`: a 32-byte dynamic string.
4. `W4 burst`: 10 000 calls back-to-back from one task.
5. `W5 contended`: 8 threads or 4 tasks logging at once.
6. `W6 isr`: 10 kHz interrupt logging one `u16`.
7. `W7 control loop`: 1 kHz PID loop logging 3 floats per iteration, measured for jitter.

**Targets and loggers compared**

| Target | Compared loggers |
| --- | --- |
| Cortex-M4 @ 100 MHz (e.g. STM32F4), RTT | defmt, delog, `log` + `rtt-target` text, semihosting printf |
| Cortex-M7 @ 400 MHz, UART DMA | defmt, SystemView (vendor figures), printf over DMA |
| Linux x86-64, 8 cores | NanoLog, spdlog async, `tracing` + `tracing-subscriber` fmt, `log` + `env_logger`, `slog-async` |
| Linux aarch64 (Raspberry Pi 4) | `tracing` fmt, `env_logger`, LTTng-UST |
| macOS and Windows desktop | `tracing` fmt, `env_logger`, `fmtlog` |

**Budgets** (enforced in CI by `scripts/spec_check.py`, which reads them from this section)

- Speed: ≤ 50 cycles per W1 call on x86-64 and aarch64 application cores, best p50 of the timing report.
- Size on Cortex-M4F (`thumbv7em-none-eabihf`, `opt-level = "s"`, LTO, `size/mcu`): fixed runtime code ≤ 2560 bytes, and ≤ 64 bytes of code per additional call site.
- RAM: the rings the application declares plus a fixed per-ring state; nothing grows at runtime.

**Expected results** (hypotheses to verify, not claims)

Budgets are stated in CPU cycles, because a call's cost is a cycle count and its time is cycles ÷ clock: 50 cycles is 15 ns at 3.3 GHz but 500 ns at 100 MHz. Nanoseconds are quoted only for a named clock.

| Logger | W1 cost | Drops under W4 | Strings in binary |
| --- | --- | --- | --- |
| This spec | ≤ 50 cycles on x86-64 / aarch64 (≈ 15 ns at 3.3 GHz); ≤ 150 cycles on Cortex-M4 (1.5 µs at 100 MHz) until measured on hardware | none up to ring capacity, then counted | none |
| defmt | to be measured in cycles on the same board (critical section per frame) | blocks or drops depending on RTT mode | none |
| NanoLog | \~7–18 ns on x86 per its paper (≈ 25–60 cycles at 3.3 GHz) | none until 1 MB buffer full | none |
| spdlog async | 100–300 ns | queue policy: block or overrun | yes |
| `tracing` fmt | 300–1 000 ns | writer blocks | yes |
| printf / semihosting | 10–100 µs | blocks | yes |

**Limits the harness must find**

- The frame rate at which a 1 KiB ring on M4 starts dropping with RTT draining from the idle hook, and the same with UART at 115 200 and at 921 600 baud.
- The ring size per context needed for zero drops under W4 at each drain latency (1 ms, 10 ms, 100 ms).
- Maximum contexts before registry lookup shows in the call latency.
- Drain CPU share where encryption is on versus off.

**Harness layout** (in-repo, `benches/`)

- `benches/std/`: Criterion benches, one per workload, with a feature flag per compared logger.
- `benches/mcu/`: `probe-rs`-driven test firmware reporting cycle counts over RTT; CI runs it on a self-hosted runner with the board attached and fails on regression above 10%.
- `benches/report.py`: collects results into one table and the README chart; every release publishes the table with the commit hash and the board revision.

## 11. Open questions and roadmap

Nineteen decisions are settled and the spec is implementable for `core` and `decode`; the items under Open questions are the only things still to choose.

**Decision log**

| # | Decision | Chosen |
| --- | --- | --- |
| 1 | Argument encoding | Fixed width; varint only for site/ctx/ts metadata; compression in the drain |
| 2 | Site IDs | 32-bit content hash; `elf16` opt-in |
| 3 | Strings | Binary first, interned statics, capped dynamic bytes, `std`-only Display |
| 4 | Buffers | Per-context SPSC rings, static on `no_std`, heap on `std` |
| 5 | Contract emission | Link-section descriptors, extracted and stripped post-build |
| 6 | `Log` trait | Const `Schema`, hash-referenced, derive emits descriptor |
| 7 | Format grammar | Strict `fmt` subset plus `{:str}` `{:hex}` `{:bin}` |
| 8 | Spans | Two control frames, 64-bit IDs, explicit parent |
| 9 | Multi-core / async | Ring per thread or core; async tasks tagged by span ID |
| 10 | Versioning | Separate `wire_version` and `contract_schema` |
| 11 | Security | AEAD per block: authentication and encryption, both optional, drain-side |
| 12 | Tests | Conformance vectors, Criterion on `std`, Cortex-M cycle test in CI |
| 13 | Debug stdout | `std` only; MCU uses the host tool |
| 14 | Naming | `rtlog` everywhere: crates `rtlog-*`, macros `rtlog::`, CLI `rtlog`, files `.rtlog`, env `RTLOG_*` |
| 15 | Licence | MIT OR Apache-2.0, at the user's option |
| 16 | Log hot path | Encode in place in the ring on hosts; stack buffer + one shared push on bare metal (code size) |
| 17 | Crash log | Read-only dump of every ring's unread tail + `crash` frame, from a panic hook and async-signal-safe handlers; `ctx_time` frame aligns times |
| 18 | Debug stdout | `rtlog_decode::stdout`: decoder in the drain thread, level from `RTLOG` |
| 19 | C / C++ | Header-only producer; site ID = descriptor index; contract read from the ELF |

**Tests and benchmarks**

- `tests/vectors/*.rtlog` with expected JSON lines; every decoder implementation must pass them, and every wire or contract-schema change adds vectors.
- Criterion benches on `std` for the log call, the ring round trip and the drain; a cycle-count test on a Cortex-M4 board in CI through `probe-rs`, failing above 150 cycles per W1 call (a placeholder until the first hardware measurement). x86-64 and aarch64 target ≤ 50 cycles per W1 call. Results published on each release.
- Fuzzing of the decoder against random and truncated streams (`cargo fuzz`), since field captures are untrusted input.

**Status**

- Ready to implement: `core` (rings, macros, descriptors), `cargo rtlog contract`, `decode` with text and JSON output, RTT and file transports, crash log.
- Needs the open questions below first: `DropOldest` layout, field contract delivery, key provisioning interface.

**Open questions**

- [ ] `DropOldest` needs a per-frame length byte in the ring; pay it everywhere or make the ring layout depend on the policy?
- [ ] Contract delivery in the field: hash only, or a compressed contract in a flash region for self-describing devices?
- [ ] Key provisioning interface for the AEAD key: trait with a single `key(id) -> &[u8; 32]`, or integrate with an existing secure-element API?
- [x] Licence for the open-source release: MIT OR Apache-2.0, dual (decision 15).

**Roadmap**

1. v0.1: `rtlog-core` (`no_std` rings, macro, contract writer), `rtlog-decode` text/JSON, RTT and file transports, Cortex-M and Linux examples.
2. v0.2: `tracing` and `log` backends, OTLP and Perfetto outputs, `cargo rtlog` contract checker.
3. v0.3: `elf-ids` feature, CTF output, DMA UART transport, drain compression on `std`.
4. v1.0: wire format frozen, contract schema versioned, bench suite published (targets in cycles per W1 call: ≤ 50 on x86-64 and aarch64, Cortex-M4 budget set from the first hardware measurement; v0.1 placeholder ≤ 150).

**Sources**

- [defmt book](https://defmt.ferrous-systems.com/)
- [NanoLog repository](https://github.com/platformlab/nanolog) and [NanoLog paper](https://purl.stanford.edu/pd423tg1235)
- [Postform crate](https://lib.rs/crates/postform_persist)
- [delog crate](https://docs.rs/delog)
