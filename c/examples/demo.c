/* rtlog from C: two worker threads and a drain thread writing a capture.
 *
 *   cc -O2 -std=gnu11 -pthread -I c/include c/examples/demo.c -o demo
 *   ./demo demo.rtlog
 *   rtlog contract --elf demo -o demo.contract.jsonl
 *   rtlog decode --contract demo.contract.jsonl demo.rtlog
 */
#define RTLOG_IMPLEMENTATION
#include "rtlog.h"

#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <time.h>

static atomic_int stop;

static int write_file(void *user, const uint8_t *bytes, size_t len) {
    return fwrite(bytes, 1, len, (FILE *)user) == len ? 0 : 1;
}

static void *drain_thread(void *file) {
    while (!atomic_load(&stop)) {
        rtlog_drain(write_file, file);
        struct timespec ms = {0, 1000000};
        nanosleep(&ms, NULL);
    }
    return NULL;
}

static void *sensor(void *arg) {
    (void)arg;
    rtlog_set_thread_name("sensor");
    for (int i = 0; i < 50; i++) {
        rtlog_span_t s = RTLOG_SPAN_BEGIN("adc.read");
        unsigned raw = 2000u + (unsigned)(i * 37 % 400);
        float volts = (float)raw * 3.3f / 4096.0f;
        RTLOG_DEBUG("adc ch={} raw={} volts={:.3}", (unsigned char)3, raw, volts);
        RTLOG_SPAN_END(s);
        if (raw > 2350) RTLOG_WARN("adc high raw={} limit={}", raw, 2350);
    }
    rtlog_thread_exit();
    return NULL;
}

static void *control(void *arg) {
    (void)arg;
    rtlog_set_thread_name("control");
    double err = 12.5;
    for (int tick = 0; tick < 50; tick++) {
        rtlog_span_t t = RTLOG_SPAN_BEGIN("control.tick");
        err *= 0.9;
        RTLOG_INFO("pid tick={} err={:.3} saturated={}", tick, err, (_Bool)(err > 10.0));
        RTLOG_SPAN_END(t);
    }
    RTLOG_ERROR("motor fault code={} name={}", -7, "overcurrent");
    rtlog_thread_exit();
    return NULL;
}

int main(int argc, char **argv) {
    FILE *f = fopen(argc > 1 ? argv[1] : "demo.rtlog", "wb");
    if (!f) return 1;
    rtlog_set_thread_name("main");
    RTLOG_INFO("demo start threads={} version={}", 2, "0.1");

    pthread_t d, a, b;
    pthread_create(&d, NULL, drain_thread, f);
    pthread_create(&a, NULL, sensor, NULL);
    pthread_create(&b, NULL, control, NULL);
    pthread_join(a, NULL);
    pthread_join(b, NULL);
    RTLOG_INFO("demo done dropped={}", rtlog_dropped());
    atomic_store(&stop, 1);
    pthread_join(d, NULL);
    rtlog_drain(write_file, f);
    fclose(f);
    return 0;
}
