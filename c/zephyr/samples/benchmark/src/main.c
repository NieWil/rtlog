#include <stdlib.h>
#include <cmsis_core.h>
#include <zephyr/irq.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>
#include <zephyr/sys/util.h>
#include "rtlog.h"

#define SAMPLE_COUNT 256u
#define DRAIN_INTERVAL 16u

static uint32_t samples[SAMPLE_COUNT];
static volatile uint32_t input = 42u;

static int discard(void *user, const uint8_t *bytes, size_t len) {
    ARG_UNUSED(user);
    ARG_UNUSED(bytes);
    ARG_UNUSED(len);
    return 0;
}

static void baseline(void) { __asm__ volatile("" ::: "memory"); }
static void disabled(void) { RTLOG_DEBUG("disabled={}", input); }
static void empty(void) { RTLOG_INFO("tick"); }
static void one_u32(void) { RTLOG_INFO("value={}", input); }
static void three_u32(void) { RTLOG_INFO("values={} {} {}", input, input, input); }
static void string_arg(void) { RTLOG_INFO("state={}", "running"); }
static void span_pair(void) {
    rtlog_span_t span = RTLOG_SPAN_BEGIN("work");
    RTLOG_SPAN_END(span);
}

static int compare_cycles(const void *left, const void *right) {
    uint32_t first = *(const uint32_t *)left;
    uint32_t second = *(const uint32_t *)right;
    return (first > second) - (first < second);
}

static uint32_t measure(void (*operation)(void)) {
    unsigned int key = irq_lock();
    __asm__ volatile("" ::: "memory");
    uint32_t start = DWT->CYCCNT;
    operation();
    __asm__ volatile("" ::: "memory");
    uint32_t elapsed = DWT->CYCCNT - start;
    irq_unlock(key);
    return elapsed;
}

static int run_case(const char *name, void (*operation)(void), int full) {
    rtlog_drain(discard, NULL);
    if (full) {
        for (unsigned int index = 0; index < CONFIG_RTLOG_RING_SIZE; index++) {
            one_u32();
        }
    } else {
        for (unsigned int index = 0; index < DRAIN_INTERVAL; index++) {
            operation();
        }
        rtlog_drain(discard, NULL);
    }
    uint32_t drops_before = rtlog_dropped();
    for (unsigned int index = 0; index < SAMPLE_COUNT; index++) {
        samples[index] = measure(operation);
        if (!full && (index + 1u) % DRAIN_INTERVAL == 0u) {
            rtlog_drain(discard, NULL);
        }
    }
    size_t remaining_frames = rtlog_drain(discard, NULL);
    uint32_t drops = rtlog_dropped() - drops_before;
    if (full) {
        drops -= CONFIG_RTLOG_RING_SIZE - (uint32_t)remaining_frames;
    }
    qsort(samples, SAMPLE_COUNT, sizeof(samples[0]), compare_cycles);
    printk("%s,%u,%u,%u,%u,%u\n", name, samples[0],
           samples[SAMPLE_COUNT / 2u], samples[(SAMPLE_COUNT * 95u + 99u) / 100u - 1u],
           samples[SAMPLE_COUNT - 1u], drops);
    if ((!full && drops != 0u) || (full && drops != SAMPLE_COUNT)) {
        printk("RTLOG_BENCH_ERROR: unexpected drops in %s\n", name);
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

    rtlog_set_thread_name("benchmark");
    rtlog_set_level(RTLOG_LEVEL_INFO);
    rtlog_set_spans(1);
    rtlog_drain(discard, NULL);
    printk("RTLOG_BENCH core_hz=%u timestamp_hz=%u samples=%u ring=%u threads=%u\n",
           (unsigned int)SystemCoreClock, (unsigned int)sys_clock_hw_cycles_per_sec(),
           SAMPLE_COUNT, CONFIG_RTLOG_RING_SIZE, CONFIG_RTLOG_MAX_THREADS);
    printk("case,min_cycles,median_cycles,p95_cycles,max_cycles,drops\n");
    if (run_case("baseline", baseline, 0) != 0 ||
        run_case("runtime_disabled", disabled, 0) != 0 ||
        run_case("no_args", empty, 0) != 0 ||
        run_case("one_u32", one_u32, 0) != 0 ||
        run_case("three_u32", three_u32, 0) != 0 ||
        run_case("string_7_bytes", string_arg, 0) != 0 ||
        run_case("span_pair", span_pair, 0) != 0 ||
        run_case("full_ring_one_u32", one_u32, 1) != 0) {
        return 0;
    }
    printk("RTLOG_BENCH_DONE\n");
    return 0;
}