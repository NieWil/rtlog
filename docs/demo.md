# rtlog demo: measure the logger on your device

Each release ships a ready-to-run demo for every supported architecture. Copy it to a board or a server, run one script, and bring back:

- the cost of a log call on that CPU, in nanoseconds and cycles, next to `write!`/`writeln!` formatting;
- a Perfetto trace of a small multi-threaded app, to open on your computer;
- the same capture decoded as text with the `rtlog` CLI, the way you would do it from the host.

Nothing to install on the device: the Linux binaries are static (musl), except RISC-V 64 (glibc).

## 1. Pick the archive

From the [releases page](https://github.com/NieWil/rtlog/releases), download `rtlog-demo-<version>-<target>.tar.gz` (`.zip` on Windows):

| Device | Target |
| --- | --- |
| PC, server, VM (64-bit) | `x86_64-unknown-linux-musl` |
| Old PC, 32-bit x86 board | `i686-unknown-linux-musl` |
| Raspberry Pi 3/4/5 (64-bit OS), Jetson, Graviton, Ampere | `aarch64-unknown-linux-musl` |
| Raspberry Pi 2/3 (32-bit OS), i.MX6, BeagleBone, STM32MP1 | `armv7-unknown-linux-musleabihf` |
| Raspberry Pi Zero / 1 (ARMv6) | `arm-unknown-linux-musleabihf` |
| VisionFive 2, Milk-V, other RISC-V 64 boards | `riscv64gc-unknown-linux-gnu` |
| Mac (Intel / Apple silicon) | `x86_64-apple-darwin` / `aarch64-apple-darwin` |
| Windows | `x86_64-pc-windows-msvc` |

Not sure? Run `uname -m` on the device: `x86_64`, `i686`, `aarch64`, `armv7l`, `armv6l` or `riscv64`.

## 2. Copy it to the device and run it

```sh
# on your computer
scp rtlog-demo-*-aarch64-unknown-linux-musl.tar.gz pi@raspberrypi.local:
ssh pi@raspberrypi.local

# on the device
tar xzf rtlog-demo-*.tar.gz
cd rtlog-demo-*/
RTLOG_BOARD="Raspberry Pi 5" ./run.sh   # results in ./results-<hostname>/
```

It takes a few seconds. The script prints the timing table, for example on x86-64:

```text
| Workload | Call | min ns | p50 ns | p99 ns | max ns | p50 cycles |
| Clock read | `clock::now()` (Counter source) | 11.8 | 14.9 | 26.1 | 103.7 | 49 |
| W1 scalar | `info!("x={} y={}", u32, u32)` | 13.7 | 13.8 | 39.7 | 321.2 | 45 |
| Baseline | `writeln!` timestamped W1 line into a buffer | 55.1 | 57.0 | 101.0 | 173.5 | 188 |
Spec budget (section 10c): ≤ 50 cycles per W1 call on x86-64 / aarch64. This run: 45 cycles (13.8 ns at 3.30 GHz), met.
```

On Windows, run the binaries from PowerShell in the unpacked folder:

```powershell
mkdir results
bin\timing.exe --out results\timing.md
bin\perfetto_quickstart.exe results\quickstart.perfetto.json
bin\app_trace.exe results\app
```

## 3. Bring the results back and look at them

```sh
# on your computer
scp -r pi@raspberrypi.local:rtlog-demo-*/results-raspberrypi .
```

| File | What it is |
| --- | --- |
| `timing.md` | Board, CPU, OS, rtlog and Rust versions, then the cost per call: clock read, W1 (two `u32` fields) to W3 payloads, disabled level, formatting baselines, 8 contending threads, throughput. [Share it](#share-your-results). |
| `quickstart.perfetto.json` | Threads and spans. Open [ui.perfetto.dev](https://ui.perfetto.dev) → *Open trace file*. |
| `app/app.perfetto.json` | The traced app: 1 kHz control loop with deadline misses, HTTP workers, a lock-order deadlock caught by a watchdog, heap counters. |
| `app/app.rtlog`, `app/app.contract.jsonl` | The raw binary capture and its contract: what a real device sends to the host. |
| `app/app.txt`, `app/app.jsonl` | The same capture decoded as text and JSON lines. |
| `stdout.txt` | The stdout mode: text printed while the program runs, like `env_logger`. |
| `system.txt` | CPU, kernel, core count. |

### Share your results

Other users compare boards in [results/](../results/README.md). Name the board when you run the demo (`RTLOG_BOARD="..."`; without it the report uses what the system reports, such as the device-tree model on a Raspberry Pi), then either:

- open an issue with the [board results form](https://github.com/NieWil/rtlog/issues/new?template=board-results.yml) and paste `timing.md`, or
- open a pull request that adds it unedited as `results/<rtlog version>/<board>.md` (lowercase, dashes: `results/0.2.4/raspberry-pi-5.md`) and runs `python3 scripts/results.py` to refresh the table. CI checks the file.

Results are kept per rtlog version, so a new version gets a new file and old numbers stay comparable.

## 4. Device writes, host decodes

That is how rtlog is meant to be used: the device only writes binary frames, and the computer turns them into text or a trace. Try it with the capture you brought back, using the CLI for your computer (the `rtlog-<version>-<target>` archive on the releases page, or the `bin/rtlog` in a demo archive for your computer):

```sh
rtlog decode --contract app/app.contract.jsonl app/app.rtlog | less -R
rtlog decode --contract app/app.contract.jsonl --perfetto -o app.json app/app.rtlog
rtlog decode --contract app/app.contract.jsonl --json app/app.rtlog | jq 'select(.level == "warn")'
```

The format strings are only in the contract, never on the wire: 18 bytes per W1 record against 33 bytes of text.

## What's in the archive

```text
rtlog-demo-<version>-<target>/
├── README.md              this guide
├── run.sh                 runs everything below and collects the results
└── bin/
    ├── timing             cost per log call (examples/timing.rs)
    ├── perfetto_quickstart  threads + spans to a Perfetto trace
    ├── app_trace          a realistic traced app
    ├── stdout_mode        text on stdout while running
    └── rtlog              the decoder CLI
```

The binaries are the repository's [examples](../examples) built with the `dist` profile (fat LTO, one codegen unit, stripped), the same optimisation as the CLI.

## Getting good numbers

- **Run on the real device.** Under QEMU or `binfmt` emulation, cycle counts mean nothing.
- **Let it settle.** Close other heavy programs; on a laptop, plug it in. On a board, `echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor` stops frequency scaling.
- **Cycles compare, nanoseconds depend on the clock.** The report measures the core clock itself, so a 1.5 GHz Pi and a 3.5 GHz server can be compared in cycles. Cycles are measured on x86-64, aarch64 and riscv64; on 32-bit Arm and x86 the report gives nanoseconds only.
- **`Permission denied` running `run.sh`:** the file system is mounted `noexec` (often `/tmp`). Unpack in your home directory.
- **Another board or OS:** build the examples yourself, see [Performance → Measure on your platform](performance.md#measure-on-your-platform).
