# Reference application: Pingora

[Pingora](https://github.com/cloudflare/pingora) is Cloudflare's HTTP proxy framework in Rust (Apache-2.0). It is the first reference application of the [roadmap](../../docs/roadmap.md) (section 12a): a program rtlog's authors did not write, where the log is essential (a proxy is operated from its access log) and where the logger sits on the latency path of every request.

This directory is the first step of the port: Pingora's own code is unchanged, and the **access log**, the one record a proxy writes per request, goes through the `log` crate, through `tracing`, or through rtlog. It answers one question: what does the logger add to a request, in instructions, time and memory?

Pinned to Pingora 0.9.0, revision `3f87e92c` (in `Cargo.toml`). The crate is outside the host workspace, like `size/mcu`.

## Run it

```sh
reference/pingora/run.sh            # 10 s per run, 32 connections, 4 proxy threads, 3 rounds
reference/pingora/run.sh 3 16 2 1   # a short check
```

One binary holds an upstream HTTP server, the Pingora proxy in front of it and a load generator (keep-alive connections that send one request after the other and time each). The first build takes about three minutes. The script runs the variants in turn, each round starting with another one, counts the instructions of the access-log call with callgrind if valgrind is installed, then checks that the text log and the decoded rtlog capture both hold the access records.

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

Counted with callgrind inside the one function that makes the log call, on the worker thread. This is the number to rely on: it is the same on every run and does not depend on the machine's load.

| Access log | Instructions per call | Against rtlog |
| --- | ---: | ---: |
| log | 9987 | 38× |
| log-buf | 10071 | 38× |
| tracing | 9670 | 37× |
| rtlog | 264 | 1× |

Buffering the file (`log-buf`) or moving the write to another thread (`tracing`) changes almost nothing here: about 10 000 instructions go into formatting the line on the worker thread, timestamp included, and that part stays. rtlog copies two short strings and four integers into the thread's ring; the 264 instructions include creating that ring on a thread's first call, spread over the run.

### Throughput, latency, memory

| Access log | Requests/s | p50 µs | p99 µs | p99.9 µs | Log call, mean ns | Log bytes/request | Peak RSS MiB | Dropped |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| none | 35524 | 874 | 1692 | 2836 | 0 | 0.0 | 13.5 | 0 |
| log | 34005 | 910 | 1744 | 3453 | 10482 | 139.7 | 13.4 | 0 |
| log-buf | 33504 | 926 | 1811 | 3616 | 11593 | 139.7 | 13.2 | 0 |
| tracing | 29899 | 1030 | 2161 | 4087 | 12844 | 145.7 | 18.0 | 0 |
| rtlog | 37846 | 822 | 1556 | 3022 | 383 | 54.9 | 13.9 | 0 |
| log | 33224 | 925 | 1900 | 5653 | 11583 | 139.7 | 13.2 | 0 |
| log-buf | 33799 | 919 | 1808 | 3725 | 10862 | 139.7 | 13.5 | 0 |
| tracing | 32436 | 956 | 1900 | 3029 | 11761 | 145.7 | 18.0 | 0 |
| rtlog | 36703 | 843 | 1676 | 3632 | 399 | 54.9 | 14.8 | 0 |
| none | 37120 | 842 | 1542 | 2618 | 0 | 0.0 | 14.4 | 0 |
| log-buf | 34590 | 899 | 1712 | 3302 | 11320 | 139.7 | 13.6 | 0 |
| tracing | 32407 | 958 | 1879 | 3299 | 10378 | 145.7 | 17.4 | 0 |
| rtlog | 38812 | 795 | 1620 | 2942 | 398 | 54.9 | 15.1 | 0 |
| none | 35939 | 861 | 1775 | 3307 | 0 | 0.0 | 13.8 | 0 |
| log | 34158 | 911 | 1748 | 3193 | 11023 | 139.7 | 13.2 | 0 |

Means of the three rounds:

| Access log | Requests/s | Against none | Log call, mean ns | Peak RSS MiB |
| --- | ---: | ---: | ---: | ---: |
| none | 36194 | | | 13.9 |
| log | 33796 | −6.6 % | 11029 | 13.3 |
| log-buf | 33964 | −6.2 % | 11258 | 13.4 |
| tracing | 31581 | −12.7 % | 11661 | 17.8 |
| rtlog | 37787 | +4.4 % | 393 | 14.6 |

What the numbers say, and what they do not:

- **The log call itself:** about 11 µs through the text loggers and about 0.4 µs through rtlog, measured inside the proxy around the call while all four CPUs are busy (the `Instant` pair adds about 20 ns to each). The ratio agrees with the instruction counts.
- **Throughput:** the text loggers cost 6 to 13 % of the requests. rtlog shows no cost; see the next section for why it shows a gain it cannot have.
- **Memory:** the process peaks at 13 to 15 MiB with no log, `log` or rtlog; the differences between those are within what one run varies. rtlog's rings are 64 KiB per logging thread, here four workers. `tracing` with its non-blocking writer peaks about 4 MiB higher, for its queue of formatted lines.
- **Log volume:** 54.9 bytes per request against 139.7 (145.7 with `tracing`), 2.5 times less, before any compression. All decode to the same fields.
- **Not shown:** a text logger configured without a timestamp, or with a cheaper format, would execute fewer instructions than the defaults measured here. The absolute requests per second are those of a shared VM where the load generator competes with the proxy; they were about 25 % higher on another day with the same binary.

## Why rtlog is not faster than no log

A log call adds work, so the proxy cannot serve more requests with it than without. The tables show more only because two runs of the same variant differ by more than rtlog's cost: the three `none` runs above span 35524 to 37120 requests per second, 4 %. In an earlier experiment with six rounds in every order, runs with no access log alone spread over 8 % (41525 to 45017), and rtlog was above `none` in four rounds and below it in two.

rtlog's cost in this workload follows from the instruction count instead: 264 instructions per request, against the several hundred thousand a proxied request executes, is well under 1 %, far below what this setup can resolve. Where the spread comes from: the upstream, the proxy and 32 load-generating threads share 4 virtual CPUs of a shared VM, so the scheduler, not the proxy, decides much of each run. Resolving a difference that small needs the load generator on other cores or another machine, pinned proxy threads and many more rounds; that is part of the comparison harness of roadmap 12b.

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
