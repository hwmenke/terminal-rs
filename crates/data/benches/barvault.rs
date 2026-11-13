//! Offline storage benchmark. Fabricated inputs exist only in this benchmark;
//! providers and the application never use them as fallback data.
use std::time::Instant;
use tr_core::{Bar, Provenance, Timeframe};
use tr_data::{BarVault, DataMode, vault::SeriesId};
fn main() {
    let mut bars = Vec::new();
    let mut date = chrono::NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
    while bars.len() < 100_000 {
        if tr_data::history::calendar::is_session(date) {
            for ts in tr_data::intraday::regular_opens(date, Timeframe::M1) {
                bars.push(Bar::new(ts, 100., 101., 99., 100., 1000.).with_provenance(Provenance::Real));
            }
        }
        date = date.succ_opt().unwrap();
    }
    bars.truncate(100_000);
    let at = bars.last().unwrap().ts + 7200;
    let id = SeriesId { symbol: "AAPL US Equity".into(), tf: Timeframe::M1, source: "bench-fixture".into() };
    let mut vault = BarVault::open_in_memory().unwrap();
    vault.ingest(&id, &bars, DataMode::Live, at).unwrap();
    let batch = &bars[bars.len() - 2730..];
    let start = Instant::now();
    for _ in 0..5 {
        std::hint::black_box(vault.ingest(&id, batch, DataMode::Live, at).unwrap());
    }
    let ingest = start.elapsed() / 5;
    let start = Instant::now();
    for _ in 0..5 {
        let read = vault.read(&id, None, 20_000).unwrap();
        assert_eq!(read.len(), 20_000);
        std::hint::black_box(read);
    }
    let read = start.elapsed() / 5;
    println!(
        "Bar Vault, in-memory SQLite, five iterations: ingest 2730 over 100000 = {ingest:?}; read 20000 = {read:?}"
    );
}
