# rtlog

Real-time deferred logger: lock-free, allocation-free log calls on MCUs, embedded Linux and desktop. Format strings stay out of the stream and are rendered on the host.

Status: v0.1 core and decoder. See [implementation notes](docs/implementation.md) for what is built and how it differs from the [specification](docs/spec.md).

## Quick start

```rust
rtlog::info!("pid out={:.3} err={}", 0.42f32, -3i16);

// Opt-in drain thread writing the binary stream to a file.
let file = std::fs::File::create("app.rtlog")?;
let drain = rtlog::spawn_drain(rtlog::IoTransport(file), std::time::Duration::from_millis(1));
rtlog::contract::write("app.contract.jsonl")?;
// ... run ...
drain.stop();
```

```text
$ rtlog decode --contract app.contract.jsonl app.rtlog
0.000000 [main] INFO  pid out=0.420 err=-3
```

On `no_std`, give each task a static ring and log through its producer:

```rust
static MOTOR: rtlog::Ring<1024> = rtlog::Ring::new();
let (mut p, mut c) = MOTOR.split(1).unwrap();
rtlog::warn!(to: &mut p, "temp={}", 81u8);
```

## Crates

| Crate | Contents |
| --- | --- |
| `crates/rtlog-core` (lib `rtlog`) | Macros, `Log` trait, SPSC rings, clock, drain, contract writer. `no_std`; `std` feature on by default. |
| `crates/rtlog-decode` | Contract loader, stream decoder, text and JSON-lines output, `rtlog` CLI. |
| `examples` | `basic` (threads, file drain, decode), `static_rings` (`no_std` API), `timing` (timing report). |

## Development

```sh
cargo test --workspace                                   # unit + integration tests
cargo llvm-cov --workspace --summary-only                # coverage
cargo run --release -p rtlog-examples --example timing   # timing report (markdown)
cargo bench -p rtlog-core --bench log_call               # Criterion benches
cargo bench -p rtlog-decode --bench decode
cargo build -p rtlog-core --no-default-features --target thumbv7em-none-eabihf
```

CI runs all of these. Tests run on Linux, macOS and Windows. Coverage, the timing report and the benchmark table are posted on each job's summary page and uploaded as artifacts.
