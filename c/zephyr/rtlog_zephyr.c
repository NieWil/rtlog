/* rtlog glue for Zephyr: implementation unit, cycle-counter clock, and an
 * optional low-priority drain thread. The default transport writes the binary
 * stream to the console UART; override rtlog_zephyr_write() to use RTT, a
 * dedicated UART, USB CDC or a flash ring instead. */
#define RTLOG_IMPLEMENTATION
#include "rtlog.h"

#include <zephyr/device.h>
#include <zephyr/drivers/uart.h>
#include <zephyr/init.h>
#include <zephyr/kernel.h>

static uint64_t rtlog_zephyr_now(void) {
#ifdef CONFIG_TIMER_HAS_64BIT_CYCLE_COUNTER
    return k_cycle_get_64();
#else
    /* Extend the 32-bit cycle counter; the drain runs often enough that it
     * never wraps twice between two reads. */
    static uint32_t last, high;
    uint32_t now = k_cycle_get_32();
    if (now < last) high++;
    last = now;
    return ((uint64_t)high << 32) | now;
#endif
}

__weak int rtlog_zephyr_write(const uint8_t *bytes, size_t len) {
    const struct device *uart = DEVICE_DT_GET(DT_CHOSEN(zephyr_console));
    if (!device_is_ready(uart)) return 1;
    for (size_t i = 0; i < len; i++) uart_poll_out(uart, bytes[i]);
    return 0;
}

static int rtlog_zephyr_write_cb(void *user, const uint8_t *bytes, size_t len) {
    (void)user;
    return rtlog_zephyr_write(bytes, len);
}

size_t rtlog_zephyr_drain(void) { return rtlog_drain(rtlog_zephyr_write_cb, NULL); }

static int rtlog_zephyr_init(void) {
    rtlog_set_clock(rtlog_zephyr_now, (uint32_t)sys_clock_hw_cycles_per_sec());
    return 0;
}
SYS_INIT(rtlog_zephyr_init, PRE_KERNEL_2, 0);

#ifdef CONFIG_RTLOG_DRAIN_THREAD
static void rtlog_zephyr_drain_thread(void *a, void *b, void *c) {
    (void)a, (void)b, (void)c;
    rtlog_set_thread_name("rtlog-drain");
    for (;;) {
        rtlog_zephyr_drain();
        k_msleep(CONFIG_RTLOG_DRAIN_PERIOD_MS);
    }
}
K_THREAD_DEFINE(rtlog_drain_tid, CONFIG_RTLOG_DRAIN_STACK_SIZE, rtlog_zephyr_drain_thread, NULL, NULL, NULL,
                CONFIG_RTLOG_DRAIN_PRIORITY, 0, 0);
#endif
