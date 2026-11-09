## rtlog MCU timing report

| Platform | |
| --- | --- |
| Board | ST NUCLEO-L432KC |
| CPU | Arm Cortex-M4F (STM32L432KC) |
| Core clock | 80 MHz |
| OS | Zephyr 4.2.0 |
| rtlog | 0.2.2 |
| API | C header (rtlog.h) on Zephyr |
| Toolchain | Zephyr SDK 0.17.2, arm-zephyr-eabi GCC 12.2.0, -O2, GNU C11 |
| Target | `nucleo_l432kc/stm32l432xx` |

Benchmark firmware [`c/zephyr/samples/benchmark`](https://github.com/NieWil/rtlog/tree/v0.2.2/c/zephyr/samples/benchmark): core cycle counter (DWT CYCCNT), 256 calls per case and run, 3 reset run(s), interrupts masked around each call, ring drained outside the timed interval. Cycles are raw: they include the timing wrapper and counter read (the Baseline row); nothing is subtracted. Per case: lowest min, median of the runs' medians, highest p95 and max.

| Case | min cycles | median cycles | p95 cycles | max cycles | median µs | drops |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Baseline (counter read, no call) | 7 | 7 | 7 | 9 | 0.087 | 0 |
| Disabled (level below runtime filter) | 13 | 13 | 13 | 15 | 0.163 | 0 |
| No arguments | 398 | 398 | 560 | 615 | 4.975 | 0 |
| One u32 | 447 | 447 | 649 | 662 | 5.588 | 0 |
| Three u32 | 520 | 520 | 743 | 750 | 6.500 | 0 |
| 7-byte string | 476 | 476 | 680 | 703 | 5.950 | 0 |
| Span enter + exit | 1164 | 1164 | 1251 | 1269 | 14.550 | 0 |
| One u32, ring full (dropped) | 252 | 252 | 252 | 254 | 3.150 | 768 |

Measured on 2026-11-04 on the code released as v0.2.2 (rtlog.h is unchanged between the measurement and the tag), flashed with OpenOCD 0.12.0 through ST-LINK/V2-1, report captured over the ST-LINK serial bridge at 115200 baud. The three reset runs gave identical statistics. Same board against Zephyr's deferred logger, footprint, and stress tests: [STM32 comparison](https://github.com/NieWil/rtlog/blob/v0.2.2/docs/stm32-comparison.md); raw data: [stm32-l432kc-cycles.csv](https://github.com/NieWil/rtlog/blob/v0.2.2/docs/measurements/stm32-l432kc-cycles.csv).
