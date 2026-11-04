/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * rtlog.h: header-only C / C++ producer for the rtlog wire format (v0.1).
 *
 * A log call writes a site ID, a timestamp delta, a context ID and raw typed
 * argument bytes into the calling thread's own lock-free ring: no formatting,
 * no allocation, no lock. Format strings go into the `rtlog_sites` ELF
 * section; `rtlog contract --elf <binary>` reads them on the host and
 * `rtlog decode` turns a capture into text, JSON lines or a Perfetto trace.
 *
 *   #define RTLOG_IMPLEMENTATION   // in exactly one .c / .cpp file
 *   #include "rtlog.h"
 *
 *   RTLOG_INFO("motor speed={} rpm temp={}", rpm, temp_c);
 *   rtlog_span_t s = RTLOG_SPAN_BEGIN("pid.update");  ...  RTLOG_SPAN_END(s);
 *   rtlog_drain(my_write, my_ctx);                     // idle loop or a thread
 *
 * Placeholders are `{}` (one per argument), as in the Rust API; `{:x}`,
 * `{:.3}` and the other host-side specs work too.
 *
 * Requirements: GCC or Clang, an ELF target (Linux, Zephyr, bare metal),
 * little-endian, C11 (`_Generic`, `_Thread_local`) or C++11.
 * Configuration (define before including): RTLOG_MAX_THREADS (16),
 * RTLOG_RING_SIZE (16384, power of two).
 */
#ifndef RTLOG_H
#define RTLOG_H

#include <stddef.h>
#include <stdint.h>
#include <string.h>

#if !defined(__GNUC__)
#error "rtlog.h needs GCC or Clang (section attributes and __atomic builtins)"
#endif
#if defined(__APPLE__) || defined(_WIN32)
#error "rtlog.h v0.1 supports ELF targets only (Linux, Zephyr, bare metal)"
#endif
#if __BYTE_ORDER__ != __ORDER_LITTLE_ENDIAN__
#error "rtlog.h supports little-endian targets only"
#endif

#ifndef RTLOG_MAX_THREADS
#define RTLOG_MAX_THREADS 16
#endif
#ifndef RTLOG_RING_SIZE
#define RTLOG_RING_SIZE 16384
#endif
#if (RTLOG_RING_SIZE & (RTLOG_RING_SIZE - 1)) != 0 || RTLOG_RING_SIZE < 256
#error "RTLOG_RING_SIZE must be a power of two >= 256"
#endif
#define RTLOG_ARGS_CAP 240
#define RTLOG_DYN_CAP 127

