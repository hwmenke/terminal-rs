use crate::{Case, fixtures};
use std::{cell::RefCell, hint::black_box as bb, rc::Rc};
use tr_chart::{
    gpu::{LineKind, Style},
    scene::{self, ChartType, Overlay, PricePane, SeriesGpu},
    viewport::Viewport,
};
use tr_core::{AssetClass, Timeframe, indicators};
use tr_data::{
    BarVault, DataMode, anchored_vwap, auction, backtest, backtest_honesty, edge, nulls, session_clock, stable_hash,
    stop_cloud, time_link, trend_cone, tstack_tune, vault::SeriesId, wall_edge,
};
#[derive(Clone, Copy)]
pub enum Scale {
    Full,
    Smoke,
}
pub fn all(scale: Scale) -> Vec<Case> {
    let full = matches!(scale, Scale::Full);
    // Smoke retains admission minimums and exercises exactly the same APIs.
    let m1 = Rc::new(fixtures::intraday(Timeframe::M1, if full { 60 } else { 6 }, 1));
    let m5 = Rc::new(fixtures::intraday(Timeframe::M5, if full { 60 } else { 8 }, 2));
    let d = Rc::new(fixtures::daily(if full { 5000 } else { 500 }, 3));
    let clock = fixtures::intraday(Timeframe::M30, if full { 60 } else { 20 }, 4);
    let auc = fixtures::intraday(Timeframe::M1, if full { 20 } else { 4 }, 5);
    let cut = m1.last().unwrap().ts + 7200;
    let id = Rc::new(SeriesId { symbol: "AAPL US Equity".into(), tf: Timeframe::M1, source: "bench-fixture".into() });
    let vault = Rc::new(RefCell::new(BarVault::open_in_memory().unwrap()));
    vault.borrow_mut().ingest(&id, &m1, DataMode::Live, cut).unwrap();
    let mut cases = vec![];
    macro_rules! add {
        ($id:literal,$f:expr) => {
            cases.push(Case { id: $id, run: Box::new($f) });
        };
    }
    let (v, i) = (vault.clone(), id.clone());
    add!("data.vault_read_5k", move || bb(v.borrow().read(&i, None, 5000).unwrap()).len());
    let (v, i) = (vault.clone(), id.clone());
    add!("data.vault_stitch_7800", move || bb(v.borrow().stitch(&i, &[], 7800, None).unwrap()).0.len());
    let (v, i, b) = (vault, id, m1.clone());
    add!("data.vault_ingest_390", move || {
        let o = bb(v.borrow_mut().ingest(&i, &b[b.len() - 390..], DataMode::Live, cut).unwrap());
        o.inserted + o.confirmed + o.corrected
    });
    let b = m1.clone();
    add!("data.bars_digest_23k", move || {
        bb(stable_hash::bars_digest(bb(&b)));
        b.len()
    });
    let b = m1.clone();
    add!("data.series_gpu_23k", move || bb(SeriesGpu::build(bb(&b))).len());
    let series = Rc::new(SeriesGpu::build(&m1));
    let closes = m1.iter().map(|b| b.close).collect::<Vec<_>>();
    let overlays = Rc::new(vec![
        Overlay {
            line: scene::line_ref(&indicators::ema(&closes, 20)),
            color: [0.0, 0.6, 1.0, 1.0],
            kind: LineKind::Line,
            scale: 1.0,
        },
        Overlay {
            line: scene::line_ref(&indicators::sma(&closes, 50)),
            color: [1.0, 0.0, 1.0, 1.0],
            kind: LineKind::Line,
            scale: 1.0,
        },
    ]);
    for (id, px) in [("scene.price_pane_all", 1600.0 / m1.len() as f64), ("scene.price_pane_200", 8.0)] {
        let (s, o) = (series.clone(), overlays.clone());
        let mut v = Viewport { right: m1.len() as f64, bar_px: px, width_px: 1600.0 };
        let mut direction = 1.0;
        cases.push(Case {
            id,
            run: Box::new(move || {
                v.pan_px(direction * 3.0);
                direction = -direction;
                bb(scene::price_pane(&PricePane {
                    series: bb(&s),
                    chart_type: ChartType::Candles,
                    show_volume: true,
                    overlays: &o,
                    viewport: &v,
                    height_px: 900.0,
                    log: false,
                    manual_scale: None,
                    style: Style::default(),
                }))
                .cmds
                .len()
            }),
        });
    }
    let b = m5.clone();
    add!("stack.trend_stack_5k", move || bb(indicators::trend_stack(
        bb(&b),
        Timeframe::M5,
        vec![],
        Default::default()
    )
    .unwrap())
    .base_len);
    let stack = indicators::trend_stack(&d, Timeframe::D1, vec![], Default::default()).unwrap();
    let states =
        stack.medium.iter().zip(&stack.confidence).map(|(&s, c)| c.is_finite().then_some(s)).collect::<Vec<_>>();
    let b = d.clone();
    add!("edge.regime_edge_5k", move || bb(edge::regime_edge(
        bb(&b),
        &states,
        &stack.entry_long,
        &stack.entry_short,
        &[1, 5, 20]
    )
    .unwrap())
    .completed
    .len());
    let b = m5.clone();
    add!("edge.wall_edge_5k", move || bb(wall_edge::compute(bb(&b), Timeframe::M5, Default::default()).unwrap())
        .cells
        .iter()
        .filter(|c| c.stats.is_some())
        .count());
    let ps = (0..if full { 10000 } else { 1000 }).map(|i| ((i * 7919) % 10000) as f64 / 10000.0).collect::<Vec<_>>();
    add!("nulls.bh_10k", move || bb(nulls::benjamini_hochberg(bb(&ps))).len());
    add!("nulls.sign_test", move || {
        bb(nulls::sign_test(bb(2400), bb(2600)));
        1
    });
    let now = clock.last().unwrap().ts + 1800;
    add!("clock.build_60d_30m", move || bb(session_clock::build_clock(
        "AAPL US Equity",
        bb(&clock),
        &session_clock::ClockQuery { timeframe: Timeframe::M30 },
        "fixture",
        now,
        Some(now)
    )
    .unwrap())
    .tested);
    let or = fixtures::or_lens(if full { 120 } else { 93 });
    let observed = or.last().unwrap().ts + 1800;
    let now = observed + 86400;
    add!("studio.or_lens_120d_30m", move || {
        let r = bb(tr_data::studio_season::opening_range_lens(
            "AAPL US Equity",
            Timeframe::M30,
            bb(&or),
            "benchmark fixture only",
            observed,
            now,
            &[],
        )
        .unwrap());
        assert_eq!(r.cells.len(), 117);
        assert_eq!(r.opening_sessions, if full { 120 } else { 93 });
        let c = r.cell(0, 1, 0);
        assert!(c.selected.len() >= 30 && c.complement.len() >= 60);
        assert!(c.inference.p.is_some() && c.inference.interval.is_some());
        assert!(r.cell(1, 1, 1).inference.p.is_some());
        assert!(r.cell(2, 1, 2).inference.p.is_some());
        assert!(r.cell(0, 12, 0).all_mean.is_none());
        r.cells.iter().map(|c| c.selected.len()).sum()
    });
    let season = fixtures::season_evidence(if full { 120 } else { 40 });
    let observed = season.last().unwrap().ts + 300;
    add!("studio.season_evidence_120d_5m", move || {
        let r = bb(tr_data::studio_season::compute_at("AAPL US Equity", Timeframe::M5, bb(&season), "benchmark fixture only", observed, observed).unwrap());
        let e = r.evidence.as_ref().unwrap();
        let sessions = if full {120} else {40};
        assert_eq!((r.bars,e.expected_bars,e.sessions,e.grid_sessions), (sessions*78,sessions*78,sessions,sessions));
        assert_eq!(e.family_size(),468);
        assert_eq!(e.closed_thru,observed);
        assert!(e.cells[..78].iter().all(|c| c.inference.n==sessions && c.inference.interval.is_some() && c.inference.p.is_some()));
        assert!(e.cells[78..].iter().all(|c| c.inference.n<30 && c.q==1. && c.inference.p.is_none()));
        assert!(e.cells.iter().all(|c| c.q.is_finite() && (0. ..=1.).contains(&c.q)));
        e.cells.iter().map(|c|c.inference.n).sum()
    });
    let by_ps = (0..117).map(|i| if i % 4 == 0 { 1. } else { (i as f64 + 1.) / 1170. }).collect::<Vec<_>>();
    add!("nulls.by_117", move || {
        let q = bb(nulls::benjamini_yekutieli(bb(&by_ps)));
        assert_eq!(q.len(), 117);
        assert!(q.iter().all(|q| q.is_finite() && (0. ..=1.).contains(q)));
        q.len()
    });
    let b = d.clone();
    let now = b.last().unwrap().ts + 86400;
    add!("stack.tune_7x7_5k", move || {
        let r = bb(tstack_tune::compute(
            bb(&b),
            Timeframe::D1,
            AssetClass::Equity,
            Some(now),
            Default::default(),
            "fixture",
            &[],
        )
        .unwrap());
        assert_eq!(r.cells.len(), 49);
        r.eligible
    });
    let b = m1.clone();
    let anchors = [0, 1, 2, 3].map(|i| b[i * b.len() / 5].ts);
    add!("avwap.report_23k_x4", move || {
        let r = bb(anchored_vwap::report(bb(&b), &anchors, b.len()).unwrap());
        assert_eq!(r.lines.len(), 4);
        assert!(r.lines.iter().all(|r| r.as_ref().is_ok_and(|l| l.used > 0)));
        r.closed
    });
    let b = m1;
    let span = (b[100].ts, b[200].ts);
    add!("avwap.time_link_span", move || bb(time_link::bars_in_span(bb(&b), Timeframe::M1, true, span)).len());
    let now = auc.last().unwrap().ts + 60;
    let r = auction::build(&auc, Timeframe::M1, None, AssetClass::Equity, now, &[], Default::default(), 0.01).unwrap();
    let summary_bars = auc.clone();
    let receipt = fixtures::stitch(&summary_bars, Timeframe::M1);
    add!("chart.session_summary_7800", move || {
        let r = bb(tr_data::chart_session::build(
            bb(&summary_bars),
            Timeframe::M1,
            AssetClass::Equity,
            false,
            Some(now),
            Some(&receipt),
        )
        .unwrap());
        assert!(r.stats.vwap.is_some() && r.stats.relative_volume.is_some());
        assert!(r.opening.is_some() && r.prior.is_some() && r.value_area.is_some());
        assert_eq!(r.coverage.len(), if full { 20 } else { 4 });
        assert_eq!(r.coverage.iter().map(|s| s.hi - s.lo).sum::<usize>(), summary_bars.len());
        r.bars
    });
    let profiles = r.sessions.clone();
    add!("auction.concentration_20", move || {
        assert_eq!(profiles.len(), if full { 20 } else { 4 });
        for s in &profiles {
            let c = bb(auction::concentration(bb(&s.volume)).unwrap());
            assert!(c.occupied > 0 && c.effective_rows.is_finite() && (0. ..=1.).contains(&c.normalized));
        }
        profiles.len()
    });
    add!("auction.build_20x390", move || bb(auction::build(
        bb(&auc),
        Timeframe::M1,
        None,
        AssetClass::Equity,
        now,
        &[],
        Default::default(),
        0.01
    )
    .unwrap())
    .sessions
    .len());
    add!("auction.composite_20", move || {
        let sessions = r.sessions.iter().collect::<Vec<_>>();
        bb(auction::composite(bb(&sessions), 70).unwrap()).volume.len()
    });
    let b = m5.clone();
    let now = b.last().unwrap().ts + 300;
    add!("stops.report_5k", move || bb(stop_cloud::report(
        bb(&b),
        Timeframe::M5,
        AssetClass::Equity,
        Some(now),
        Default::default(),
        Default::default()
    )
    .unwrap())
    .closed);
    let b = m5;
    add!("cone.report_5k", move || {
        let r = bb(trend_cone::report(
            bb(&b),
            Timeframe::M5,
            AssetClass::Equity,
            Some(now),
            Default::default(),
            Default::default(),
        )
        .unwrap());
        assert!(r.conditional.paths > 0);
        r.closed
    });
    let b = fixtures::daily(if full { 1260 } else { 500 }, 6);
    add!("bt.report_5y", move || bb(backtest::compute_report(
        "AAPL US Equity",
        "AAPL",
        bb(&b),
        &Default::default(),
        "fixture"
    )
    .unwrap())
    .results
    .len());
    let tapes = fixtures::returns_family(if full { 64 } else { 8 }, if full { 1260 } else { 126 }, 7);
    add!("bt.dsr_family_64", move || bb(backtest_honesty::compare_family(bb(&tapes)).unwrap()).comparisons.len());
    cases
}
