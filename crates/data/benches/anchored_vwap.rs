//! Anchored VWAP and Time Link cost on a large 1-minute series. Benchmark input only:
//! the app never computes AVWAP on generated bars (sample bars are refused).
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tr_core::{Timeframe, sample};
use tr_data::{anchored_vwap, time_link};

fn bench(c: &mut Criterion) {
    let bars = sample::generate("BENCH", Timeframe::M1, 100_000, 1_790_000_000);
    let n = bars.len();
    let anchors: Vec<i64> = [n / 10, n / 3, n / 2, n - 500].iter().map(|&i| bars[i].ts).collect();
    let mut g = c.benchmark_group("cockpit_100k_1m_bars");
    g.sample_size(20);
    g.bench_function("avwap_4_anchors", |b| {
        b.iter(|| anchored_vwap::build(black_box(&bars), black_box(&anchors), n - 1))
    });
    let span = (bars[n / 2].ts, bars[n / 2].ts + 900);
    g.bench_function("time_link_bars_in_span", |b| {
        b.iter(|| time_link::bars_in_span(black_box(&bars), Timeframe::M1, true, black_box(span)))
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
