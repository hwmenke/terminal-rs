//! Compute budget on an explicitly supplied real-data export; never fabricates bars.
use std::{hint::black_box, time::Instant};
use tr_core::{AssetClass, Bar, Timeframe};
fn main() {
    let Ok(path) = std::env::var("EDGE_WALL_BARS_JSON") else {
        eprintln!("Set EDGE_WALL_BARS_JSON to a verified regular-session 5m Bar JSON export");
        return;
    };
    let bars: Vec<Bar> = serde_json::from_slice(&std::fs::read(path).expect("read real bars")).expect("Bar JSON");
    let input = tr_data::wall_edge::prepare(
        &bars,
        Timeframe::M5,
        AssetClass::Equity,
        Some(chrono::Utc::now().timestamp()),
        None,
    )
    .map_err(|m| m.detail)
    .expect("verified closed window");
    let started = Instant::now();
    for _ in 0..5 {
        black_box(
            tr_data::wall_edge::compute(black_box(&input.bars), Timeframe::M5, Default::default())
                .expect("Edge report"),
        );
    }
    println!("Wall Edge · {} real closed bars · {:?}/name", input.bars.len(), started.elapsed() / 5);
}
