# Performance and size

rtlog's budget is stated in **CPU cycles**. A log call's cost is a cycle count, and its time is cycles ÷ clock: 50 cycles is 15 ns at 3.3 GHz but 500 ns on a 100 MHz MCU. Every report here gives both, and names the clock.

## Cost of a log call

The [`timing`](../examples/timing.rs) example measures each workload in 2 000 batches of 256 calls, with the ring drained between batches. It reports the per-call mean of each batch. It also measures the core clock itself, by timing a chain of dependent adds (one per cycle), so the cycle column doesn't rely on what the OS reports.

CI runs it on an x86-64 runner and on an arm64 runner, and posts the table on the job's summary page: [`timing report (x86_64)` / `timing report (aarch64)`](https://github.com/NieWil/rtlog/actions/workflows/ci.yml).

<!-- timing-table -->
Measured by CI on GitHub-hosted runners (release build). Each CI run republishes these numbers.

| Workload | x86-64: AMD EPYC 9V74, 3.60 GHz measured | aarch64: 3.39 GHz measured |
| --- | ---: | ---: |
| Timestamp read (TSC / `CNTVCT_EL0`) | 9.7 ns · 35 cycles | 13.4 ns · 46 cycles |
| W1: `info!("x={} y={}", u32, u32)` | **14.9 ns · 53 cycles** | **18.6 ns · 63 cycles** |
| W2: 6 mixed fields | 14.9 ns · 54 cycles | 19.0 ns · 65 cycles |
| W3: 32-byte string | 14.3 ns · 51 cycles | 18.5 ns · 63 cycles |
| Disabled call | 0.4 ns · 1 cycle | 0.5 ns · 2 cycles |
| W5: 8 threads at once, per thread | 44.6 ns | 31.7 ns |
| Baseline: timestamped `writeln!` into a buffer | 73.2 ns · 263 cycles | 87.0 ns · 295 cycles |
| Ring + drain throughput, one thread | 36.7 M frames/s | 34.0 M frames/s |
<!-- /timing-table -->

A log call costs about 5× less than formatting the same line. The spec's budget is ≤ 50 cycles per W1 call: v0.1 is 3 cycles over it on x86-64 and 13 over on aarch64. Two-thirds of the cost is the timestamp read itself; the rest is the thread-local lookup and a copy through a stack buffer, which are the next optimisation targets. Shared VMs vary from run to run; on the development VM, W1 measured 79–127 cycles.

## Size on a microcontroller

[`size/mcu`](../size/mcu) is a minimal Cortex-M4F firmware built in four variants: no logging, rtlog with 1 and 10 call sites, and the same 10 messages formatted with `core::fmt`. [`scripts/size_report.py`](../scripts/size_report.py) builds them and reads the section sizes. CI posts the table on the `size (Cortex-M4F)` job page.

| Variant (`thumbv7em-none-eabihf`, `opt-level = "s"`, LTO) | Code | Read-only data | Site descriptors | Flash total | RAM |
| --- | ---: | ---: | ---: | ---: | ---: |
| No logging | 180 B | 0 B | 0 B | 1 204 B | 0 B |
| rtlog, 1 call site | 2 296 B | 52 B | 44 B | 3 416 B | 1 064 B |
| rtlog, 10 call sites | 2 672 B | 196 B | 404 B | 4 296 B | 1 064 B |
| `core::fmt`, same 10 messages | 14 932 B | 2 072 B | 0 B | 18 028 B | 0 B |

- Fixed runtime (ring, drain, encoder): about **2.1 KB** of code.
- Each additional call site: about **42 B** of code, plus a 40 B descriptor that only the host reads.
- RAM: the ring you choose (1 KiB here) plus 40 B of state.
- 10 messages cost **3.1 KB** of flash with rtlog against **16.8 KB** with `core::fmt`: 5.4× smaller.

## Measure on your platform

### Linux, macOS, Windows (native)

```sh
cargo run --release -p rtlog-examples --example timing -- --out timing.md
cargo bench -p rtlog-core --bench log_call          # Criterion, per workload
RUSTFLAGS="-C target-cpu=native" cargo run --release -p rtlog-examples --example timing
```

### Prebuilt demo

The quickest way on a board: download `rtlog-demo-<version>-<target>` from the [releases page](https://github.com/NieWil/rtlog/releases), copy it over and run `./run.sh`. It runs this timing report and a traced app; see [Demo on your device](demo.md).

### Cross-compiling for an ARM, RISC-V or x86 board

Install the target and a linker, build, copy the binary over and run it there. Timing under QEMU is not meaningful, so run it on the real board.

| Board | Target | Linker / toolchain |
| --- | --- | --- |
| Raspberry Pi 4/5, Jetson, Graviton (arm64) | `aarch64-unknown-linux-gnu` | `gcc-aarch64-linux-gnu` |
| Raspberry Pi 2/3 32-bit, i.MX6 (ARMv7) | `armv7-unknown-linux-gnueabihf` | `gcc-arm-linux-gnueabihf` |
| Raspberry Pi Zero/1 (ARMv6) | `arm-unknown-linux-gnueabihf` | `gcc-arm-linux-gnueabihf` |
| VisionFive 2, Milk-V (RISC-V 64) | `riscv64gc-unknown-linux-gnu` | `gcc-riscv64-linux-gnu` |
| 32-bit x86 | `i686-unknown-linux-gnu` | `gcc-multilib` |

```sh
rustup target add aarch64-unknown-linux-gnu
sudo apt install gcc-aarch64-linux-gnu
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
  cargo build --release -p rtlog-examples --example timing --target aarch64-unknown-linux-gnu
scp target/aarch64-unknown-linux-gnu/release/examples/timing pi@board:
ssh pi@board ./timing
```

[`cross`](https://github.com/cross-rs/cross) does the same with a container: `cross build --release -p rtlog-examples --example timing --target armv7-unknown-linux-gnueabihf`. The core clock is measured on x86-64, aarch64 and riscv64. On other architectures the report gives nanoseconds only.

### Microcontrollers

The core builds for `thumbv7m-none-eabi` (Cortex-M3), `thumbv7em-none-eabihf` (M4F/M7), `thumbv8m.main-none-eabihf` (M33) and `riscv32imac-unknown-none-elf`; CI checks all four. To measure cycles on a board, read the DWT cycle counter around a batch of calls:

```rust
let start = unsafe { core::ptr::read_volatile(0xE000_1004 as *const u32) }; // DWT_CYCCNT
for i in 0..256u32 { rtlog::info!(to: &mut p, "x={} y={}", i, 7u32); }
let cycles = unsafe { core::ptr::read_volatile(0xE000_1004 as *const u32) }.wrapping_sub(start) / 256;
```

An automated on-board cycle test (probe-rs in CI) is on the roadmap. Until it lands, the Cortex-M budget in the spec is a placeholder.

## Release binaries

The [release workflow](../.github/workflows/release.yml) builds the `rtlog` CLI with the `dist` profile (fat LTO, one codegen unit, stripped) for these hosts:

- x86-64, i686, arm64, ARMv7, ARMv6 (static musl);
- riscv64 (glibc);
- macOS on Intel and Apple silicon;
- Windows.

Each host also gets a demo package (`rtlog-demo-<version>-<target>`): the timing, Perfetto quickstart, traced-app and stdout examples with the same profile, a `run.sh` and a [guide](demo.md). It also packages the C/C++ header with the Zephyr module. See [Releases](https://github.com/NieWil/rtlog/releases), and [Releasing](releasing.md) for how a release is cut.
