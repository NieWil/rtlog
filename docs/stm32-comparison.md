# STM32L432KC: rtlog versus Zephyr deferred logging

See the [hardware stress comparison](stm32-stress.md) for overload, paused
consumer, 127-byte dynamic payload, interrupt interference, and recovery tests.

## Status

Built from Debian 13 under WSL2 on 2026-11-04 using Zephyr **v4.2.0**
(`413b789deb391d3a37d06b463288a5fe765ee57e`), Zephyr SDK **0.17.2**, and
ARM GCC **12.2.0**, with `-O2` and GNU C11 for `nucleo_l432kc/stm32l432xx`.
All five images below compiled and linked for the real board target.

**Both timing images were flashed and measured on the board.** After fixing
WSL device permissions, the SDK's OpenOCD 0.12.0 connected through ST-LINK/V2-1
and programmed both images. The MCU identified as STM32L43/L44xx revision Z,
with 256 KiB flash. Both firmware reports confirmed an **80 MHz** core clock.
Three reset runs per logger produced identical CSV statistics and completed
with `RTLOG_BENCH_DONE`; no debugger halted the timed workload. Reports were
captured through the ST-LINK serial bridge at 115200 baud. The rtlog benchmark
was restored to the board after comparison.

## Measured producer performance

Raw cycles include the timing wrapper and counter-read overhead; neither the
7-cycle baseline nor any other overhead has been subtracted. Each case has
256 observations per reset run. Microseconds are median cycles divided by 80.
The [cycle results](measurements/stm32-l432kc-cycles.csv) preserve all three
runs per logger. Reset repeatability is not evidence of worst-case latency
under application load.

| Case | rtlog median cycles | Zephyr median cycles | rtlog median us | Zephyr median us | Zephyr / rtlog |
| --- | ---: | ---: | ---: | ---: | ---: |
| Baseline | 7 | 7 | 0.088 | 0.088 | 1.00 |
| Runtime-disabled debug | 13 | 13 | 0.163 | 0.163 | 1.00 |
| No arguments | 398 | 628 | 4.975 | 7.850 | 1.58 |
| One uint32_t | 447 | 649 | 5.588 | 8.113 | 1.45 |
| Three uint32_t | 520 | 1,171 | 6.500 | 14.638 | 2.25 |
| Seven-byte static string | 476 | 1,888 | 5.950 | 23.600 | 3.97 |

| Case | rtlog p95 cycles | Zephyr p95 cycles | rtlog max cycles | Zephyr max cycles |
| --- | ---: | ---: | ---: | ---: |
| No arguments | 560 | 684 | 615 | 775 |
| One uint32_t | 649 | 697 | 662 | 796 |
| Three uint32_t | 743 | 1,191 | 750 | 1,284 |
| Seven-byte static string | 680 | 1,899 | 703 | 2,031 |

Every accepted-log case had zero drops. Zephyr additionally checked that each
enabled case delivered exactly 256 records to its discard backend. rtlog's
span begin/end pair measured 1,164 median cycles (14.550 us), p95 1,251, max
1,269. Its full-ring one-integer call measured 252 median cycles (3.150 us),
p95 252, max 254, and exactly 256 timed drops. There is no matching Zephyr
span or full-ring row in this comparison.

**Interpretation:** rtlog is faster in all four enabled-log cases measured,
with the smallest median advantage for the one-integer call. Its one-integer
p95 advantage is smaller than its median advantage (649 versus 697 cycles),
so a median-only summary would overstate the improvement for tail latency.
Both runtime-disabled sites cost the same. The measured rtlog C/Zephyr path
costs hundreds of cycles on this MCU; it does not demonstrate a 50-cycle call
or reproduce the existing host/Rust cycle benchmarks. These results identify
room for optimization, not a reason to change a spec budget.

## Matched footprint results

The [footprint workload](../c/zephyr/samples/benchmark/src/footprint.c) has ten
distinct call sites with one volatile `uint32_t` argument each. Both loggers
use a 2048-byte buffer, no processing thread, and a discard sink drained from
main. rtlog has one thread slot. All variants have the same main stack,
console, optimization, and TLS setting; the baseline only reads the input.

These are **linker region totals in bytes**, including the kernel, drivers,
reserved stacks, and application, not standalone library sizes:

| Matched image | Flash | Static RAM | Flash above baseline | RAM above baseline |
| --- | ---: | ---: | ---: | ---: |
| No logger | 16,468 | 5,440 | 0 | 0 |
| rtlog | 19,456 | 7,616 | 2,988 | 2,176 |
| Zephyr deferred logger | 27,936 | 7,744 | 11,468 | 2,304 |

For this workload, rtlog uses **8,480 fewer flash bytes** (30.4% of the Zephyr
image) and **128 fewer RAM bytes** (1.7%). Above baseline, the flash additions
are 2,988 versus 11,468 bytes. Do not generalize those percentages to other
applications, site counts, backends, or configurations.

