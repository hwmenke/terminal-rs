//! Offline benchmark fixtures only; generated bars never enter the application.
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tr_core::{AssetClass, Bar, Provenance, Timeframe};
use tr_data::{auction, history::calendar::next_session, intraday};
fn bench(c: &mut Criterion) {
    let mut date = "2026-09-01".parse().unwrap();
    let mut bars = vec![];
    for day in 0..20 {
        for (i, ts) in intraday::regular_opens(date, Timeframe::M1).into_iter().enumerate() {
            let p = 100. + day as f64 * 0.2 + (i as f64 * 0.07).sin();
            bars.push(
                Bar::new(ts, p, p + 0.12, p - 0.09, p + 0.02, 100. + (i % 37) as f64 * 10.)
                    .with_provenance(Provenance::Real),
            );
        }
        date = next_session(date);
    }
    let cut = bars.last().unwrap().ts + 60;
    let build = || {
        auction::build(black_box(&bars), Timeframe::M1, None, AssetClass::Equity, cut, &[], Default::default(), 0.01)
            .unwrap()
    };
    let report = build();
    let sessions: Vec<_> = report.sessions.iter().collect();
    let mut group = c.benchmark_group("auction_20_sessions_7800_bars");
    group.sample_size(30);
    group.bench_function("build", |b| b.iter(build));
    group.bench_function("composite", |b| b.iter(|| auction::composite(black_box(&sessions), 70)));
    group.finish();
}
criterion_group!(benches, bench);
criterion_main!(benches);
