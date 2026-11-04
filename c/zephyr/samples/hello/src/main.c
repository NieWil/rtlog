/* Two threads logging through rtlog; the module's drain thread streams the
 * binary capture to the console UART. On the host:
 *   rtlog contract --elf build/zephyr/zephyr.elf -o app.contract.jsonl
 *   (capture the UART to app.rtlog, e.g. with `cat /dev/ttyACM0 > app.rtlog`)
 *   rtlog decode --contract app.contract.jsonl app.rtlog
 */
#include <zephyr/kernel.h>
#include "rtlog.h"

static void sensor(void *a, void *b, void *c) {
    (void)a, (void)b, (void)c;
    rtlog_set_thread_name("sensor");
    for (unsigned i = 0;; i++) {
        rtlog_span_t s = RTLOG_SPAN_BEGIN("adc.read");
        RTLOG_DEBUG("adc ch={} raw={}", 3, 2000u + (i * 37u) % 400u);
        RTLOG_SPAN_END(s);
        k_msleep(5);
    }
}
K_THREAD_DEFINE(sensor_tid, 1024, sensor, NULL, NULL, NULL, 5, 0, 0);

int main(void) {
    rtlog_set_thread_name("main");
    for (int tick = 0;; tick++) {
        rtlog_span_t t = RTLOG_SPAN_BEGIN("control.tick");
        RTLOG_INFO("tick={} uptime_ms={}", tick, (uint32_t)k_uptime_get_32());
        RTLOG_SPAN_END(t);
        k_msleep(10);
    }
    return 0;
}