The rtlog ring object is 2,128 bytes (`rtlog__rings` in the ELF); the configured
payload capacity is 2,048 bytes. RAM figures are static reservations, **not
measured stack high-water marks or peak runtime use**. Alignment and TLS mean
that `size`'s `data + bss` may differ slightly from the linker's region total.
No drain-thread stack is included because processing threads are disabled.

## Timing images

| Timing image | Flash bytes | Static RAM bytes |
| --- | ---: | ---: |
| rtlog | 21,348 | 8,640 |
| Zephyr deferred logger | 29,748 | 8,768 |

These contain sample arrays, sorting, and CSV reporting. Use the matched
footprint table above for size comparisons: rtlog's timing image also has
span and overflow cases absent from the Zephyr timing image.

Both timing programs use DWT core cycles, 256 observations per case, interrupt
masking around each call, and draining every 16 observations outside the
timed interval. The Zephyr comparison verifies accepted record counts and
zero drops. Its debug site is compiled in but runtime-filtered to INFO.
Zephyr uses a 64-bit log timestamp, runtime filtering, no overflow overwrite,
and no producer blocking. Neither backend formats or transmits timed records.

Compare corresponding no-argument, one-u32, three-u32, and string rows plus
their timing baselines. Do not infer speed from binary size. Zephyr can retain
a pointer to a static string while rtlog copies
its bytes, so the string case has different serialization semantics. The
loggers also have different timestamp and buffer implementations. These are
configured producer-cost measurements, not equal wire formats or a full
transport/formatting benchmark. Zephyr has one shared buffer; rtlog's memory
scales with the number of thread rings. Neither result predicts multi-thread
contention or end-to-end throughput.

## Reproduce the builds

Activate the installed environment and work in the external Zephyr workspace:

```sh
. ~/.venvs/rtlog-zephyr/bin/activate
cd ~/zephyrproject
export RTLOG_DIR=/home/niels/ui-mockup-task/rtlog
export APP="$RTLOG_DIR/c/zephyr/samples/benchmark"

west build -d build/rtlog-benchmark -b nucleo_l432kc "$APP" -- \
  -DZEPHYR_EXTRA_MODULES="$RTLOG_DIR"
west build -d build/zephyr-log-benchmark -b nucleo_l432kc "$APP" -- \
  -DZEPHYR_EXTRA_MODULES="$RTLOG_DIR" '-DCONF_FILE=common.conf;zephyr.conf'

west build -d build/footprint-baseline -b nucleo_l432kc "$APP" -- \
  -DZEPHYR_EXTRA_MODULES="$RTLOG_DIR" '-DCONF_FILE=common.conf;baseline.conf' \
  -DRTLOG_BENCH_FOOTPRINT=ON
west build -d build/footprint-rtlog -b nucleo_l432kc "$APP" -- \
  -DZEPHYR_EXTRA_MODULES="$RTLOG_DIR" -DRTLOG_BENCH_FOOTPRINT=ON
west build -d build/footprint-zephyr -b nucleo_l432kc "$APP" -- \
  -DZEPHYR_EXTRA_MODULES="$RTLOG_DIR" '-DCONF_FILE=common.conf;zephyr.conf' \
  -DRTLOG_BENCH_FOOTPRINT=ON
```

## WSL access and capture

The device is already attached to WSL, so no additional usbipd attach is
currently needed. Install the SDK's ST-LINK udev rules and configure serial
permissions for a persistent solution. On WSL, ensure udev is actually running;
otherwise a rule file alone will not change an existing device node.

For this attachment only, the user can run the following privileged commands
directly in their own terminal. The current user already belongs to `plugdev`:

```sh
sudo chgrp plugdev /dev/bus/usb/001/003 /dev/ttyACM0
sudo chmod g+rw /dev/bus/usb/001/003 /dev/ttyACM0
```

Confirm the USB node still identifies the ST-LINK before applying these
commands; USB bus/device numbers can change after reconnecting. These changes
do not survive device reattachment. Do not run builds as root.

Once access is fixed:

```sh
. ~/.venvs/rtlog-zephyr/bin/activate
cd ~/zephyrproject
west flash -d build/rtlog-benchmark --runner openocd
stty -F /dev/ttyACM0 115200 raw -echo -ixon -ixoff -crtscts
cat /dev/ttyACM0 | tee rtlog-cycles.csv
```

Press reset after capture starts, wait for `RTLOG_BENCH_DONE`, then stop
capture with Ctrl-C. Repeat with `build/zephyr-log-benchmark`, capturing to
`zephyr-cycles.csv`. Repeat each test after reset, without debugger halts.
The output header gives the actual core frequency. Timing comparisons and
nanosecond conversions must use those real captures, not host simulations.
