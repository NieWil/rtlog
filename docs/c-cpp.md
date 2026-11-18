# C and C++

[`c/include/rtlog.h`](../c/include/rtlog.h) is a single header that writes the same wire format as the Rust core. A capture from a C or C++ program decodes with the same `rtlog` CLI, into text, JSON lines or a Perfetto trace.

## Use it

```c
#define RTLOG_IMPLEMENTATION   /* in exactly one .c or .cpp file */
#include "rtlog.h"

static int write_file(void *f, const uint8_t *bytes, size_t len) {
    return fwrite(bytes, 1, len, (FILE *)f) == len ? 0 : 1;   /* 0 = taken */
}

void control_tick(int tick, double err) {
    rtlog_span_t s = RTLOG_SPAN_BEGIN("control.tick");
    RTLOG_INFO("pid tick={} err={:.3}", tick, err);
    RTLOG_SPAN_END(s);
}

/* in an idle loop or a low-priority thread: */
rtlog_drain(write_file, file);
```

In C++, a span is a scope:

```cpp
void handle(int id, const std::string &route) {
    RTLOG_SPAN(req, "request");                    // closes at end of scope
    RTLOG_INFO("request id={} route={}", id, route);
}
```

Placeholders are `{}`, one per argument, and the host-side specs (`{:x}`, `{:.3}`, `{:b}`) work as in Rust. In C++ a wrong placeholder count fails to compile. C has no such check: the call compiles and logs, and `rtlog contract --elf` then rejects the binary with `site 0x…: 2 placeholders but 1 args`.

## Build, capture, decode

With the `rtlog-dev` Debian/Ubuntu package from the [releases page](https://github.com/NieWil/rtlog/releases) (since 0.2.5), the header is in `/usr/include` and pkg-config gives the flags: `cc demo.c $(pkg-config --cflags --libs rtlog) -o demo`. From a checkout:

```sh
cc  -O2 -std=gnu11 -pthread -I c/include c/examples/demo.c   -o demo
c++ -O2 -std=c++17 -pthread -I c/include c/examples/demo.cpp -o demo_cpp
./demo demo.rtlog
rtlog contract --elf demo -o demo.contract.jsonl     # format strings, read from the binary
rtlog decode --contract demo.contract.jsonl demo.rtlog
rtlog decode --contract demo.contract.jsonl --perfetto -o demo.json demo.rtlog
```

[`scripts/c_e2e.sh`](../scripts/c_e2e.sh) does all of this and checks the output. CI runs it with GCC and Clang on Linux, Apple Clang on macOS and MinGW-w64 GCC on Windows, and runs the C demo under ThreadSanitizer, AddressSanitizer and UBSan on Linux.

### macOS and Windows

The same commands work on macOS (Apple Clang) and, in an MSYS2 or Git Bash shell, on Windows with MinGW-w64 GCC; the binary is then `demo.exe`, and `rtlog contract --elf demo.exe` reads it (the option reads Mach-O and PE as well as ELF).

| Platform | Compilers | Descriptor section | Default clock | Ring slot released at thread exit |
| --- | --- | --- | --- | --- |
| Linux, BSD | GCC, Clang | `rtlog_sites` | `CLOCK_MONOTONIC`, 1 ns | yes (pthread destructor) |
| macOS | Apple Clang, Clang | `__DATA,rtlog_sites` | `CLOCK_MONOTONIC`, 1 ns | yes (pthread destructor) |
| Windows | MinGW-w64 GCC; Clang for the MinGW or the MSVC ABI | `rtlog` | `QueryPerformanceCounter`, usually 100 ns | yes (fiber-local-storage callback) |
| Zephyr, bare metal | GCC, Clang | `rtlog_sites` | set with `rtlog_set_clock` | no: call `rtlog_thread_exit()` |

Microsoft's `cl.exe` cannot compile the header: it uses GNU extensions (statement expressions, `__attribute__((section))`, `__atomic` builtins). Clang for the MSVC ABI (`clang++`, `clang-cl`) accepts them and links with `link.exe` or `lld-link`; the header itself uses only the Win32 API there, no pthreads. The C examples do use pthreads, so they need MinGW-w64; the C++ example uses `std::thread` and builds with either.

## API

| Call | What it does |
| --- | --- |
| `RTLOG_TRACE` … `RTLOG_ERROR(fmt, ...)` | Log with up to 16 arguments |
| `RTLOG_SPAN_BEGIN(name)` / `RTLOG_SPAN_END(s)` | Open and close a span (C) |
| `RTLOG_SPAN(var, name)` | Scoped span (C++) |
| `rtlog_drain(write, user)` | Move pending frames to a transport; call from one place |
| `rtlog_set_thread_name(name)` | Name the calling thread's track |
| `rtlog_thread_exit()` | Release the thread's ring slot early; on POSIX and Windows it is released by itself when the thread exits |
| `rtlog_set_clock(now, hz)` | Use your own clock (DWT, RTOS tick); POSIX defaults to `CLOCK_MONOTONIC`, Windows to the performance counter |
| `rtlog_set_level(level)` | Runtime level filter: `RTLOG_LEVEL_TRACE`, `RTLOG_LEVEL_DEBUG`, `RTLOG_LEVEL_INFO`, `RTLOG_LEVEL_WARN`, `RTLOG_LEVEL_ERROR` or `RTLOG_LEVEL_OFF`; records below `level` are skipped |
| `rtlog_set_spans(on)` | Record spans or not, independently of the level |
| `rtlog_dropped()` | Frames dropped so far |
| `rtlog_crash_install(dir)` / `rtlog_crash_snapshot(reason)` | Crash log: dumps to `<dir>/crash-<pid>-<n>.rtlog`; see [When the process crashes](crash.md) |
| `rtlog_crash_dumps()` | Crash dumps this process has written so far |

The crash log exists on POSIX and Windows: the header defines these three functions only when `__unix__`, `__linux__`, `__APPLE__` or `_WIN32` is defined, so on Zephyr or bare metal a call to them fails to link.

Supported argument types: `bool`, `char`, all integer types, `float`, `double`, `const char *` (and `std::string` in C++). Strings are capped at 127 bytes.

## How it works

- Each thread claims one of `RTLOG_MAX_THREADS` static rings (`RTLOG_RING_SIZE` bytes each) on its first log call. There is no `malloc`. On POSIX and Windows the slot is closed by a thread-exit callback and freed by the next `rtlog_drain`, so a thread pool that creates many threads over its life never runs out; on other targets call `rtlog_thread_exit()` at the end of each thread. A call that finds no free slot is counted and reported as a drop on context 0.
- A call site's descriptor (level, format string, file, line) goes into the `rtlog_sites` section of the binary (`rtlog` in a PE file, where two all-zero marker entries bound it). The ID written on the wire is the descriptor's index in that section, so the log path does no hashing and never reads a string.
- `rtlog contract --elf` reads the section on the host from an ELF, Mach-O or PE binary, applying load-time relocations for position-independent executables.

## Limits in v0.3

- GCC or Clang: Linux, macOS, Windows (MinGW-w64, or Clang for the MSVC ABI), Zephyr, bare metal. Not Microsoft's `cl.exe`.
- Little-endian only.
- Format strings are in `.rodata` by default. On ELF targets, `-DRTLOG_STRIP` and a linker script keep them and the descriptors out of the shipped image: see [Ship without the format strings](strip.md).
- On Zephyr and bare metal, a finished thread keeps its ring slot until `rtlog_thread_exit()`.

For Zephyr, see [Zephyr](zephyr.md).
