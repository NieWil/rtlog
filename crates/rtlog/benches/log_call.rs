//! Criterion benches for the log call, the ring round trip and the drain.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use rtlog::{Ring, FRAME_MAX};

struct Discard;
impl rtlog::Transport for Discard {
    fn write(&mut self, _f: &[u8]) -> Result<(), rtlog::Full> {
        Ok(())
    }
}

fn log_call(c: &mut Criterion) {
    let mut drain = rtlog::Drain::new(Discard);
    let mut g = c.benchmark_group("log_call");
    g.throughput(Throughput::Elements(1));
    let mut n = 0u32;
    g.bench_function("w1_scalar", |b| {
        b.iter(|| {
            n = n.wrapping_add(1);
            rtlog::info!("x={} y={}", black_box(n), 7u32);
            if n.is_multiple_of(1024) {
                drain.poll().unwrap();
            }
        })
    });
    g.bench_function("w2_six_fields", |b| {
        b.iter(|| {
            n = n.wrapping_add(1);
            rtlog::info!(
                "a={} b={} c={} d={} e={} f={}",
                black_box(n),
                2u16,
                -3i32,
                0.5f32,
                true,
                9u64
            );
            if n.is_multiple_of(1024) {
                drain.poll().unwrap();
            }
        })
    });
    let s = "0123456789abcdef0123456789abcdef";
    g.bench_function("w3_string32", |b| {
        b.iter(|| {
            n = n.wrapping_add(1);
            rtlog::info!("name={}", black_box(s));
            if n.is_multiple_of(1024) {
                drain.poll().unwrap();
            }
        })
    });
    rtlog::set_level(rtlog::Level::Warn);
    g.bench_function("disabled", |b| {
        b.iter(|| rtlog::debug!("x={}", black_box(1u32)))
    });
    rtlog::set_level(rtlog::Level::Trace);
    g.finish();
}

fn ring(c: &mut Criterion) {
    static RING: Ring<4096> = Ring::new();
    let (mut p, mut cons) = RING.split(1).unwrap();
    let mut out = [0u8; FRAME_MAX];
    let mut g = c.benchmark_group("ring");
    g.throughput(Throughput::Elements(1));
    g.bench_function("push_pop_10B", |b| {
        b.iter(|| {
            p.push_at(1, 5, black_box(&[0u8; 10]));
            black_box(cons.pop(&mut out));
        })
    });
    g.bench_function("explicit_producer_w1", |b| {
        b.iter(|| {
            rtlog::info!(to: &mut p, "x={} y={}", black_box(1u32), 7u32);
            cons.pop(&mut out);
        })
    });
    g.finish();
}

fn drain(c: &mut Criterion) {
    let mut g = c.benchmark_group("drain");
    g.throughput(Throughput::Elements(1_000));
    let mut d = rtlog::Drain::new(Discard);
    g.bench_function("poll_1000_frames", |b| {
        b.iter_batched(
            || {
                for i in 0..1_000u32 {
                    rtlog::info!("x={} y={}", i, 7u32);
                }
            },
            |()| d.poll().unwrap(),
            BatchSize::PerIteration,
        )
    });
    g.finish();
}

criterion_group!(benches, log_call, ring, drain);
criterion_main!(benches);
