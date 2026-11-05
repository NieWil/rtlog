#include <stdlib.h>
#include <cmsis_core.h>
#include <zephyr/irq.h>
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/logging/log_backend.h>
#include <zephyr/logging/log_ctrl.h>
#include <zephyr/sys/printk.h>

LOG_MODULE_REGISTER(benchmark, LOG_LEVEL_DBG);

#define SAMPLE_COUNT 256u
#define DRAIN_INTERVAL 16u

static uint32_t samples[SAMPLE_COUNT];
static volatile uint32_t input = 42u;
static uint32_t processed;
static uint32_t dropped;

static void discard(const struct log_backend *backend, union log_msg_generic *message) {
    ARG_UNUSED(backend);
    ARG_UNUSED(message);
    processed++;
}

static void count_dropped(const struct log_backend *backend, uint32_t count) {
    ARG_UNUSED(backend);
    dropped += count;
}

static void panic(const struct log_backend *backend) { ARG_UNUSED(backend); }
static const struct log_backend_api backend_api = {
    .process = discard,
    .dropped = count_dropped,
    .panic = panic,
};
LOG_BACKEND_DEFINE(benchmark_sink, backend_api, false);

static void baseline(void) { __asm__ volatile("" ::: "memory"); }
static void disabled(void) { LOG_DBG("disabled=%u", input); }
static void empty(void) { LOG_INF("tick"); }
static void one_u32(void) { LOG_INF("value=%u", input); }
static void three_u32(void) { LOG_INF("values=%u %u %u", input, input, input); }
static void string_arg(void) { LOG_INF("state=%s", "running"); }

static void drain(void) {
    while (log_process()) {
    }
}

static int compare_cycles(const void *left, const void *right) {
    uint32_t first = *(const uint32_t *)left;
    uint32_t second = *(const uint32_t *)right;
    return (first > second) - (first < second);
}

static int run_case(const char *name, void (*operation)(void), int enabled) {
    operation();
    drain();
    uint32_t processed_before = processed;
    uint32_t dropped_before = dropped;
    for (unsigned int index = 0; index < SAMPLE_COUNT; index++) {
        unsigned int key = irq_lock();
        __asm__ volatile("" ::: "memory");
        uint32_t start = DWT->CYCCNT;
        operation();
        __asm__ volatile("" ::: "memory");
        samples[index] = DWT->CYCCNT - start;
        irq_unlock(key);
        if ((index + 1u) % DRAIN_INTERVAL == 0u) {
            drain();
        }
    }
    drain();
    qsort(samples, SAMPLE_COUNT, sizeof(samples[0]), compare_cycles);
    printk("%s,%u,%u,%u,%u,%u\n", name, samples[0], samples[SAMPLE_COUNT / 2u],
           samples[(SAMPLE_COUNT * 95u + 99u) / 100u - 1u], samples[SAMPLE_COUNT - 1u],
           dropped - dropped_before);
    if (processed - processed_before != (enabled ? SAMPLE_COUNT : 0u) ||
        dropped != dropped_before) {
        printk("RTLOG_BENCH_ERROR: unexpected accepted/dropped count in %s\n", name);
        return -1;
    }
    return 0;
}

int main(void) {
    k_msleep(2000);
    CoreDebug->DEMCR |= CoreDebug_DEMCR_TRCENA_Msk;
    if ((DWT->CTRL & DWT_CTRL_NOCYCCNT_Msk) != 0u) {
        printk("RTLOG_BENCH_ERROR: DWT CYCCNT unavailable\n");
        return 0;
    }
    DWT->CYCCNT = 0u;
    DWT->CTRL |= DWT_CTRL_CYCCNTENA_Msk;
    __DSB();
    __ISB();
    uint32_t first = DWT->CYCCNT;
    for (volatile unsigned int spin = 0; spin < 32u; spin++) {
    }
    if (DWT->CYCCNT == first) {
        printk("RTLOG_BENCH_ERROR: DWT CYCCNT is not running\n");
        return 0;
    }
    log_backend_enable(&benchmark_sink, NULL, LOG_LEVEL_INF);
    log_filter_set(&benchmark_sink, 0, log_source_id_get("benchmark"), LOG_LEVEL_INF);
    drain();
    printk("ZEPHYR_BENCH core_hz=%u samples=%u buffer=%u\n",
           (unsigned int)SystemCoreClock, SAMPLE_COUNT, CONFIG_LOG_BUFFER_SIZE);
    printk("case,min_cycles,median_cycles,p95_cycles,max_cycles,drops\n");
    if (run_case("baseline", baseline, 0) != 0 ||
        run_case("runtime_disabled", disabled, 0) != 0 ||
        run_case("no_args", empty, 1) != 0 ||
        run_case("one_u32", one_u32, 1) != 0 ||
        run_case("three_u32", three_u32, 1) != 0 ||
        run_case("string_7_bytes", string_arg, 1) != 0) {
        return 0;
    }
    printk("RTLOG_BENCH_DONE\n");
    return 0;
}