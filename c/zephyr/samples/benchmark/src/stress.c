#include <stdlib.h>
#include <string.h>
#include <cmsis_core.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/atomic.h>
#include <zephyr/sys/printk.h>

#define OBSERVATIONS 512u
#define BATCHES 16u

static uint32_t samples[OBSERVATIONS];
static char payload[128];
static volatile uint32_t input = 42u;
static uint32_t delivered;
static atomic_t interrupt_count;

#if defined(CONFIG_RTLOG)
#include "rtlog.h"
#define LOGGER_NAME "rtlog"
static int discard(void *user, const uint8_t *bytes, size_t len) {
    ARG_UNUSED(user);
    ARG_UNUSED(bytes);
    ARG_UNUSED(len);
    return 0;
}
static void emit_integer(void) { RTLOG_INFO("value={}", input); }
static void emit_payload(void) { RTLOG_INFO("payload={}", payload); }
static void drain(void) { delivered += rtlog_drain(discard, NULL); }
static uint32_t drops(void) { return rtlog_dropped(); }
static void initialize(void) {
    rtlog_set_thread_name("stress");
    rtlog_set_level(RTLOG_LEVEL_INFO);
}
#else
#include <zephyr/logging/log.h>
#include <zephyr/logging/log_backend.h>
#include <zephyr/logging/log_ctrl.h>
LOG_MODULE_REGISTER(stress, LOG_LEVEL_DBG);
#define LOGGER_NAME "zephyr"
static uint32_t discarded;
static void discard(const struct log_backend *backend, union log_msg_generic *message) {
    ARG_UNUSED(backend);
    ARG_UNUSED(message);
    delivered++;
}
static void count_dropped(const struct log_backend *backend, uint32_t count) {
    ARG_UNUSED(backend);
    discarded += count;
}
static void panic(const struct log_backend *backend) { ARG_UNUSED(backend); }
static const struct log_backend_api backend_api = {
    .process = discard, .dropped = count_dropped, .panic = panic,
};
LOG_BACKEND_DEFINE(stress_sink, backend_api, false);
static void emit_integer(void) { LOG_INF("value=%u", input); }
static void emit_payload(void) { LOG_INF("payload=%s", payload); }
static void drain(void) { while (log_process()) {} }
static uint32_t drops(void) { return discarded; }
static void initialize(void) { log_backend_enable(&stress_sink, NULL, LOG_LEVEL_INF); }
#endif

static void timer_expired(struct k_timer *timer) {
    ARG_UNUSED(timer);
    atomic_inc(&interrupt_count);
    k_busy_wait(20);
}
K_TIMER_DEFINE(load_timer, timer_expired, NULL);

static int compare_cycles(const void *left, const void *right) {
    uint32_t first = *(const uint32_t *)left;
    uint32_t second = *(const uint32_t *)right;
    return (first > second) - (first < second);
}

static int run_case(const char *name, void (*emit)(void), unsigned int interval,
                    int interrupt_load, int stall) {
    drain();
    uint32_t delivered_before = delivered;
    uint32_t drops_before = drops();
    uint32_t interrupts_before = (uint32_t)atomic_get(&interrupt_count);
    if (interrupt_load) {
        k_timer_start(&load_timer, K_USEC(100), K_USEC(100));
    }
    uint32_t maximum = 0u;
    uint32_t median_max = 0u;
    uint32_t p95_max = 0u;
    uint32_t drain_cycles = 0u;
    for (unsigned int batch = 0; batch < BATCHES; batch++) {
        for (unsigned int index = 0; index < OBSERVATIONS; index++) {
            __asm__ volatile("" ::: "memory");
            uint32_t start = DWT->CYCCNT;
            emit();
            __asm__ volatile("" ::: "memory");
            samples[index] = DWT->CYCCNT - start;
            if (stall) {
                k_busy_wait(13);
            }
            if (interval && (index + 1u) % interval == 0u) {
                start = DWT->CYCCNT;
                drain();
                drain_cycles += DWT->CYCCNT - start;
            }
        }
        qsort(samples, OBSERVATIONS, sizeof(samples[0]), compare_cycles);
        if (samples[OBSERVATIONS - 1u] > maximum) maximum = samples[OBSERVATIONS - 1u];
        if (samples[OBSERVATIONS / 2u] > median_max) median_max = samples[OBSERVATIONS / 2u];
        uint32_t p95 = samples[(OBSERVATIONS * 95u + 99u) / 100u - 1u];
        if (p95 > p95_max) p95_max = p95;
    }
    k_timer_stop(&load_timer);
    uint32_t interrupts = (uint32_t)atomic_get(&interrupt_count) - interrupts_before;
    uint32_t start = DWT->CYCCNT;
    drain();
    drain_cycles += DWT->CYCCNT - start;
    uint32_t accepted = delivered - delivered_before;
    uint32_t lost = drops() - drops_before;
    printk("%s,%u,%u,%u,%u,%u,%u,%u,%u\n", name, OBSERVATIONS * BATCHES,
           accepted, lost, median_max, p95_max, maximum, drain_cycles, interrupts);
    if (accepted + lost != OBSERVATIONS * BATCHES ||
        (interval && lost != 0u) || (!interval && lost == 0u) ||
        (interrupt_load && interrupts == 0u)) {
        printk("STRESS_ERROR accounting_or_load\n");
        return -1;
    }
    delivered_before = delivered;
    drops_before = drops();
    emit_integer();
    drain();
    if (delivered != delivered_before + 1u || drops() != drops_before) {
        printk("STRESS_ERROR recovery\n");
        return -1;
    }
    return 0;
}

int main(void) {
    k_msleep(2000);
    CoreDebug->DEMCR |= CoreDebug_DEMCR_TRCENA_Msk;
    DWT->CYCCNT = 0u;
    DWT->CTRL |= DWT_CTRL_CYCCNTENA_Msk;
    __DSB();
    __ISB();
    uint32_t first = DWT->CYCCNT;
    k_busy_wait(10);
    if (DWT->CYCCNT == first) {
        printk("STRESS_ERROR cycle_counter\n");
        return 0;
    }
    memset(payload, 'x', sizeof(payload) - 1u);
    initialize();
    emit_integer();
    drain();
    printk("STRESS logger=%s core_hz=%u ticks_hz=%u attempts_per_case=%u\n",
           LOGGER_NAME, (unsigned int)SystemCoreClock, CONFIG_SYS_CLOCK_TICKS_PER_SEC,
           OBSERVATIONS * BATCHES);
    printk("case,attempts,accepted,drops,max_batch_median,max_batch_p95,max_cycles,drain_cycles,interrupts\n");
    if (run_case("drained_integer", emit_integer, 16u, 0, 0) ||
        run_case("dynamic_string_127", emit_payload, 4u, 0, 0) ||
        run_case("no_drain_burst", emit_integer, 0u, 0, 0) ||
        run_case("consumer_paused_100ms", emit_integer, 0u, 0, 1) ||
        run_case("irq_load_integer", emit_integer, 16u, 1, 0) ||
        run_case("irq_load_string_127", emit_payload, 4u, 1, 0)) {
        return 0;
    }
    printk("STRESS_DONE\n");
    return 0;
}