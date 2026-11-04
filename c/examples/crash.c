/* Crash logging from C: log from two threads, then crash for real.
 *
 *   cc -O1 -std=gnu11 -pthread -I c/include c/examples/crash.c -o crash
 *   ./crash fpe crashes/           # or: segv, abort, snapshot, threads
 *   rtlog contract --elf crash -o crash.contract.jsonl
 *   rtlog decode --contract crash.contract.jsonl crashes/crash-<pid>-1.rtlog
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
    /* No rtlog_thread_exit(): on POSIX the slot is released when the thread ends. */
    return NULL;
}

static int discard(void *user, const uint8_t *bytes, size_t len) {
    (void)user, (void)bytes, (void)len;
    return 0;
}

static void *short_lived(void *arg) {
    RTLOG_TRACE("short-lived thread {}", (int)(intptr_t)arg);
    return NULL;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s fpe|segv|abort|snapshot|threads <crash-dir>\n", argv[0]);
        return 2;
    }
    if (rtlog_crash_install(argv[2]) != 0) return 1;
    rtlog_set_thread_name("main");
    if (!strcmp(argv[1], "threads")) {
        /* More threads than slots over the process life (main holds one slot;
         * every round fills the rest): each exiting thread frees its slot by
         * itself (pthread TLS destructor) once a drain pass runs, so none of
         * the later threads is left without a ring. */
        enum { PER_ROUND = RTLOG_MAX_THREADS - 1 };
        for (int round = 0; round < 10; round++) {
            pthread_t ts[PER_ROUND];
            for (int i = 0; i < PER_ROUND; i++)
                pthread_create(&ts[i], NULL, short_lived, (void *)(intptr_t)(round * PER_ROUND + i));
            for (int i = 0; i < PER_ROUND; i++) pthread_join(ts[i], NULL);
            rtlog_drain(discard, NULL);
        }
        printf("dropped %u\n", rtlog_dropped());
        return rtlog_dropped() ? 4 : 0;
    }
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
