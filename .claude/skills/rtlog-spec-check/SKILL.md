---
name: rtlog-spec-check
description: Check a change to rtlog against the rules of docs/spec.md (real-time log path, performance in cycles, memory on Cortex-M, wire-format compatibility across Rust, C and the decoder, crash safety, and usability of exports and the stdout mode). Use before opening or updating a PR that touches crates/, c/, the wire format, the log macros or the docs, and when asked whether a change "respects the spec".
---

# rtlog spec check

rtlog promises that a log call does not disturb the program it observes. These rules come from [docs/spec.md](../../../docs/spec.md); this skill makes every change prove it keeps them.

## 1. Run the checker

```sh
python3 scripts/spec_check.py
```

It reads its budgets from `docs/spec.md`, then checks:

| Check | Rule |
| --- | --- |
| `realtime` | The log hot path (`ring.rs` `emit`/`push_raw`, `arg.rs` encoders, `enabled`, `clock::now`, `__local_emit`) has no locks, allocation, formatting, I/O, panicking calls or sleeps |
| `wire` | Control-frame IDs and argument tags are identical in `wire.rs`, `arg.rs`, `c/include/rtlog.h` and handled in the decoder |
| `usability` | Every output mode is reachable from the CLI and documented; relative doc links resolve |
| `spec` | The spec still states the budgets the checker enforces |

For a change that can affect speed or size, also measure and pass the reports in:

```sh
cargo run --release -p rtlog-examples --example timing -- --out timing.md
python3 scripts/size_report.py --out size.md       # needs thumbv7em-none-eabihf + llvm-tools-preview
python3 scripts/spec_check.py --timing timing.md --size size.md
```

`performance` passes when the best W1 p50 is within the spec's cycle budget. `memory` passes when the fixed runtime and per-call-site code on Cortex-M4F are within the spec's size budgets. CI runs all of this in the `spec rules` job. On a shared VM, run timing twice and pass both reports; the best one counts.

A broken rule is fixed in the code, never by loosening the checker. If a budget itself must change, change it in `docs/spec.md` with a decision-log entry explaining why, in the same PR.

## 2. Review what the script cannot see

Go through each list for the parts the change touches.

**Real-time (spec §0, §7)**

- Nothing a log call reaches may block, allocate, take a lock, make a syscall or panic, including code called through new helper functions. Add new hot-path functions to `HOT_PATH` in `scripts/spec_check.py`.
- A full ring drops and counts; it never waits.
- Arguments of a disabled site are not evaluated.

**Performance (spec §10c)**

- Budgets are in cycles, not nanoseconds. Quote ns only with the clock.
- Prefer constant-size copies (`put_array`) over slice copies on the log path; check instruction counts with valgrind (`--tool=callgrind`) when in doubt.

**Memory (spec §0, §7)**

- No growth at runtime: rings are sized at creation. On `no_std`, nothing may need `alloc`.
- `cargo build -p rtlog-core --no-default-features --target thumbv7em-none-eabihf` must still build.

**Wire compatibility (spec §5)**

- A new frame or tag goes into `wire.rs`, `c/include/rtlog.h`, the decoder (`stream.rs`), the spec's frame tables, and a test in `crates/rtlog-decode/tests/malformed.rs`.
- Never reuse an ID. Old captures must keep decoding.

**Crash safety (spec §8b)**

- Code reachable from `crash.rs` signal handlers or `rtlog__on_signal` is async-signal-safe: no allocation, no blocking lock (`try_lock` only), `open`/`write` only.
- `cargo test -p rtlog-decode --test crash` and `scripts/c_e2e.sh` must pass; they crash real processes.

**Usability**

- A developer must be able to read logs without tools: `rtlog_decode::stdout::init()` (text on stdout, level from `RTLOG`).
- Every capture must export to text, JSON lines and Perfetto (`rtlog decode --perfetto`).
- Update `README.md` or the guide in `docs/` that covers the change, and `CHANGELOG.md`.

## 3. Report

Give the checker's output, any measured budgets with their margin, and for each review area either "no impact" or what was verified. If the change deviates from the spec on purpose, record it in `docs/implementation.md` and say so.
