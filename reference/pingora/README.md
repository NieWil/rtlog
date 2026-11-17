# Reference application: Pingora

[Pingora](https://github.com/cloudflare/pingora) is Cloudflare's HTTP proxy framework in Rust (Apache-2.0). It is the first reference application of the [roadmap](../../docs/roadmap.md) (section 12a): a program rtlog's authors did not write, where the log is essential (a proxy is operated from its access log) and where the logger sits on the latency path of every request.

This directory is the first step of the port: Pingora's own code is unchanged, and the **access log**, the one record a proxy writes per request, goes through the `log` crate, through `tracing`, or through rtlog. It answers one question: what does the logger add to a request, in instructions, time and memory?

Pinned to Pingora 0.9.0, revision `3f87e92c` (in `Cargo.toml`). The crate is outside the host workspace, like `size/mcu`.

CI runs a short round on every pull request (`bench` workflow, job `Bencher (pingora)`) and sends the instruction counts to Bencher, where a growth of more than 2 % fails the pull request.

## Run it

```sh
reference/pingora/run.sh            # 10 s per run, 32 connections, 4 proxy threads, 3 rounds
reference/pingora/run.sh 3 16 2 1   # a short check
```

One binary holds an upstream HTTP server, the Pingora proxy in front of it and a load generator (keep-alive connections that send one request after the other and time each). The first build takes about three minutes. The script runs the variants in turn, each round starting with another one, counts the instructions of the access-log call with callgrind if valgrind is installed (in a mode without the proxy, so that the count is the same on every run), then checks that the text log and the decoded rtlog capture both hold the access records.

| Variant | Access log |
| --- | --- |
| `none` | no access log: the proxy alone |
| `log` | `log::info!` with `env_logger` writing text to a file, one `write` per record, as with its default stderr target |
| `log-buf` | the same through a 64 KiB `BufWriter`: a `write` only when the buffer is full |
| `tracing` | `tracing::info!` with `tracing-subscriber`'s text format and `tracing-appender`'s non-blocking writer: formatted on the worker thread, written by a background thread |
| `rtlog` | `rtlog::info!` into the worker thread's ring; a drain thread writes the binary frames to a file every millisecond |

All loggers get the same six fields: method, path, status, body bytes, peer port, duration. The three text loggers run with their default line format, which starts with a formatted timestamp.

## Result

Intel Xeon at 2.30 GHz, 4 virtual CPUs of a shared cloud VM, Linux 6.18, rustc 1.97.0, release build with thin LTO, valgrind 3.22. 8 s per run, 32 connections, 4 proxy threads, 3 rounds; upstream, proxy and load generator share the 4 CPUs.

### Instructions per access-log call

Counted with callgrind inside the one function that makes the log call: 20 000 calls with fixed fields from one thread, without the proxy (`rtlog-reference-pingora icount <logger>`), after one call that sets the logger up. This is the number to rely on. Two runs gave the same count for `log` and `log-buf` and counts within 0.01 % of each other for `tracing` and rtlog.

| Access log | Instructions per call | Against rtlog |
| --- | ---: | ---: |
| log | 9881 | 32× |
| log-buf | 9965 | 32× |
| tracing | 9351 | 30× |
| rtlog | 313 | 1× |

Buffering the file (`log-buf`) or moving the write to another thread (`tracing`) changes almost nothing here: more than 9 000 instructions go into formatting the line on the calling thread, timestamp included, and that part stays. rtlog copies two short strings and four integers into the thread's ring.

### How regular the log call is

The time of each access-log call inside the running proxy, as a distribution: the longest calls are what a latency-sensitive program pays for its log.

| Access log | Median | 99 % of calls under | 99.9 % under | Longest call |
| --- | ---: | ---: | ---: | ---: |
| log | 3.3 to 3.6 µs | 74 to 82 µs | 197 to 328 µs | 2.7 to 20.1 ms |
| log-buf | 3.6 to 3.8 µs | 74 to 82 µs | 164 to 262 µs | 2.1 to 3.3 ms |
| tracing | 2.8 to 3.8 µs | 131 to 164 µs | 492 to 524 µs | 3.2 to 3.9 ms |
| rtlog | 0.29 to 0.32 µs | 0.64 to 0.70 µs | 20 µs | 0.5 to 1.0 ms |

Ranges are over the three rounds; the values are bucket bounds of a histogram with eight buckets per power of two. For 99 % of requests the rtlog call stays under 0.7 µs, where the text loggers take 74 to 164 µs: they format, take a lock that four worker threads share, and (`log`) make a system call. The rare long calls of rtlog (20 µs at 99.9 %, up to 1 ms) are the worker thread being descheduled in the middle of the call: 37 busy threads share 4 CPUs here, and that affects every variant. rtlog itself never waits: a full ring drops the record and counts it (0 dropped in every run).

### Throughput, latency, memory

| Access log | Requests/s | Request p50 µs | p99 µs | p99.9 µs | Log call ns: mean | p50 | p99 | p99.9 | longest | Log bytes/request | Peak RSS MiB | Dropped |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| none | 35476 | 874 | 1733 | 2953 | 0 | 0 | 0 | 0 | 0 | 0.0 | 13.5 | 0 |
| log | 35163 | 886 | 1683 | 2872 | 9854 | 3328 | 73728 | 196608 | 2672144 | 139.7 | 13.7 | 0 |
| log-buf | 33634 | 926 | 1757 | 2831 | 10576 | 3840 | 81920 | 163840 | 2072321 | 139.7 | 13.4 | 0 |
| tracing | 30936 | 998 | 2077 | 3891 | 11012 | 3072 | 131072 | 491520 | 3888253 | 145.7 | 17.3 | 0 |
| rtlog | 38874 | 800 | 1545 | 2425 | 388 | 288 | 640 | 20480 | 469956 | 54.9 | 14.4 | 0 |
| log | 33697 | 913 | 1860 | 4652 | 10397 | 3328 | 81920 | 294912 | 4836328 | 139.7 | 13.5 | 0 |
| log-buf | 33899 | 913 | 1816 | 3356 | 10891 | 3584 | 81920 | 262144 | 2937855 | 139.7 | 13.4 | 0 |
| tracing | 31423 | 982 | 2033 | 3134 | 10397 | 2816 | 131072 | 524288 | 3758897 | 145.7 | 17.2 | 0 |
| rtlog | 38059 | 802 | 1689 | 3958 | 391 | 288 | 704 | 20480 | 990538 | 54.9 | 14.1 | 0 |
| none | 36024 | 859 | 1713 | 2756 | 0 | 0 | 0 | 0 | 0 | 0.0 | 14.9 | 0 |
| log-buf | 35496 | 873 | 1707 | 3599 | 9827 | 3584 | 73728 | 212992 | 3251841 | 139.7 | 13.2 | 0 |
| tracing | 30394 | 1018 | 2077 | 3074 | 13623 | 3840 | 163840 | 524288 | 3152585 | 145.7 | 17.2 | 0 |
| rtlog | 39527 | 787 | 1507 | 2791 | 426 | 320 | 704 | 20480 | 499837 | 54.9 | 14.2 | 0 |
| none | 37764 | 820 | 1606 | 3417 | 0 | 0 | 0 | 0 | 0 | 0.0 | 13.8 | 0 |
| log | 33306 | 925 | 1887 | 3698 | 10139 | 3584 | 81920 | 327680 | 20119897 | 139.7 | 14.1 | 0 |

Means of the three rounds:

| Access log | Requests/s | Against none | Log call, mean ns | Peak RSS MiB |
| --- | ---: | ---: | ---: | ---: |
| none | 36421 | | | 14.1 |
| log | 34055 | −6.5 % | 10130 | 13.8 |
| log-buf | 34343 | −5.7 % | 10431 | 13.3 |
| tracing | 30918 | −15.1 % | 11677 | 17.2 |
| rtlog | 38820 | +6.6 % | 402 | 14.2 |

What the numbers say, and what they do not:

- **Throughput:** the text loggers cost 6 to 15 % of the requests. rtlog shows a gain, which a log call cannot produce; the next section finds where it comes from.
- **Memory:** the process peaks at 13 to 15 MiB with no log, `log` or rtlog; the differences between those are within what one run varies. rtlog's rings are 64 KiB per logging thread, here four workers. `tracing` with its non-blocking writer peaks about 3 MiB higher, for its queue of formatted lines.
- **Log volume:** 54.9 bytes per request against 139.7 (145.7 with `tracing`), 2.5 times less, before any compression. All decode to the same fields.
- **Not shown:** a text logger configured without a timestamp, or with a cheaper format, would execute fewer instructions than the defaults measured here. The absolute requests per second are those of a shared VM where the load generator competes with the proxy; they were about 25 % higher on another day with the same binary.

## Why rtlog looks faster than no log

A log call adds work, so the proxy cannot serve more requests because of it. Yet rtlog was above `none` in every round above. A control run separates the two things rtlog adds, the log call and a drain thread that wakes every millisecond: `PINGORA_IDLE_DRAIN=1` with the `none` variant starts the drain thread and logs nothing. Four rounds of 6 s, the three variants in turn:

| Variant | Requests/s in the four rounds | Mean | Against none |
| --- | --- | ---: | ---: |
| none | 35167, 34240, 34230, 33919 | 34389 | |
| none with an idle drain thread | 36144, 34921, 36905, 36449 | 36105 | +5.0 % |
| rtlog | 35492, 35565, 34744, 36040 | 35460 | +3.1 % |

The idle drain thread alone raises throughput, in all four rounds, and by more than rtlog does. So the gain is an effect of one more thread waking every millisecond on this machine (upstream, proxy and 32 load-generating threads share 4 virtual CPUs, and the periodic wake-ups change how the scheduler spreads them), not of logging. Against the idle-drain control, the rtlog log calls cost 1.8 % in the mean, with rtlog below the control in three rounds of four; that is the size the call times predict (0.4 µs out of about 110 µs of worker time per request is 0.4 %) plus noise.

What to take from it: on this setup the throughput column cannot resolve rtlog's cost, and it flatters rtlog. The instruction counts and the call-time distribution are the measurements that carry the comparison. A harness with the load generator on other cores or another machine belongs to roadmap 12b.

## What a full port needs

Pingora 0.9.0 has 552 log call sites through the `log` crate's macros (counted with a script over the sources at the pinned revision):

| Call sites | Count | Port |
| --- | ---: | --- |
| Total | 552 | |
| No placeholder (fixed text) | 223 | Works today: rename the macro |
| Placeholders, `Display` only | 210 | Arguments that are integers, booleans or strings work today; errors, addresses and other `Display` types need the `{:display}` escape hatch (roadmap section 0) |
| At least one `{:?}` | 119 | Need `derive(Log)` or the `{:debug}` escape hatch (roadmap section 0) |
| Using inline captures such as `{key}` | 206 | Need inline-capture support in the macros (roadmap, v0.3), or a rewrite to `"key={}", key` |

So 40 % of the sites port with a rename, and the rest wait for three roadmap items. The next steps of 12a for this reference: port the fixed-text sites and the scalar ones as a patch series on the pinned revision, measure the proxy at debug level (several records per request), then the remaining sites as those features land.
