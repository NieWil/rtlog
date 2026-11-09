# Zephyr

rtlog ships as a Zephyr module: the C header, a glue file with the clock and a drain thread, a linker snippet that keeps call-site descriptors in ROM, and Kconfig options.

> Status: the module and [`samples/hello`](../c/zephyr/samples/hello) are not built in CI yet, because that needs the Zephyr SDK. The header they use is tested on Linux with GCC and Clang. Reports from boards are welcome.

## Add the module

Fetch rtlog in one of two ways.

**With west.** Add it to the `projects` list of your workspace's `west.yml`, pinned to a release tag, then run `west update`:

```yaml
manifest:
  projects:
    - name: rtlog
      url: https://github.com/NieWil/rtlog
      revision: v0.2.5
      path: modules/lib/rtlog
```

West then finds the module by itself. The sources are in `modules/lib/rtlog` inside the workspace.

**As a separate checkout.** Clone rtlog anywhere and pass its absolute path to every build:

```sh
export RTLOG_DIR=/absolute/path/to/rtlog
west build ... -- -DZEPHYR_EXTRA_MODULES="$RTLOG_DIR"
```

The commands below set `RTLOG_DIR` to wherever the rtlog sources are. With west, that is `~/zephyrproject/modules/lib/rtlog`, and you leave out `-DZEPHYR_EXTRA_MODULES`.

In `prj.conf`:

```ini
CONFIG_RTLOG=y
# the default transport writes the binary stream to the console UART
CONFIG_PRINTK=n
CONFIG_LOG=n
```

## Log

```c
#include "rtlog.h"

void sensor_thread(void) {
    rtlog_set_thread_name("sensor");
    for (;;) {
        rtlog_span_t s = RTLOG_SPAN_BEGIN("adc.read");
        RTLOG_DEBUG("adc ch={} raw={}", 3, read_adc());
        RTLOG_SPAN_END(s);
        k_msleep(5);
    }
}
```

The module's drain thread runs at a low priority every `CONFIG_RTLOG_DRAIN_PERIOD_MS` and writes to the console UART. To send the stream elsewhere (RTT, a second UART, USB CDC, a flash ring), define your own transport:

```c
int rtlog_zephyr_write(const uint8_t *bytes, size_t len) {
    SEGGER_RTT_Write(1, bytes, len);
    return 0;
}
```

## Decode

This builds the [hello sample](../c/zephyr/samples/hello), which is at `$RTLOG_DIR/c/zephyr/samples/hello`. Put the serial port in raw mode before the capture: a tty in its default mode translates CR/LF bytes and corrupts the binary frames. Use the baud rate your board's console runs at.

```sh
cd ~/zephyrproject
west build -b <board> "$RTLOG_DIR/c/zephyr/samples/hello" -- -DZEPHYR_EXTRA_MODULES="$RTLOG_DIR"
# if west fetched rtlog: west build -b <board> modules/lib/rtlog/c/zephyr/samples/hello
rtlog contract --elf build/zephyr/zephyr.elf -o app.contract.jsonl
stty -F /dev/ttyACM0 115200 raw -echo -ixon -ixoff -crtscts
cat /dev/ttyACM0 > app.rtlog          # or your RTT / probe tool's capture
rtlog decode --contract app.contract.jsonl app.rtlog
rtlog decode --contract app.contract.jsonl --perfetto -o app.json app.rtlog
```

## Options

| Kconfig | Default | Meaning |
| --- | --- | --- |
| `CONFIG_RTLOG_MAX_THREADS` | 8 | Threads that can log at once |
| `CONFIG_RTLOG_RING_SIZE` | 2048 | Bytes per thread ring (power of two) |
| `CONFIG_RTLOG_DRAIN_THREAD` | y | Start the drain thread |
| `CONFIG_RTLOG_DRAIN_PERIOD_MS` | 10 | Drain period |
| `CONFIG_RTLOG_DRAIN_PRIORITY` | 14 | Drain thread priority |

The module needs thread-local storage (`CONFIG_THREAD_LOCAL_STORAGE`), which it selects. The timestamp clock is the hardware cycle counter (`k_cycle_get_64`, or an extended 32-bit counter).

## Measure on a NUCLEO-L432KC

See the [STM32 comparison report](stm32-comparison.md) for verified Zephyr 4.2
builds, measured rtlog/Zephyr cycle costs at 80 MHz, matched flash and RAM
measurements, and WSL flash-access requirements.
The [stress report](stm32-stress.md) adds overload, paused-consumer, dynamic
payload, interrupt-interference, and recovery measurements on the board.

The [benchmark sample](../c/zephyr/samples/benchmark) measures the C logger on
the STM32L432KC's Cortex-M4 using its DWT core-cycle counter. Connect the board's
ST-LINK USB port: it provides flashing/debugging and a virtual serial port to
the MCU's console UART. This is not native USB CDC firmware on the STM32.

### Build and flash

