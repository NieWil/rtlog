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
#include <unistd.h>
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

static int to_file(void *user, const uint8_t *bytes, size_t len) {
    return fwrite(bytes, 1, len, (FILE *)user) == len ? 0 : 1;
}

static pthread_barrier_t both;

static void *faults_too(void *arg) {
    (void)arg;
    pthread_barrier_wait(&both);
    *(volatile int *)0 = 2; /* SIGSEGV on a second thread, at the same moment */
    return NULL;
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
    if (!strcmp(argv[1], "resync")) {
        /* 12 000 records through the drain into a capture: about 240 KB, so the drain
         * writes several resync frames. */
        char path[512];
        snprintf(path, sizeof path, "%s/resync.rtlog", argv[2]);
        FILE *f = fopen(path, "wb");
        if (!f) return 1;
        for (int round = 0; round < 40; round++) {
            for (int i = 0; i < 300; i++) RTLOG_INFO("sample n={} v={}", round * 300 + i, (round * 300 + i) * 7);
            rtlog_drain(to_file, f);
        }
        rtlog_drain(to_file, f);
        fclose(f);
        return rtlog_dropped() ? 4 : 0;
    }
    if (!strcmp(argv[1], "collide")) {
        /* An earlier run with the same pid left crash-<pid>-1.rtlog behind. */
        char path[512];
        snprintf(path, sizeof path, "%s/crash-%d-1.rtlog", argv[2], (int)getpid());
        FILE *f = fopen(path, "w");
        if (!f) return 1;
        fputs("earlier crash", f);
        fclose(f);
        rtlog_crash_snapshot("second run");
        return rtlog_crash_dumps() == 1 ? 0 : 5;
    }
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
    } else if (!strcmp(argv[1], "flood")) {
        for (int i = 0; i < 40; i++) rtlog_crash_snapshot("noisy watchdog");
        if (rtlog_crash_dumps() != RTLOG_MAX_SOFT_DUMPS) return 5;
        *null = 1; /* a fatal signal is still dumped */
    } else if (!strcmp(argv[1], "two")) {
        pthread_barrier_init(&both, NULL, 2);
        pthread_t t;
        pthread_create(&t, NULL, faults_too, NULL);
        pthread_barrier_wait(&both);
        *null = 1; /* both threads fault at once */
        pthread_join(t, NULL);
    } else if (!strcmp(argv[1], "snapshot")) {
        rtlog_crash_snapshot("watchdog: deadline missed");
        return 0;
    }
    return 3;
}
