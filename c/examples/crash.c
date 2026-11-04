/* Crash logging from C: log from two threads, then crash for real.
 *
 *   cc -O1 -std=gnu11 -pthread -I c/include c/examples/crash.c -o crash
 *   ./crash fpe crash.rtlog        # or: segv, abort, snapshot
 *   rtlog contract --elf crash -o crash.contract.jsonl
 *   rtlog decode --contract crash.contract.jsonl crash.rtlog
 */
#define RTLOG_IMPLEMENTATION
#include "rtlog.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void *worker(void *arg) {
    (void)arg;
    rtlog_set_thread_name("worker");
    for (int i = 0; i < 200; i++) RTLOG_INFO("worker step={}", i);
    rtlog_thread_exit();
    return NULL;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s fpe|segv|abort|snapshot <crash-file>\n", argv[0]);
        return 2;
    }
    if (rtlog_crash_install(argv[2]) != 0) return 1;
    rtlog_set_thread_name("main");
    pthread_t t;
    pthread_create(&t, NULL, worker, NULL);
    pthread_join(t, NULL);
    /* No drain runs: everything below lives only in the rings until the crash. */
    for (int i = 0; i < 500; i++) RTLOG_DEBUG("before crash i={} sensor={}", i, 2000 + i);
    RTLOG_WARN("about to fail mode={}", argv[1]);

    volatile int zero = 0;
    volatile int *null = NULL;
    if (!strcmp(argv[1], "fpe")) {
        volatile int r = 100 / zero; /* SIGFPE on x86 */
        (void)r;
    } else if (!strcmp(argv[1], "segv")) {
        *null = 1; /* SIGSEGV */
    } else if (!strcmp(argv[1], "abort")) {
        abort(); /* SIGABRT */
    } else if (!strcmp(argv[1], "snapshot")) {
        rtlog_crash_snapshot("watchdog: deadline missed");
        return 0;
    }
    return 3;
}
