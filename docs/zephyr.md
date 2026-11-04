# Zephyr

rtlog ships as a Zephyr module: the C header, a glue file with the clock and a drain thread, a linker snippet that keeps call-site descriptors in ROM, and Kconfig options.

> Status: the module and [`samples/hello`](../c/zephyr/samples/hello) are not built in CI yet, because that needs the Zephyr SDK. The header they use is tested on Linux with GCC and Clang. Reports from boards are welcome.

## Add the module

In your `west.yml`:

```yaml
manifest:
  projects:
    - name: rtlog
      url: https://github.com/NieWil/rtlog
      revision: main
```

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

```sh
west build -b <board> c/zephyr/samples/hello
rtlog contract --elf build/zephyr/zephyr.elf -o app.contract.jsonl
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
