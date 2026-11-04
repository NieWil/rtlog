//! Decoder throughput on a captured stream.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use rtlog_decode::{decode, Contract, Output};

fn capture() -> Vec<u8> {
    let mut d = rtlog::Drain::new(Vec::new());
    for chunk in 0..10u32 {
        for i in 0..1_000u32 {
            rtlog::info!(
                "pid out={:.3} err={} step={}",
                0.42f32,
                -3i16,
                chunk * 1_000 + i
            );
        }
        d.poll().unwrap();
    }
    d.into_inner()
}

fn bench(c: &mut Criterion) {
    let stream = capture();
    let contract = Contract::from_jsonl(&rtlog::contract::to_jsonl()).unwrap();
    let mut g = c.benchmark_group("decode");
    g.throughput(Throughput::Elements(10_000));
    g.bench_function("text_10k", |b| {
        b.iter(|| decode(&contract, &stream, Output::Text, false).unwrap())
    });
    g.bench_function("json_10k", |b| {
        b.iter(|| decode(&contract, &stream, Output::Json, false).unwrap())
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