#ifdef __cplusplus
extern "C" {
#endif

enum {
    RTLOG_LEVEL_TRACE = 0,
    RTLOG_LEVEL_DEBUG = 1,
    RTLOG_LEVEL_INFO = 2,
    RTLOG_LEVEL_WARN = 3,
    RTLOG_LEVEL_ERROR = 4,
    RTLOG_LEVEL_OFF = 5
};

/* One call site. Read on the host from the `rtlog_sites` section; the log
 * path only uses its address. Layout is part of the format: do not reorder. */
struct rtlog_site {
    uint32_t magic; /* "RTLS" */
    uint32_t line;
    uint8_t level;
    uint8_t nargs;
    uint8_t kind; /* 0 = log, 1 = span */
    uint8_t reserved;
    uint32_t reserved2;
    const char *fmt;
    const char *file;
    const char *func;
};

typedef struct {
    uint8_t *buf;
    uint32_t len;
    uint32_t overflow;
} rtlog_writer;

typedef struct {
    uint64_t id;
    uint64_t parent;
} rtlog_span_t;

/* A transport: take all `len` bytes (one frame) and return 0, or return
 * non-zero to keep the frame for the next drain. */
typedef int (*rtlog_write_fn)(void *user, const uint8_t *bytes, size_t len);

void rtlog_set_clock(uint64_t (*now)(void), uint32_t hz);
void rtlog_set_level(int level);
void rtlog_set_thread_name(const char *name);
void rtlog_thread_exit(void);
void rtlog_push(const struct rtlog_site *site, const rtlog_writer *w);
rtlog_span_t rtlog_span_enter(const struct rtlog_site *site);
void rtlog_span_exit(rtlog_span_t span);
size_t rtlog_drain(rtlog_write_fn write, void *user);
uint32_t rtlog_dropped(void);

extern int rtlog__level;

/* ---- argument encoding (tags match the Rust `Tag` enum) ---------------- */

static inline void rtlog__put(rtlog_writer *w, const void *p, uint32_t n) {
    if (w->len + n <= RTLOG_ARGS_CAP) {
        memcpy(w->buf + w->len, p, n);
        w->len += n;
    } else {
        w->overflow = 1;
    }
}

static inline void rtlog__tagged(rtlog_writer *w, uint8_t tag, const void *p, uint32_t n) {
    rtlog__put(w, &tag, 1);
    rtlog__put(w, p, n);
}

#define RTLOG__SCALAR(name, type, tag) \
    static inline void name(rtlog_writer *w, type v) { rtlog__tagged(w, tag, &v, sizeof v); }
RTLOG__SCALAR(rtlog__u8, uint8_t, 1)
RTLOG__SCALAR(rtlog__u16, uint16_t, 2)
RTLOG__SCALAR(rtlog__u32, uint32_t, 3)
RTLOG__SCALAR(rtlog__u64, uint64_t, 4)
RTLOG__SCALAR(rtlog__i8, int8_t, 5)
RTLOG__SCALAR(rtlog__i16, int16_t, 6)
RTLOG__SCALAR(rtlog__i32, int32_t, 7)
RTLOG__SCALAR(rtlog__i64, int64_t, 8)
RTLOG__SCALAR(rtlog__f32, float, 9)
RTLOG__SCALAR(rtlog__f64, double, 10)

static inline void rtlog__boolv(rtlog_writer *w, int v) {
    uint8_t b = v ? 1 : 0;
    rtlog__tagged(w, 11, &b, 1);
}
static inline void rtlog__charv(rtlog_writer *w, char c) {
    uint32_t cp = (uint32_t)(unsigned char)c;
    rtlog__tagged(w, 12, &cp, 4);
}
static inline void rtlog__long(rtlog_writer *w, long v) {
    if (sizeof(long) == 8) rtlog__i64(w, (int64_t)v); else rtlog__i32(w, (int32_t)v);
}
static inline void rtlog__ulong(rtlog_writer *w, unsigned long v) {
    if (sizeof(long) == 8) rtlog__u64(w, (uint64_t)v); else rtlog__u32(w, (uint32_t)v);
}
static inline void rtlog__str(rtlog_writer *w, const char *s) {
    size_t n = s ? strlen(s) : 0;
    uint8_t len;
    if (n > RTLOG_DYN_CAP) {
        n = RTLOG_DYN_CAP;
        while (n > 0 && ((unsigned char)s[n] & 0xC0) == 0x80) n--; /* keep UTF-8 whole */
        len = (uint8_t)(n | 0x80);
    } else {
        len = (uint8_t)n;
    }
    uint8_t tag = 13;
    rtlog__put(w, &tag, 1);
    rtlog__put(w, &len, 1);
    rtlog__put(w, s, (uint32_t)n);
}

/* ---- call-site descriptors ---------------------------------------------- */

#define RTLOG__SECTION __attribute__((section("rtlog_sites"), used, aligned(sizeof(void *))))
#define RTLOG__SITE(lvl, kind_, fmt_, n)                                                       \
    __extension__({                                                                            \
        static const struct rtlog_site rtlog__site_ RTLOG__SECTION = {                        \
            0x534c5452u, __LINE__, (uint8_t)(lvl), (uint8_t)(n), (uint8_t)(kind_), 0, 0,       \
            fmt_, __FILE__, __func__};                                                         \
        &rtlog__site_;                                                                         \
    })

/* ---- argument counting and expansion (up to 16 arguments) --------------- */

#define RTLOG__CAT_(a, b) a##b
#define RTLOG__CAT(a, b) RTLOG__CAT_(a, b)
#define RTLOG__NARG(...) \
    RTLOG__NARG_(0, ##__VA_ARGS__, 16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0)
#define RTLOG__NARG_(_0, _1, _2, _3, _4, _5, _6, _7, _8, _9, _10, _11, _12, _13, _14, _15, _16, N, ...) N
#define RTLOG__FE_0(w)
#define RTLOG__FE_1(w, a) RTLOG__ARG(w, a)
#define RTLOG__FE_2(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_1(w, __VA_ARGS__)
#define RTLOG__FE_3(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_2(w, __VA_ARGS__)
#define RTLOG__FE_4(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_3(w, __VA_ARGS__)
#define RTLOG__FE_5(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_4(w, __VA_ARGS__)
#define RTLOG__FE_6(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_5(w, __VA_ARGS__)
#define RTLOG__FE_7(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_6(w, __VA_ARGS__)
#define RTLOG__FE_8(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_7(w, __VA_ARGS__)
#define RTLOG__FE_9(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_8(w, __VA_ARGS__)
#define RTLOG__FE_10(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_9(w, __VA_ARGS__)
#define RTLOG__FE_11(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_10(w, __VA_ARGS__)
#define RTLOG__FE_12(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_11(w, __VA_ARGS__)
#define RTLOG__FE_13(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_12(w, __VA_ARGS__)
#define RTLOG__FE_14(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_13(w, __VA_ARGS__)
#define RTLOG__FE_15(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_14(w, __VA_ARGS__)
#define RTLOG__FE_16(w, a, ...) RTLOG__ARG(w, a) RTLOG__FE_15(w, __VA_ARGS__)
#define RTLOG__FE(w, ...) RTLOG__CAT(RTLOG__FE_, RTLOG__NARG(__VA_ARGS__))(w, ##__VA_ARGS__)

#ifdef __cplusplus
} /* extern "C" */
#endif

#ifdef __cplusplus
/* C++: overloads pick the wire type; a constexpr check matches placeholders
 * to arguments at compile time. */
namespace rtlog_detail {
inline void put(rtlog_writer *w, bool v) { rtlog__boolv(w, v); }
inline void put(rtlog_writer *w, char v) { rtlog__charv(w, v); }
inline void put(rtlog_writer *w, signed char v) { rtlog__i8(w, v); }
inline void put(rtlog_writer *w, unsigned char v) { rtlog__u8(w, v); }
inline void put(rtlog_writer *w, short v) { rtlog__i16(w, v); }
inline void put(rtlog_writer *w, unsigned short v) { rtlog__u16(w, v); }
inline void put(rtlog_writer *w, int v) { rtlog__i32(w, v); }
inline void put(rtlog_writer *w, unsigned int v) { rtlog__u32(w, v); }
inline void put(rtlog_writer *w, long v) { rtlog__long(w, v); }
inline void put(rtlog_writer *w, unsigned long v) { rtlog__ulong(w, v); }
inline void put(rtlog_writer *w, long long v) { rtlog__i64(w, v); }
inline void put(rtlog_writer *w, unsigned long long v) { rtlog__u64(w, v); }
inline void put(rtlog_writer *w, float v) { rtlog__f32(w, v); }
inline void put(rtlog_writer *w, double v) { rtlog__f64(w, v); }
inline void put(rtlog_writer *w, const char *v) { rtlog__str(w, v); }
/* std::string, std::string_view-like types with c_str(). */
template <class T>
auto put(rtlog_writer *w, const T &s) -> decltype(s.c_str(), void()) { rtlog__str(w, s.c_str()); }

constexpr unsigned count_placeholders(const char *s, unsigned n = 0) {
    return *s == 0 ? n
         : (s[0] == '{' && s[1] == '{') ? count_placeholders(s + 2, n)
         : (s[0] == '}' && s[1] == '}') ? count_placeholders(s + 2, n)
         : s[0] == '{' ? count_placeholders(s + 1, n + 1)
         : count_placeholders(s + 1, n);
}
} // namespace rtlog_detail
#define RTLOG__ARG(w, x) rtlog_detail::put(w, x);
#define RTLOG__CHECK(fmt, n) \
    static_assert(rtlog_detail::count_placeholders(fmt) == (n), "rtlog: placeholder count does not match arguments");

namespace rtlog {
/* RAII span: `RTLOG_SPAN(s, "db.query");` closes at end of scope. */
class Span {
  public:
    explicit Span(const rtlog_site *site) : s_(rtlog_span_enter(site)) {}
    ~Span() { rtlog_span_exit(s_); }
    Span(const Span &) = delete;
    Span &operator=(const Span &) = delete;
  private:
    rtlog_span_t s_;
};
} // namespace rtlog
#define RTLOG_SPAN(var, name) ::rtlog::Span var(RTLOG__SITE(RTLOG_LEVEL_INFO, 1, name, 0))
#else
#define RTLOG__ARG(w, x)                                                                        \
    _Generic((x),                                                                               \
        _Bool: rtlog__boolv,                                                                    \
        char: rtlog__charv,                                                                     \
        signed char: rtlog__i8,                                                                 \
        unsigned char: rtlog__u8,                                                               \
        short: rtlog__i16,                                                                      \
        unsigned short: rtlog__u16,                                                             \
        int: rtlog__i32,                                                                        \
        unsigned int: rtlog__u32,                                                               \
        long: rtlog__long,                                                                      \
        unsigned long: rtlog__ulong,                                                            \
        long long: rtlog__i64,                                                                  \
        unsigned long long: rtlog__u64,                                                         \
        float: rtlog__f32,                                                                      \
        double: rtlog__f64,                                                                     \
        char *: rtlog__str,                                                                     \
        const char *: rtlog__str)(w, x);
#define RTLOG__CHECK(fmt, n)
#endif

/* ---- the public macros -------------------------------------------------- */

#define RTLOG_LOG(lvl, fmt, ...)                                                                \
    do {                                                                                        \
        RTLOG__CHECK(fmt, RTLOG__NARG(__VA_ARGS__))                                             \
        const struct rtlog_site *rtlog__s = RTLOG__SITE(lvl, 0, fmt, RTLOG__NARG(__VA_ARGS__)); \
        if ((lvl) >= __atomic_load_n(&rtlog__level, __ATOMIC_RELAXED)) {                        \
            uint8_t rtlog__b[RTLOG_ARGS_CAP];                                                   \
            rtlog_writer rtlog__w = {rtlog__b, 0, 0};                                           \
            RTLOG__FE(&rtlog__w, ##__VA_ARGS__)                                                 \
            rtlog_push(rtlog__s, &rtlog__w);                                                    \
        }                                                                                       \
    } while (0)

#define RTLOG_TRACE(fmt, ...) RTLOG_LOG(RTLOG_LEVEL_TRACE, fmt, ##__VA_ARGS__)
#define RTLOG_DEBUG(fmt, ...) RTLOG_LOG(RTLOG_LEVEL_DEBUG, fmt, ##__VA_ARGS__)
#define RTLOG_INFO(fmt, ...) RTLOG_LOG(RTLOG_LEVEL_INFO, fmt, ##__VA_ARGS__)
#define RTLOG_WARN(fmt, ...) RTLOG_LOG(RTLOG_LEVEL_WARN, fmt, ##__VA_ARGS__)
#define RTLOG_ERROR(fmt, ...) RTLOG_LOG(RTLOG_LEVEL_ERROR, fmt, ##__VA_ARGS__)

#define RTLOG_SPAN_BEGIN(name) rtlog_span_enter(RTLOG__SITE(RTLOG_LEVEL_INFO, 1, name, 0))
#define RTLOG_SPAN_END(span) rtlog_span_exit(span)

/* ======================================================================== */
#ifdef RTLOG_IMPLEMENTATION

#ifdef __cplusplus
#define RTLOG__TLS thread_local
extern "C" {
#else
#define RTLOG__TLS _Thread_local
#endif

#if defined(__unix__) || defined(__linux__)
#include <time.h>
static uint64_t rtlog__monotonic_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000000u + (uint64_t)t.tv_nsec;
}
#define RTLOG__DEFAULT_NOW rtlog__monotonic_ns
#define RTLOG__DEFAULT_HZ 1000000000u
#else
static uint64_t rtlog__no_clock(void) { return 0; }
#define RTLOG__DEFAULT_NOW rtlog__no_clock
#define RTLOG__DEFAULT_HZ 1u
#endif

#define RTLOG__CTRL_DROPPED 0xFFFFFF01u
#define RTLOG__CTRL_CTX_REGISTER 0xFFFFFF02u
#define RTLOG__CTRL_CTX_CLOSE 0xFFFFFF03u
#define RTLOG__CTRL_SPAN_OPEN 0xFFFFFF04u
#define RTLOG__CTRL_SPAN_CLOSE 0xFFFFFF05u

enum { RTLOG__FREE = 0, RTLOG__CLAIMED = 1, RTLOG__LIVE = 2, RTLOG__CLOSED = 3 };

struct rtlog__ring {
    uint32_t head;      /* producer, atomic */
    uint32_t tail;      /* consumer, atomic */
    uint32_t drops;     /* atomic */
    uint32_t state;     /* atomic */
    uint32_t ctx;
    uint32_t announced; /* atomic; cleared to re-announce a rename */
    uint64_t last_ts;   /* producer only */
    uint64_t span_seq;  /* producer only */
    uint64_t current_span;
    char name[32];
    uint8_t buf[RTLOG_RING_SIZE];
};

extern const struct rtlog_site __start_rtlog_sites[] __attribute__((weak));
extern const struct rtlog_site __stop_rtlog_sites[] __attribute__((weak));

int rtlog__level = RTLOG_LEVEL_TRACE;
static struct rtlog__ring rtlog__rings[RTLOG_MAX_THREADS];
static RTLOG__TLS struct rtlog__ring *rtlog__mine;
static uint64_t (*rtlog__now)(void) = RTLOG__DEFAULT_NOW;
static uint32_t rtlog__hz = RTLOG__DEFAULT_HZ;
static uint32_t rtlog__next_ctx = 1;
static uint32_t rtlog__busy;
static uint32_t rtlog__header_sent;
static uint32_t rtlog__total_drops;

void rtlog_set_clock(uint64_t (*now)(void), uint32_t hz) {
    rtlog__now = now;
    rtlog__hz = hz ? hz : 1;
}

void rtlog_set_level(int level) { __atomic_store_n(&rtlog__level, level, __ATOMIC_RELAXED); }

uint32_t rtlog_dropped(void) { return __atomic_load_n(&rtlog__total_drops, __ATOMIC_RELAXED); }

static size_t rtlog__varint(uint64_t v, uint8_t *out) {
    size_t i = 0;
    while (v >= 0x80) {
        out[i++] = (uint8_t)(v | 0x80);
        v >>= 7;
    }
    out[i++] = (uint8_t)v;
    return i;
}

static void rtlog__set_name(struct rtlog__ring *r, const char *name) {
    size_t n = strlen(name);
    if (n > sizeof r->name - 1) n = sizeof r->name - 1;
    memcpy(r->name, name, n);
    r->name[n] = 0;
}

static struct rtlog__ring *rtlog__claim(void) {
    if (rtlog__mine) return rtlog__mine;
    for (int i = 0; i < RTLOG_MAX_THREADS; i++) {
        struct rtlog__ring *r = &rtlog__rings[i];
        uint32_t expected = RTLOG__FREE;
        if (__atomic_compare_exchange_n(&r->state, &expected, RTLOG__CLAIMED, 0, __ATOMIC_ACQ_REL,
                                        __ATOMIC_RELAXED)) {
            r->ctx = __atomic_fetch_add(&rtlog__next_ctx, 1, __ATOMIC_RELAXED);
            r->last_ts = 0;
            r->span_seq = 0;
            r->current_span = 0;
            char name[24] = "thread-";
            uint8_t digits[10];
            size_t nd = 0, pos = 7;
            uint32_t c = r->ctx;
            do {
                digits[nd++] = (uint8_t)('0' + c % 10);
                c /= 10;
            } while (c);
            while (nd) name[pos++] = (char)digits[--nd];
            name[pos] = 0;
            rtlog__set_name(r, name);
            __atomic_store_n(&r->announced, 0, __ATOMIC_RELAXED);
            __atomic_store_n(&r->state, RTLOG__LIVE, __ATOMIC_RELEASE);
            rtlog__mine = r;
            return r;
        }
    }
    return 0;
}

void rtlog_set_thread_name(const char *name) {
    struct rtlog__ring *r = rtlog__claim();
    if (!r) return;
    rtlog__set_name(r, name);
    __atomic_store_n(&r->announced, 0, __ATOMIC_RELEASE);
}

void rtlog_thread_exit(void) {
    if (rtlog__mine) {
        __atomic_store_n(&rtlog__mine->state, RTLOG__CLOSED, __ATOMIC_RELEASE);
        rtlog__mine = 0;
    }
}

static void rtlog__copy_in(struct rtlog__ring *r, uint32_t pos, const void *src, uint32_t n) {
    uint32_t idx = pos & (RTLOG_RING_SIZE - 1), first = RTLOG_RING_SIZE - idx;
    if (first > n) first = n;
    memcpy(r->buf + idx, src, first);
    memcpy(r->buf, (const uint8_t *)src + first, n - first);
}

static void rtlog__copy_out(const struct rtlog__ring *r, uint32_t pos, void *dst, uint32_t n) {
    uint32_t idx = pos & (RTLOG_RING_SIZE - 1), first = RTLOG_RING_SIZE - idx;
    if (first > n) first = n;
    memcpy(dst, r->buf + idx, first);
    memcpy((uint8_t *)dst + first, r->buf, n - first);
}

static int rtlog__push_raw(struct rtlog__ring *r, uint32_t site, const uint8_t *args, uint32_t alen) {
    uint64_t ts = rtlog__now();
    uint8_t meta[4 + 10 + 5];
    memcpy(meta, &site, 4);
    size_t m = 4;
    m += rtlog__varint(ts > r->last_ts ? ts - r->last_ts : 0, meta + m);
    m += rtlog__varint(r->ctx, meta + m);
    uint32_t len = (uint32_t)m + alen, need = 2 + len;
    uint32_t head = __atomic_load_n(&r->head, __ATOMIC_RELAXED);
    uint32_t tail = __atomic_load_n(&r->tail, __ATOMIC_ACQUIRE);
    if (alen > RTLOG_ARGS_CAP + 20 || RTLOG_RING_SIZE - (head - tail) < need) {
        __atomic_fetch_add(&r->drops, 1, __ATOMIC_RELAXED);
        return 0;
    }
    uint16_t len16 = (uint16_t)len;
    rtlog__copy_in(r, head, &len16, 2);
    rtlog__copy_in(r, head + 2, meta, (uint32_t)m);
    rtlog__copy_in(r, head + 2 + (uint32_t)m, args, alen);
    if (ts > r->last_ts) r->last_ts = ts;
    __atomic_store_n(&r->head, head + need, __ATOMIC_RELEASE);
    return 1;
}

static uint32_t rtlog__site_id(const struct rtlog_site *s) {
    return (uint32_t)(s - __start_rtlog_sites) + 1;
}

void rtlog_push(const struct rtlog_site *site, const rtlog_writer *w) {
    struct rtlog__ring *r = rtlog__claim();
    if (!r) {
        __atomic_fetch_add(&rtlog__total_drops, 1, __ATOMIC_RELAXED);
        return;
    }
    if (w->overflow) {
        __atomic_fetch_add(&r->drops, 1, __ATOMIC_RELAXED);
        return;
    }
    rtlog__push_raw(r, rtlog__site_id(site), w->buf, w->len);
}

rtlog_span_t rtlog_span_enter(const struct rtlog_site *site) {
    rtlog_span_t s = {0, 0};
    struct rtlog__ring *r;
    if (RTLOG_LEVEL_INFO < __atomic_load_n(&rtlog__level, __ATOMIC_RELAXED) || !(r = rtlog__claim())) return s;
    uint64_t seq = ++r->span_seq;
    uint64_t id = ((uint64_t)(r->ctx & 0xffff) << 48) | (seq & 0xffffffffffffull);
    uint8_t p[20];
    uint32_t sid = rtlog__site_id(site);
    memcpy(p, &sid, 4);
    memcpy(p + 4, &id, 8);
    memcpy(p + 12, &r->current_span, 8);
    s.parent = r->current_span;
    if (rtlog__push_raw(r, RTLOG__CTRL_SPAN_OPEN, p, 20)) {
        s.id = id;
        r->current_span = id;
    }
    return s;
}

void rtlog_span_exit(rtlog_span_t s) {
    struct rtlog__ring *r = rtlog__mine;
    if (!s.id || !r) return;
    rtlog__push_raw(r, RTLOG__CTRL_SPAN_CLOSE, (const uint8_t *)&s.id, 8);
    r->current_span = s.parent;
}

static uint32_t rtlog__fnv(const uint8_t *p, size_t n) {
    uint32_t h = 0x811c9dc5u;
    while (n--) h = (h ^ *p++) * 0x01000193u;
    return h;
}

static int rtlog__control(rtlog_write_fn wr, void *user, uint32_t id, uint32_t ctx, const void *tail, size_t tn) {
    uint8_t b[4 + 5 + 1 + 32 + 4];
    memcpy(b, &id, 4);
    size_t n = 4 + rtlog__varint(ctx, b + 4);
    memcpy(b + n, tail, tn);
    return wr(user, b, n + tn);
}

size_t rtlog_drain(rtlog_write_fn wr, void *user) {
    uint32_t idle = 0;
    if (!__atomic_compare_exchange_n(&rtlog__busy, &idle, 1, 0, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) return 0;
    size_t frames = 0;
    uint8_t frame[2 + 19 + RTLOG_ARGS_CAP + 20];
    if (!rtlog__header_sent) {
        uint32_t n = __start_rtlog_sites ? (uint32_t)(__stop_rtlog_sites - __start_rtlog_sites) : 0, hash = 0;
        for (uint32_t id = 1; id <= n; id++) hash += rtlog__fnv((const uint8_t *)&id, 4);
        uint8_t h[17] = {'R', 'T', 'L', '1', 1, 0};
        memcpy(h + 6, &hash, 4);
        memcpy(h + 10, &rtlog__hz, 4);
        h[14] = (uint8_t)sizeof(void *);
        if (wr(user, h, sizeof h) != 0) goto out;
        rtlog__header_sent = 1;
    }
    for (int i = 0; i < RTLOG_MAX_THREADS; i++) {
        struct rtlog__ring *r = &rtlog__rings[i];
        uint32_t st = __atomic_load_n(&r->state, __ATOMIC_ACQUIRE);
        if (st < RTLOG__LIVE) continue;
        if (!__atomic_load_n(&r->announced, __ATOMIC_ACQUIRE)) {
            uint8_t name[33];
            size_t nl = strlen(r->name);
            name[0] = (uint8_t)nl;
            memcpy(name + 1, r->name, nl);
            if (rtlog__control(wr, user, RTLOG__CTRL_CTX_REGISTER, r->ctx, name, nl + 1) != 0) goto out;
            __atomic_store_n(&r->announced, 1, __ATOMIC_RELEASE);
        }
        uint32_t d = __atomic_exchange_n(&r->drops, 0, __ATOMIC_RELAXED);
        if (d) {
            if (rtlog__control(wr, user, RTLOG__CTRL_DROPPED, r->ctx, &d, 4) != 0) {
                __atomic_fetch_add(&r->drops, d, __ATOMIC_RELAXED);
                goto out;
            }
            __atomic_fetch_add(&rtlog__total_drops, d, __ATOMIC_RELAXED);
        }
        uint32_t tail = __atomic_load_n(&r->tail, __ATOMIC_RELAXED);
        for (;;) {
            uint32_t head = __atomic_load_n(&r->head, __ATOMIC_ACQUIRE);
            if (head == tail) break;
            uint16_t len;
            rtlog__copy_out(r, tail, &len, 2);
            rtlog__copy_out(r, tail + 2, frame, len);
            if (wr(user, frame, len) != 0) goto out;
            tail += 2u + len;
            __atomic_store_n(&r->tail, tail, __ATOMIC_RELEASE);
            frames++;
        }
        if (st == RTLOG__CLOSED && !__atomic_load_n(&r->drops, __ATOMIC_RELAXED)) {
            if (rtlog__control(wr, user, RTLOG__CTRL_CTX_CLOSE, r->ctx, "", 0) != 0) goto out;
            __atomic_store_n(&r->state, RTLOG__FREE, __ATOMIC_RELEASE);
        }
    }
out:
    __atomic_store_n(&rtlog__busy, 0, __ATOMIC_RELEASE);
    return frames;
}

#ifdef __cplusplus
} /* extern "C" */
#endif
#endif /* RTLOG_IMPLEMENTATION */
#endif /* RTLOG_H */
