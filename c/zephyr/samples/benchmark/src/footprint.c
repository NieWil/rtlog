#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>

#if defined(CONFIG_RTLOG)
#include "rtlog.h"
#define BENCH_LOG(number) RTLOG_INFO("site " #number " value={}", input)
static int discard(void *user, const uint8_t *bytes, size_t len) {
    ARG_UNUSED(user);
    ARG_UNUSED(bytes);
    ARG_UNUSED(len);
    return 0;
}
#elif defined(CONFIG_LOG)
#include <zephyr/logging/log.h>
#include <zephyr/logging/log_backend.h>
#include <zephyr/logging/log_ctrl.h>
LOG_MODULE_REGISTER(footprint, LOG_LEVEL_DBG);
#define BENCH_LOG(number) LOG_INF("site " #number " value=%u", input)
static void discard(const struct log_backend *backend, union log_msg_generic *message) {
    ARG_UNUSED(backend);
    ARG_UNUSED(message);
}
static void panic(const struct log_backend *backend) { ARG_UNUSED(backend); }
static const struct log_backend_api backend_api = { .process = discard, .panic = panic };
LOG_BACKEND_DEFINE(footprint_sink, backend_api, false);
#else
#define BENCH_LOG(number) ((void)input)
#endif

static volatile uint32_t input = 42u;

int main(void) {
#if defined(CONFIG_RTLOG)
    rtlog_set_thread_name("footprint");
#elif defined(CONFIG_LOG)
    log_backend_enable(&footprint_sink, NULL, LOG_LEVEL_INF);
#endif
    for (;;) {
        BENCH_LOG(0);
        BENCH_LOG(1);
        BENCH_LOG(2);
        BENCH_LOG(3);
        BENCH_LOG(4);
        BENCH_LOG(5);
        BENCH_LOG(6);
        BENCH_LOG(7);
        BENCH_LOG(8);
        BENCH_LOG(9);
#if defined(CONFIG_RTLOG)
        rtlog_drain(discard, NULL);
#elif defined(CONFIG_LOG)
        while (log_process()) {
        }
#endif
        printk("footprint alive\n");
        k_msleep(1000);
    }
    return 0;
}