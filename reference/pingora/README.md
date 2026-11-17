# Reference application: Pingora

[Pingora](https://github.com/cloudflare/pingora) is Cloudflare's HTTP proxy framework in Rust (Apache-2.0). It is the first reference application of the [roadmap](../../docs/roadmap.md) (section 12a): a program rtlog's authors did not write, where the log is essential (a proxy is operated from its access log) and where the logger sits on the latency path of every request.

This directory is the first step of the port: Pingora's own code is unchanged, and the **access log**, the one record a proxy writes per request, goes through the `log` crate or through rtlog. It answers one question: what does the logger add to a request?

Pinned to Pingora 0.9.0, revision `3f87e92c` (in `Cargo.toml`). The crate is outside the host workspace, like `size/mcu`.

## Run it

```sh
reference/pingora/run.sh            # 10 s per run, 32 connections, 4 proxy threads, 3 rounds
reference/pingora/run.sh 3 16 2 1   # a short check
```

One binary holds an upstream HTTP server, the Pingora proxy in front of it and a load generator (keep-alive connections that send one request after the other and time each). The first build takes about two minutes. The script runs the three variants in turn, then checks that the text log and the decoded rtlog capture both hold the access records.

| Variant | Access log |
| --- | --- |
| `none` | no access log: the proxy alone |
| `log` | `log::info!` with `env_logger` writing text to a file, one `write` per record, as with its default stderr target |
| `rtlog` | `rtlog::info!` into the worker thread's ring; a drain thread writes the binary frames to a file every millisecond |

Both loggers get the same six fields: method, path, status, body bytes, peer port, duration.

## Result

Intel Xeon at 2.30 GHz, 4 virtual CPUs of a shared cloud VM, Linux 6.18, rustc 1.97.0, release build with thin LTO. 8 s per run, 32 connections, 4 proxy threads, 3 rounds; upstream, proxy and load generator share the 4 CPUs.

| Access log | Requests/s | p50 µs | p99 µs | p99.9 µs | Log call, mean ns | Log bytes/request | Dropped |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| none | 46241 | 672 | 1323 | 2321 | 0 | 0.0 | 0 |
| log | 42902 | 725 | 1404 | 2545 | 9681 | 139.7 | 0 |
| rtlog | 51498 | 593 | 1245 | 2282 | 252 | 54.9 | 0 |
| none | 47097 | 660 | 1294 | 1919 | 0 | 0.0 | 0 |
| log | 41923 | 736 | 1485 | 2666 | 9688 | 139.7 | 0 |
| rtlog | 47193 | 652 | 1333 | 2678 | 286 | 54.9 | 0 |
| none | 45026 | 689 | 1348 | 2436 | 0 | 0.0 | 0 |
| log | 41375 | 745 | 1519 | 3163 | 10038 | 139.7 | 0 |
| rtlog | 47885 | 645 | 1286 | 2521 | 274 | 54.9 | 0 |

What the numbers say, and what they do not:

- **The log call itself:** about 9.7 µs through `log` and `env_logger`, about 270 ns through rtlog, measured inside the proxy around the call (the `Instant` pair adds about 20 ns to both). The `log` figure is formatting plus one `write` system call per record, with four worker threads taking turns on the file. rtlog copies two short strings and four integers into the thread's ring.
- **Throughput:** with the `log` access log the proxy serves 7 to 11 % fewer requests than with none, in every round. With rtlog the difference from no logging is inside the noise of this machine: rtlog is above `none` in all three rounds, which a logger cannot cause. Read it as "not measurable here", not as a gain. See the next section.
- **Log volume:** 54.9 bytes per request against 139.7, 2.5 times less, before any compression. Both logs decode to the same fields.
- **Not shown:** a buffered or asynchronous `log` backend would cost less per call than `env_logger`'s direct writes; that comparison belongs to the logger comparison of roadmap section 12b. The numbers come from a shared VM and vary by several percent between rounds; the three rounds are listed rather than averaged for that reason.

## Why rtlog is not faster than no log

A log call adds work, so the proxy cannot serve more requests with it than without. The table above shows more only because two runs of the same variant differ by more than rtlog's cost. A second experiment, six rounds of 6 s with the variants in every possible order, on the same machine:

| Access log | Requests/s, lowest to highest of 6 runs | Mean | Against none |
| --- | ---: | ---: | ---: |
| none | 41525 to 45017 | 43666 | |
| log | 38636 to 44056 | 40859 | −6.4 % |
| rtlog | 42482 to 48510 | 44840 | +2.7 % |

Runs with no access log alone spread over 8 % (41525 to 45017). rtlog was above `none` in four rounds and below it in two. Its cost in this workload can be estimated from the call time instead: 270 ns per request on one of four worker threads that each spend about 85 µs per request, about 0.3 %, far below what this setup can resolve.

Where the spread comes from: the upstream, the proxy and 32 load-generating threads share 4 virtual CPUs of a shared VM, so the scheduler, not the proxy, decides much of each run. Resolving a 0.3 % difference needs the load generator on other cores or another machine, pinned proxy threads, and many more rounds; that is part of the comparison harness of roadmap 12b.

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