First install the host dependencies from Zephyr's
[getting started guide](https://docs.zephyrproject.org/latest/develop/getting_started/index.html).
If you do not already have a Zephyr workspace and SDK, a typical setup is:

```sh
python3 -m venv ~/.venvs/zephyr
. ~/.venvs/zephyr/bin/activate
pip install west
west init ~/zephyrproject
cd ~/zephyrproject
west update
west zephyr-export
pip install -r zephyr/scripts/requirements.txt
cd zephyr
west sdk install --toolchains arm-zephyr-eabi
```

With that environment active, build from your Zephyr workspace. The commands
below use a separate rtlog checkout, added as an extra module; change
`RTLOG_DIR` to its absolute path. If west fetched rtlog (see
[Add the module](#add-the-module)), set `RTLOG_DIR` to
`~/zephyrproject/modules/lib/rtlog` and leave out `-DZEPHYR_EXTRA_MODULES`:

```sh
export RTLOG_DIR=/absolute/path/to/rtlog
cd ~/zephyrproject
west build -d build/rtlog-benchmark -b nucleo_l432kc \
  "$RTLOG_DIR/c/zephyr/samples/benchmark" -- \
  -DZEPHYR_EXTRA_MODULES="$RTLOG_DIR"
west flash -d build/rtlog-benchmark --runner openocd
```

The board target is `nucleo_l432kc`, not the MCU part number. See the
[Zephyr board guide](https://docs.zephyrproject.org/latest/boards/st/nucleo_l432kc/doc/index.html)
for the console pins, flash runners, and board-specific requirements. The command
above requires OpenOCD (from the SDK host tools or your host installation);
omitting `--runner openocd` uses the board's default STM32CubeProgrammer runner,
which requires a separate STM32CubeProgrammer installation. Linux
needs the appropriate ST-LINK udev rules and serial-device permissions. If
flashing fails with a permission error, follow that guide rather than running
the build as root.

### Capture the report

Open the ST-LINK serial device at **115200 baud, 8N1, no flow control** before
pressing the board's reset button. On Linux it is usually `/dev/ttyACM0`; prefer
its stable name under `/dev/serial/by-id/` if multiple boards are attached.

```sh
stty -F /dev/ttyACM0 115200 raw -echo -ixon -ixoff -crtscts
cat /dev/ttyACM0 | tee rtlog-benchmark.csv
```

Press reset, wait for `RTLOG_BENCH_DONE`, then stop capture with Ctrl-C. There is
a two-second startup delay. Close other serial monitors before capturing.

The report starts with the actual `core_hz`, the logger's Zephyr
`timestamp_hz`, sample count, ring size, and number of thread slots, followed by:

```text
case,min_cycles,median_cycles,p95_cycles,max_cycles,drops
```

Rows cover the empty timing baseline, a runtime-disabled debug call, an enabled
call with no arguments, one and three `uint32_t` arguments, a seven-byte string,
a span begin/end pair, and a call when the ring is full. All values are raw
**core cycles**, including the counter-read and wrapper overhead; the baseline
shows that overhead rather than silently subtracting it. A span row measures
both calls together. To estimate nanoseconds use
`cycles * 1_000_000_000 / core_hz`, not the timestamp frequency. Do not assume
that the board runs at its maximum supported clock.

The sample uses speed optimization, one 2048-byte ring, 256 observations per
case, and no background drain thread. It warms each case and drains to a
discard sink every 16 observations, outside the timed interval. Interrupts are
masked only around each timed operation: these are uncontended producer-cost
measurements, not application latency with interrupts and scheduling. The
logger still uses the module's normal Zephyr timestamp callback; DWT is only
the measurement clock. A debugger must not halt or single-step during a run.
First-use ring registration is excluded.

Successful-write rows must have zero drops; the full-ring row must have exactly
256, excluding the untimed prefill. Otherwise the sample prints
`RTLOG_BENCH_ERROR` instead of the completion marker. The counter is checked
at startup; unsupported/non-running DWT counters also produce an error.

This sample prints **text only**: binary frames go to the discard sink. Do not
feed its CSV to `rtlog decode`. It measures the logger call path, not UART/USB
throughput, and its firmware footprint includes the reporting code and sample
array. For whole-image RAM/ROM breakdowns:

```sh
west build -d build/rtlog-benchmark -t ram_report
west build -d build/rtlog-benchmark -t rom_report
```

To test end-to-end logging, build the existing
[hello sample](../c/zephyr/samples/hello) with the same board and extra-module
argument in a separate build directory, then use the binary capture and decode
steps above. Keep text output off that binary UART. At 115200 baud, 8N1 carries
at most 11,520 payload bytes/s before framing costs; a sustained producer rate
above the transport capacity will drop records even when each call is fast.

Record the Zephyr revision, compiler version, board, reported clock, optimization
settings, and ring configuration with any results. Repeat after resetting the
board. Host timings and the spec's existing Rust benchmarks are not measured
STM32 C/Zephyr performance results.
