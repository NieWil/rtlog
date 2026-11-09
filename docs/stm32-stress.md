# STM32L432KC stress comparison

## Scope and method

Measured on the physical NUCLEO-L432KC at 80 MHz on 2026-11-04, using the same
Zephyr 4.2.0, SDK 0.17.2, GCC 12.2.0, `-O2`, and 2048-byte buffers as the
[initial comparison](stm32-comparison.md). Both variants compile the same
[stress workload](../c/zephyr/samples/benchmark/src/stress.c). Neither has a
background drain thread or a timed UART backend. rtlog has one producer ring;
Zephyr has one shared buffer, overwrite disabled, and thread blocking disabled.

Each logger ran six cases, three times after independent resets: **294,912
total attempted records** excluding warmup and recovery probes. All runs
printed `STRESS_DONE`, and all three reports per logger were identical.
The [CSV](measurements/stm32-l432kc-stress.csv) preserves the observations.

Each case has 16 batches of 512 calls. Interrupts remain enabled while timing
calls with DWT. Ordinary system timer interrupts can therefore affect even
the cases without the additional interrupt load. Drained integer cases drain
every 16 calls; 127-byte dynamic-string cases drain every four. The mutable
string contains 127 nonzero bytes followed by a terminator. Sorting and
report printing are outside call timing.

Overload cases never drain until all 8192 calls have been attempted. The
paused-consumer case also busy-waits 13 us between calls, so recording
continues without draining for at least 106 ms, plus call and sorting time.
It simulates a consumer not making progress, **not an unplugged USB cable or
a backend callback stuck inside a write**.

Additional interrupt load is a Zephyr timer callback scheduled every 100 us
at 10,000 kernel ticks/s. It calls `k_busy_wait(20)` and increments an atomic
counter. The nominal busy-work duty cycle is 20%; actual interrupt cost also
includes timer, kernel, entry/exit, and callback overhead. The handler does
not log. Interrupt counts differ because slower cases run for longer; this
is equal configured load, not an equal number of preemptions.

`max_batch_median` and `max_batch_p95` are the largest batch median and batch
p95, respectively, **not pooled percentiles**. `max_cycles` is the largest
single observed call interval across all 8192 calls, including preemption.
No timing overhead has been subtracted. These are observed stress results,
**not a proven worst-case execution-time bound**.

## Call latency

All values below are core cycles. Reports were identical across resets.

| Case | rtlog max batch median | Zephyr max batch median | rtlog max batch p95 | Zephyr max batch p95 | rtlog observed max | Zephyr observed max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Integer, drained | 448 | 644 | 656 | 705 | 2,123 | 2,178 |
| Dynamic string, 127 bytes | 2,912 | 3,733 | 2,959 | 3,864 | 4,504 | 5,398 |
| No-drain burst | 265 | 214 | 448 | 648 | 1,861 | 1,814 |
| Consumer paused, over 100 ms | 265 | 214 | 613 | 694 | 1,810 | 705 |
| Integer, interrupt load | 448 | 644 | 3,566 | 3,727 | 4,707 | 4,910 |
| Dynamic string, interrupt load | 5,982 | 6,803 | 6,033 | 6,931 | 7,220 | 12,178 |

At 80 MHz, interrupt-loaded integer maxima were **58.84 us for rtlog** and
**61.38 us for Zephyr**. Dynamic-string maxima were **90.25 us** versus
**152.23 us**. Those maxima are not guaranteed bounds; interrupt alignment,
firmware layout, and the rest of the application can change them.

rtlog was faster in the drained producer cases. **Zephyr's saturated drop
path was faster**: 214 versus 265 cycles for the largest batch median.
Most overload calls were drops, so their low median must not be interpreted
as successful-record throughput. The stalled-case maximum difference also
reflects different alignment with system interrupts, not a universal bound.

## Overload and recovery

| Case, per run | rtlog accepted | rtlog dropped | Zephyr accepted | Zephyr dropped |
| --- | ---: | ---: | ---: | ---: |
| No-drain burst, 8192 attempts | 146 | 8,046 | 64 | 8,128 |
| Paused consumer, 8192 attempts | 146 | 8,046 | 64 | 8,128 |
| Every drained case, 8192 attempts | 8,192 | 0 | 8,192 | 0 |

rtlog retained 2.28 times as many one-integer records in the same nominal
buffer capacity in these cases. Record layouts and timestamps differ.
Both loggers continued to completion under overload, and every case satisfied
`accepted + dropped == attempted`. After draining, a new one-integer record
was successfully delivered without another drop after **every** case.
This verifies counts and forward progress, not payload integrity, wire decode,
or Perfetto timeline correctness.

## Drain costs and memory

Summed drain cycles, for all 8192 accepted records with no extra interrupt
load and no formatting or I/O:

| Payload | rtlog drain cycles | Zephyr drain cycles |
| --- | ---: | ---: |
| One uint32_t | 1,584,842 | 4,324,872 |
| Dynamic 127-byte string | 8,068,723 | 4,536,193 |

**rtlog's dynamic-string drain was slower**, despite its faster producer.
The backends and stored representations differ; this is discard-drain cost,
not equal-format transport throughput. Under the added timer load, summed
drain timing also includes preemption (see the CSV).

| Stress firmware | Flash bytes | Static RAM bytes |
| --- | ---: | ---: |
| rtlog | 21,720 | 9,856 |
| Zephyr deferred | 29,864 | 9,984 |

These totals include the timer, 2048-byte sample array, mutable payload,
sorting, console, kernel, and stacks. Use the matched ten-site footprint
table in the initial comparison to assess logger memory overhead. No peak
stack use or dynamic high-water mark was measured.

## Reproduce

With the tools from [Zephyr](zephyr.md#build-and-flash) installed and USB
access set up as in the [comparison report](stm32-comparison.md#wsl-access-and-capture).
Change `RTLOG_DIR` to your rtlog checkout's absolute path:

```sh
. ~/.venvs/zephyr/bin/activate
cd ~/zephyrproject
export RTLOG_DIR=/absolute/path/to/rtlog
export APP="$RTLOG_DIR/c/zephyr/samples/benchmark"
west build -d build/stress-rtlog -b nucleo_l432kc "$APP" -- \
  -DZEPHYR_EXTRA_MODULES="$RTLOG_DIR" -DRTLOG_BENCH_STRESS=ON
west build -d build/stress-zephyr -b nucleo_l432kc "$APP" -- \
  -DZEPHYR_EXTRA_MODULES="$RTLOG_DIR" '-DCONF_FILE=common.conf;zephyr.conf' \
  -DRTLOG_BENCH_STRESS=ON
west flash -d build/stress-rtlog --runner openocd
stty -F /dev/ttyACM0 115200 raw -echo -ixon -ixoff -crtscts
cat /dev/ttyACM0
```

Press reset after opening capture. Wait for `STRESS_DONE`, then stop capture
and repeat for `build/stress-zephyr`. `STRESS_ERROR` indicates a failed
accounting, recovery, load, or cycle-counter check.

## Production assessment

This is positive evidence for bounded application diagnostics with an
explicitly lossy buffer. It is not production certification or validation
of OS tracing. Missing tests include multi-producer contention, nested ISR
logging, scheduler hooks, slot exhaustion, long-duration soak, real transport
disconnects and backpressure, peak stacks, and binary/Perfetto correctness
under loss. No claims are made about these untested conditions.

In particular, draining fast enough prevented all loss in the drained cases,
but both loggers lost over 98% of records when draining stopped. A production
system must size buffers and transport bandwidth for its workload, expose
drops, and avoid treating missing trace events as idle time.
