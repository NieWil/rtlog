# Changelog

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
- See [implementation notes](docs/implementation.md).

Licensed under MIT OR Apache-2.0.
