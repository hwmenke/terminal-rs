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
    let expected_canvas_n=if full {7800}else{1560};
    let canvas=fixtures::canvas_inputs(if full {20}else{4});
    let canvas_spec=tr_data::studio_canvas::CanvasSpec {mode:tr_data::studio_canvas::CompareMode::Z,base_ts:canvas[0].bars[0].ts,..Default::default()};
    let strip_line=tr_data::studio_canvas::compute(&canvas,&tr_data::studio_canvas::CanvasSpec::default(),None,None,&[]).unwrap().lines.remove(0);
    let events=fixtures::canvas_events(&canvas,2000);
    let event_ts=strip_line.ts.clone();
    add!("studio.canvas_4x7800",move || {
        let r=bb(tr_data::studio_canvas::compute(bb(&canvas),&canvas_spec,None,None,&[]).unwrap());
        assert_eq!(r.lines.len(),4);
        assert!(r.lines.iter().all(|l|l.ts.len()==expected_canvas_n));
        let n=r.lines.iter().map(|l|l.values.iter().flatten().count()).sum::<usize>();
        assert_eq!(n,4*(expected_canvas_n-63));
        assert!(r.labels.iter().all(|l|l.unavailable.is_none()&&l.prior_n==63&&l.prior_sd.is_some()));
        assert_eq!(r.strip.as_ref().unwrap().counts.iter().sum::<usize>(),r.strip.as_ref().unwrap().n);
        n
    });
    add!("chart.margin_strip_7800",move || {
        let r=bb(tr_data::studio_canvas::margin_distribution(bb(&strip_line),None));
        assert_eq!(r.n,expected_canvas_n);
        assert!(r.percentile.is_some());assert_eq!(r.counts.iter().sum::<usize>(),r.n);r.n
    });
    add!("studio.events_2000",move || {
        let r=bb(tr_data::studio_canvas::align_events(bb(&events),bb(&event_ts),Timeframe::M1,true));
        assert_eq!(r.len(),2000);assert!(r.iter().all(|e|e.hi>e.lo));
        assert!(r.iter().any(|e|matches!(e.event.time,tr_data::studio_canvas::EventTime::Date {..})));r.len()
    });
    // Studio slice 3: When report, its session bootstrap and selected/rest subset.
    let when_sessions = if full { 120 } else { 40 };
    let when_input = fixtures::when_input(when_sessions);
    let when_spec = tr_data::studio_when::WhenSpec::parse("RSI(14)<45", 0.).unwrap();
    let when_report = tr_data::studio_when::compute(&when_input, &when_spec).unwrap();
    add!("studio.when_120d_5m", move || {
        let r = bb(tr_data::studio_when::compute(bb(&when_input), &when_spec).unwrap());
        // Every expected regular-session bar (half days included) is on the grid.
        assert_eq!(r.mask.ts.len(), when_input.bars.len());
        assert!(r.mask.ts.len() > when_sessions * 77);
        assert_eq!(r.sessions, when_sessions);
        assert_eq!((r.cells.len(), r.buckets.len()), (4, 20));
        assert!(r.mask.values.iter().flatten().filter(|v| **v == 1.).count() > 300, "rule must match");
        for (c, h) in r.cells.iter().zip(tr_data::studio_when::HORIZONS) {
            let complete = when_input.bars.len() - when_sessions * h;
            assert!(c.raw_n <= complete && c.raw_n + 20 >= complete, "completed in-session outcomes after warm-up");
            assert!(c.mean_contrast.p.is_some() && c.hit_contrast.p.is_some(), "inference available at h={h}");
            assert!((0. ..=1.).contains(&c.q_mean) && (0. ..=1.).contains(&c.q_hit));
        }
        assert!(r.edges.iter().flatten().count() > when_sessions * 70);
        r.cells.iter().map(|c| c.raw_n).sum()
    });
    add!("studio.when_bootstrap_120d_x499", move || {
        let b = bb(when_report.bootstrap());
        assert_eq!((b.draws, b.sessions), (tr_data::studio_when::DRAWS, when_sessions));
        assert!(b.probability.iter().all(|p| p.is_some_and(|(a, z)| a.is_finite() && a <= z)));
        assert!(b.unavailable.iter().all(Option::is_none));
        b.draws * b.sessions
    });
    // UX cleanup slice 1: default 10Y window over a >20,000-session daily history (SPX-like depth).
    let when_daily_n = if full { 24_841 } else { 20_500 };
    let when_daily = {
        let bars = fixtures::daily(when_daily_n, 1_234);
        tr_data::studio_canvas::CanvasInput {
            symbol: "AAPL US Equity".into(),
            tf: Timeframe::D1,
            observed_at: Some(bars.last().unwrap().ts + 3 * 86_400),
            stitch: None,
            source: "benchmark fixture only".into(),
            bars: std::sync::Arc::new(bars),
        }
    };
    let when_daily_spec = tr_data::studio_when::WhenSpec::parse("RSI(14)<45", 0.).unwrap();
    add!("studio.when_daily_10y_of_24k", move || {
        let r = bb(tr_data::studio_when::compute(bb(&when_daily), &when_daily_spec).unwrap());
        let w = r.window.as_ref().expect("daily window");
        assert!(when_daily.bars.len() > 20_000, "history exceeds the 20,000-session grid limit");
        assert_eq!(w.period, tr_data::studio_when::WhenPeriod::Years(10));
        assert_eq!(w.warmup_bars, tr_data::studio_when::WARMUP_BARS);
        assert!(r.sessions > 2_400 && r.sessions < 2_700, "ten calendar years of sessions");
        assert_eq!(r.mask.ts[0], w.scored_from, "warm-up never enters scored rows");
        assert!(r.mask.values.iter().flatten().filter(|v| **v == 1.).count() > 100, "rule must match");
        for c in &r.cells {
            assert!(c.raw_n > 2_000 && c.raw_n < r.sessions, "windowed completed outcomes at h={}", c.horizon);
            assert!(c.mean_contrast.p.is_some() && c.hit_contrast.p.is_some());
        }
        r.cells.iter().map(|c| c.raw_n).sum()
    });
    let subset_bars = fixtures::daily(if full { 5001 } else { 501 }, 17);
    let subset_dist = tr_data::studio_dist::compute(
        "AAPL US Equity",
        &subset_bars,
        Timeframe::D1,
        tr_data::studio_dist::Window::All,
        "benchmark fixture only",
        None,
    )
    .unwrap();
    let third = subset_bars.len() / 3;
    let subset_span = (subset_bars[third].ts, subset_bars[2 * third].ts);
    add!("studio.subset_5k", move || {
        let s = bb(tr_data::studio_when::subset_from_dist(bb(&subset_dist), subset_span).unwrap());
        let n = s.distribution.n;
        assert_eq!(n[0] + n[1], subset_dist.returns.len());
        assert!(n[0] >= third - 1 && n[1] >= 2 * third - 2);
        for g in 0..2 {
            assert!((s.distribution.mass[g].iter().sum::<f64>() - 1.).abs() < 1e-9);
        }
        assert!(s.distance_interval.is_some_and(|(a, b)| a <= b), "session-block interval");
        (n[0] + n[1]) * s.draws
    });
    let routes = fixtures::studio_routes();
    add!("studio.route_1000", move || {
        use tr_data::studio::{StudioContext, StudioLens};
        let context = StudioContext {
            symbol: "AAPL US Equity".into(),
            tf: Timeframe::M5,
            selected_span: Some((20, 40)),
            as_of: None,
        };
        let mut lens = StudioLens::Backtest;
        let mut visits = [0usize; 5];
        let mut transitions = 0;
        for i in 0..1000 {
            let (next, tf, benchmark) = tr_data::studio::parse_spec(bb(routes[i % 5])).unwrap();
            assert_eq!(tf, context.tf);
            transitions += usize::from(lens.select(next));
            visits[i % 5] += 1;
            bb((&context, lens, benchmark));
        }
        assert_eq!(transitions, 1000);
        assert_eq!(visits, [200; 5]);
        assert!(context.contains(20) && context.contains(30) && !context.contains(40));
        transitions
    });
    let regime_bars = fixtures::regime_walk(if full { 5000 } else { 1500 });
    let regime = tr_data::studio_dist::compute(
        "AAPL",
        &regime_bars,
        Timeframe::D1,
        tr_data::studio_dist::Window::All,
        "benchmark fixture only",
        None,
    )
    .unwrap();
    let shape_report = regime.clone();
    let shape = tr_data::studio_shape::compute(&shape_report, 1, false).unwrap();
    add!("studio.shape_5k", move || {
        let s = bb(tr_data::studio_shape::compute(bb(&shape_report), 1, false).unwrap());
        assert!(
            s.values.len() >= 1500
                && s.bins.len() >= 12
                && s.curves.len() == 128
                && s.river.len() >= 1250
                && s.bandwidth.is_some()
        );
        assert_eq!(s.bins.iter().map(|b| b.2).sum::<usize>(), s.values.len());
        assert!(s.ladder.windows(2).all(|w| w[0] <= w[1]));
        for i in [0, 2, 4, 6] {
            assert!(s.tails[i + 1] >= s.tails[i]);
        }
        s.values.len()
    });
    add!("studio.shape_bootstrap_5k_x500", move || {
        let b = bb(shape.bootstrap());
        assert_eq!(b.draws, 500);
        assert!(b.tails.iter().all(|x| x.is_some_and(|(a, b)| a.is_finite() && b.is_finite() && a <= b)));
        b.draws
    });
    let rotation = regime.regime.rotation_input.clone().unwrap();
    add!("studio.regime_fingerprints_5k", move || {
        let r = bb(tr_data::studio_dist::compute(
            "AAPL",
            bb(&regime_bars),
            Timeframe::D1,
            tr_data::studio_dist::Window::All,
            "benchmark fixture only",
            None,
        )
        .unwrap());
        assert!(r.regime.unavailable.is_none());
        for ridge in &r.regime.ridges {
            assert!(ridge.n >= 300 && ridge.contrast.interval.is_some());
            let mass =
                (ridge.counts.iter().sum::<usize>() + ridge.outside.iter().sum::<usize>()) as f64 / ridge.n as f64;
            assert!((mass - 1.).abs() < 1e-12);
        }
        r.regime.classified
    });
    add!("studio.regime_rotation_5k_x499", move || {
        let r = bb(rotation.compute());
        assert_eq!(r.draws, 499);
        assert!(r.p.iter().all(Option::is_some));
        assert!(r.q[2] <= 0.10, "Stressed rotation q {}", r.q[2]);
        r.draws
    });
    let (v, i) = (vault.clone(), id.clone());
    add!("data.vault_read_5k", move || bb(v.borrow().read(&i, None, 5000).unwrap()).len());
    let (v, i) = (vault.clone(), id.clone());
    add!("data.vault_stitch_7800", move || bb(v.borrow().stitch(&i, &[], 7800, None).unwrap()).0.len());
    let (v, i, b) = (vault, id, m1.clone());
    add!("data.vault_ingest_390", move || {
        let o = bb(v.borrow_mut().ingest(&i, &b[b.len() - 390..], DataMode::Live, cut).unwrap());
        o.inserted + o.confirmed + o.corrected
    });
    {
        // Daily Vault (deterministic constructed inputs; every database is built outside the timed region).
        use tr_data::vault::daily::{
            AdjustmentBasis, DailyClosePolicy, DailyExtent, DailyFetch, DailyIngest, DailyRead, DailySeriesId,
        };
        let n = if full { 25_000 } else { 500 };
        let observed = 1_795_132_800; // 2026-11-20T00:00Z: every fixture row (last 2026-10-30) is closed
        let dsym = "AAPL US Equity";
        let mk = |symbol: &str, bars: Vec<tr_core::Bar>, extent| DailyFetch {
            id: DailySeriesId { symbol: symbol.into(), source: "bench-fixture#quote-ohlcv-v1".into() },
            basis: AdjustmentBasis::ProviderQuoteOhlcvV1,
            close_policy: DailyClosePolicy::UsCashV1,
            fetched_at: observed,
            observed_at: observed,
            upstream_fetched_at: None,
            bars,
            extent,
            full_compare: false,
        };
        let a = fixtures::daily(n, 11);
        let dvault = Rc::new(RefCell::new(BarVault::open_in_memory().unwrap()));
        dvault.borrow_mut().daily_ingest(&mk(dsym, a.clone(), DailyExtent::FullAvailable)).unwrap();
        let did = DailySeriesId { symbol: dsym.into(), source: "bench-fixture#quote-ohlcv-v1".into() };
        let (v, i) = (dvault.clone(), did.clone());
        add!("data.daily_vault_read_25k", move || match v.borrow().daily_read(&i).unwrap() {
            DailyRead::Hit(h) => {
                assert_eq!(h.bars.len(), n);
                assert!(h.bars.iter().all(|b| b.valid_ohlc()));
                h.bars.len()
            }
            DailyRead::Empty => panic!("daily series missing"),
        });
        let (v, tail) = (dvault.clone(), a[a.len() - 10..].to_vec());
        let upd = mk(dsym, tail, DailyExtent::Window);
        add!("data.daily_vault_update_10_of_25k", move || {
            let DailyIngest::Stored(o) = bb(v.borrow_mut().daily_ingest(bb(&upd)).unwrap()) else { panic!("rejected") };
            assert_eq!((o.confirmed, o.inserted, o.revised), (10, 0, 0));
            assert!(o.content_hash != 0);
            o.confirmed
        });
        let many = Rc::new(RefCell::new(BarVault::open_in_memory().unwrap()));
        let small = fixtures::daily(20, 12);
        for k in 0..500 {
            many.borrow_mut()
                .daily_ingest(&mk(&format!("P{k:04} US Equity"), small.clone(), DailyExtent::Window))
                .unwrap();
        }
        let v = many.clone();
        add!("data.daily_vault_snapshot_500", move || {
            let s = bb(v.borrow().daily_summaries(None).unwrap());
            assert_eq!(s.len(), 500);
            assert!(s.iter().all(|x| x.bars == 20 && x.content_hash.is_some()));
            s.len()
        });
        // Alternate between two scalings of the same dates: every iteration is a real revision.
        let scaled: Vec<tr_core::Bar> = a
            .iter()
            .map(|b| tr_core::Bar { open: b.open * 1.5, high: b.high * 1.5, low: b.low * 1.5, close: b.close * 1.5, ..*b })
            .collect();
        // Index 0 = original (active at start), index 1 = scaled; the first iteration applies the scaled one.
        let versions = [mk(dsym, a.clone(), DailyExtent::FullAvailable), mk(dsym, scaled, DailyExtent::FullAvailable)];
        let rvault = Rc::new(RefCell::new(BarVault::open_in_memory().unwrap()));
        rvault.borrow_mut().daily_ingest(&mk(dsym, a.clone(), DailyExtent::FullAvailable)).unwrap();
        let (v, flip) = (rvault, std::cell::Cell::new(1usize));
        add!("data.daily_vault_replace_25k", move || {
            let k = flip.get();
            flip.set(k + 1);
            let DailyIngest::Stored(o) = bb(v.borrow_mut().daily_ingest(bb(&versions[k % 2])).unwrap()) else {
                panic!("rejected")
            };
            assert!(o.replaced && o.revised == n && o.generation as usize >= 2);
            o.revised
        });
    }
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
        let r = bb(tr_data::studio_season::compute_at(
            "AAPL US Equity",
            Timeframe::M5,
            bb(&season),
            "benchmark fixture only",
            observed,
            observed,
        )
        .unwrap());
        let e = r.evidence.as_ref().unwrap();
        let sessions = if full { 120 } else { 40 };
        assert_eq!(
            (r.bars, e.expected_bars, e.sessions, e.grid_sessions),
            (sessions * 78, sessions * 78, sessions, sessions)
        );
        assert_eq!(e.family_size(), 468);
        assert_eq!(e.closed_thru, observed);
        assert!(
            e.cells[..78]
                .iter()
                .all(|c| c.inference.n == sessions && c.inference.interval.is_some() && c.inference.p.is_some())
        );
        assert!(e.cells[78..].iter().all(|c| c.inference.n < 30 && c.q == 1. && c.inference.p.is_none()));
        assert!(e.cells.iter().all(|c| c.q.is_finite() && (0. ..=1.).contains(&c.q)));
        e.cells.iter().map(|c| c.inference.n).sum()
    });
    let n = if full { 5000 } else { 200 };
    let (asset, benchmark) = fixtures::relationship(n);
    add!("studio.corr_hac_5k", move || {
        let r = bb(tr_data::studio_corr::compute(
            "AAPL",
            "QQQ",
            bb(&asset),
            bb(&benchmark),
            252.,
            &[],
            "benchmark fixture only",
        )
        .unwrap());
        assert_eq!(r.ols.n, n);
        assert_eq!(r.robust.fits.len(), 4);
        assert!(r.robust.fits.iter().all(|f| f.intervals.iter().all(Option::is_some)));
        assert_eq!(r.robust.rolling[2].len(), if full { 1000 } else { 141 });
        assert!(r.robust.rolling[2].iter().all(|p| p.interval.is_some()));
        assert!(r.robust.holdout.as_ref().unwrap().r_squared_oos.is_some());
        let sen = r.robust.theil_sen.as_ref().unwrap();
        assert_eq!(sen.n, n.min(2000));
        assert!(sen.slope.is_some() && sen.slopes > 0);
        r.robust.rolling[2].len() + r.ols.n
    });
    // Relate (dev-only `related` fixture: asset log return = 0.8·benchmark + noise). Each case times
    // only its own function; the prerequisites are built once outside the closure.
    {
        use tr_data::studio::Transform;
        use tr_data::studio_corr as sc;
        let (ra, rb) = fixtures::related(n, 0.8, 91);
        let named = |symbol: &str, bars: Vec<tr_core::Bar>| sc::NamedBars {
            symbol: symbol.into(),
            bars,
            source: "benchmark fixture only".into(),
            ..Default::default()
        };
        let (asset, bench) = (named("AAPL US Equity", ra), named("QQQ US Equity", rb));
        let options = sc::Options { annual_periods: 252., daily: true };
        let transform = Transform::LogReturn { h: 5 };
        let closed = asset.bars.last().unwrap().ts + 86_400;
        let prepare = {
            let (a, b) = (asset.clone(), bench.clone());
            move || sc::prepare(&a, &b, options, tr_core::Timeframe::D1, transform, "benchmark fixture only", closed, None)
        };
        add!("studio.relate_transforms_5k", {
            let prepare = prepare.clone();
            move || {
                let p = bb(prepare().unwrap());
                let fit = sc::training_fit(&p, 7, tr_data::studio_inference::CovarianceMethod::HacFixed5).unwrap();
                // Strict h=5 grid on n+1 bars: the first five rows have no return.
                assert_eq!(p.valid_rows().len(), n - 4);
                assert_eq!(fit.n, (n - 4) * 4 / 5);
                assert!(fit.beta > 0.5 && fit.beta < 1.1, "{}", fit.beta);
                p.ts.len() + fit.n
            }
        });
        let p = prepare().unwrap();
        let pts: Vec<(i64, f64, f64, bool)> = p
            .valid_rows()
            .into_iter()
            .map(|t| (p.ts[t], p.y.values[t].unwrap(), p.x.values[t].unwrap(), p.y.break_before[t]))
            .collect();
        let expect_expanding = pts.len().min(1000);
        add!("studio.relate_expanding_5k", move || {
            let out = bb(sc::expanding_beta(bb(&pts)));
            assert_eq!(out.len(), expect_expanding);
            assert!(out.last().unwrap().1.is_some());
            out.len()
        });
        let fit = sc::training_fit(&p, 7, tr_data::studio_inference::CovarianceMethod::HacFixed5).unwrap();
        add!("studio.fair_value_5k", move || {
            let points = bb(sc::fair_value(bb(&p), bb(&fit)));
            assert_eq!(points.len(), sc::MAX_POINTS.min(n - 4));
            assert!(points.iter().any(|q| q.in_sample) && points.iter().any(|q| !q.in_sample));
            assert!(points.iter().all(|q| q.bands[0] < q.centre && q.centre < q.bands[3]));
            points.len()
        });
    }
    // Regression Lab (slice 5): seven factors, H = 5. `build_design` alone, then the whole `compute`
    // (design, pivoted QR, four covariances, holdout, diagnostics).
    {
        let (spec, target, factors) = fixtures::lab_model(n);
        let daily = true;
        let closed = target.bars.last().unwrap().ts + 86_400;
        let (t, f, s) = (target.clone(), factors.clone(), spec.clone());
        add!("studio.lab_design_7x5000", move || {
            let d = bb(tr_data::studio_lab::build_design(
                bb(&t),
                bb(&f),
                bb(&s),
                tr_core::Timeframe::D1,
                daily,
                "benchmark fixture only",
                closed,
                None,
            ))
            .unwrap();
            // Daily bars: target h=1 needs one prior bar; the deepest lag + h window drops a few more.
            assert!(d.y.len() >= n - 12 && d.x.len() == 7 && d.x.iter().all(|c| c.len() == d.y.len()));
            d.y.len()
        });
        add!("studio.lab_7x5000", move || {
            let r = bb(tr_data::studio_lab::compute(
                bb(&target),
                bb(&factors),
                bb(&spec),
                tr_core::Timeframe::D1,
                daily,
                "benchmark fixture only",
                closed,
                None,
                1,
            ));
            let fit = r.fit.as_ref().expect("lab fit available");
            assert_eq!(fit.k, 8);
            // Real inference work at both scales: 200 bars still leave ≥ 80 training and ≥ 20 test rows.
            assert_eq!(r.inference.len(), 4);
            assert!(r.low_n.is_none(), "{:?}", r.low_n);
            assert!(r.inference.iter().all(|i| i.covariance.covariance.is_some()));
            assert!(r.holdout.as_ref().unwrap().r2_oos.is_some());
            assert!(!full || r.points.len() == r.design_rows && r.design_rows > 4900 && fit.n_train > 3900);
            assert!(r.diagnostics.is_some() && r.points.len() <= tr_data::studio_corr::MAX_POINTS);
            r.points.len() + fit.n_train
        });
    }
    // Frozen residual tickers (slice 6): scoring a frozen 7-factor model (design + dot products +
    // series), the ADF lag search on 2,000 AR(1) residuals, and the fixed-horizon CUSUM monitor.
    {
        use tr_data::studio_residual as sr;
        let (fit, target, factors) = fixtures::resid_model(n);
        let closed = target.bars.last().unwrap().ts + 86_400;
        let (f1, t1, r1) = (fit.clone(), target.clone(), factors.clone());
        add!("studio.residual_score_7x5000", move || {
            let r = bb(sr::score(bb(&f1), bb(&t1), bb(&r1), true, "benchmark fixture only", closed, None, 1));
            assert!(r.unavailable.is_none(), "{:?}", r.unavailable);
            assert!(!full || r.scored.len() > 900 && r.residual.ts.len() == tr_data::studio_corr::MAX_POINTS.min(n));
            assert!(!r.scored.is_empty() && r.scored.len() == r.scored_ts.len());
            assert!(r.z.values.iter().flatten().any(|z| *z != 0.));
            r.scored.len() + r.residual.ts.len()
        });
        let e = fixtures::resid_ar1(if full { 2000 } else { 200 }, 0.9);
        add!("studio.residual_adf_2000", move || {
            let a = bb(tr_data::studio_stationarity::adf(bb(&e))).expect("adf ready");
            assert!(a.max_lag >= if full { 25 } else { 14 } && a.n_eff + a.max_lag + 1 == e.len());
            a.n_eff + a.lag
        });
        let mut mfit = fit.clone();
        let stored = if full { 2000 } else { 400 };
        mfit.train_residuals = fixtures::resid_ar1(stored, 0.3);
        mfit.train_breaks = vec![false; stored];
        mfit.train_breaks[0] = true;
        (mfit.monitoring_horizon, mfit.bootstrap_block) = (250, 13);
        mfit.train_residuals_dropped = 0;
        mfit.content_digest = mfit.compute_digest();
        let mut rep = sr::score(&fit, &target, &factors, true, "benchmark fixture only", closed, None, 1);
        rep.scored = fixtures::resid_ar1(400, 0.3);
        rep.scored_ts = (0..400).collect();
        rep.scored_breaks = vec![false; 400];
        rep.scored_breaks[0] = true;
        add!("studio.residual_monitor_250x499", move || {
            match bb(sr::monitor_check(bb(&mfit), bb(&rep))) {
                sr::CheckState::Ready(m) => {
                    assert!(m.p.is_some() && m.k == 250 && m.draws == 499 && m.envelope.len() == 250);
                    m.k + m.envelope.len()
                }
                other => panic!("monitor not ready: {other:?}"),
            }
        });
        // Factor attribution (slice 7): a pure function of the frozen fit and its scored report;
        // the window is the latest complete stride-1 run, so the sum mode does real work.
        {
            use tr_data::studio_attribution as sa;
            let rep = sr::score(&fit, &target, &factors, true, "benchmark fixture only", closed, None, 1);
            let obs = rep.residual.values.iter().rev().take_while(|v| v.is_some()).count().saturating_sub(1).max(1);
            assert!(!full || obs >= 4000, "attribution window too short: {obs}");
            let w = sa::AttrWindow::Obs { obs, to: None };
            let fit = fit.clone();
            add!("studio.attribution_7x5000", move || {
                let a = bb(sa::attribute(bb(&fit), bb(&rep), bb(&w)));
                assert!(a.unavailable.is_none(), "{:?}", a.unavailable);
                assert!(a.m == obs && a.covariances.len() == 4, "m {} covs {}", a.m, a.covariances.len());
                let sum: f64 = a.intercept + a.factors.iter().map(|f| f.contribution).sum::<f64>();
                assert!((a.actual - sum - a.residual).abs() <= 1e-9 * a.actual.abs().max(1.));
                let not_moves = a.factors.iter().filter(|f| matches!(f.kind, sa::ExposureKind::NotAMove { .. })).count();
                let level = a
                    .factors
                    .iter()
                    .filter(|f| matches!(&f.kind, sa::ExposureKind::NotAMove { why } if why.contains("levels")))
                    .count();
                assert!(not_moves == 3 && level == 1, "NotAMove {not_moves}, level {level}");
                a.m + a.path.len()
            });
        }
    }
    // SCAN VIEW engine (slice 8): 650 daily bars per name, so the weekly reads (>=120 completed
    // weeks of context) and both RSI counters do real work for every row.
    {
        use tr_data::whats_news::equity_engine as ee;
        let n = if full { 500 } else { 50 };
        let h: Rc<Vec<(String, Vec<tr_core::Bar>)>> =
            Rc::new((0..n).map(|i| (format!("S{i:03}"), fixtures::daily(650, 9000 + i as u64))).collect());
        let p = ee::EngineParams::default();
        let hc = h.clone();
        add!("scan.engine_board_500_cold", move || {
            let b = bb(ee::engine_board(bb(&hc), p));
            assert_eq!(b.rows.len(), n);
            assert!(b.short.is_empty() && b.invalid.is_empty());
            for r in &b.rows {
                let w = r.weekly.as_ref().expect("weekly read");
                assert!(r.pattern_d.is_ok() && r.rsic_d.is_ok() && w.pattern.is_ok() && w.rsic.is_ok(), "{}", r.symbol);
            }
            assert!(b.rows.iter().all(|r| r.rsic_d.as_ref().is_ok_and(|c| c.valid == 20 && c.closes_used == 400)));
            b.rows.len() * 4
        });
        let cache = ee::BoardCache::default();
        assert!(!cache.get_or_compute(&h, p).1, "first call computes");
        add!("scan.engine_board_500_warm", move || {
            let (board, hit) = bb(cache.get_or_compute(bb(&h), p));
            assert!(hit, "warm path must be a cache hit");
            assert_eq!(board.rows.len(), n);
            board.rows.len()
        });
    }
    let bars = fixtures::daily(if full { 5000 } else { 200 }, 83);
    let seed = stable_hash::bars_digest(&bars);
    let values: Vec<_> = bars.iter().map(|b| b.close / b.open - 1.).collect();
    let breaks = vec![false; values.len()];
    add!("inference.stationary_bootstrap_5k_x500", move || {
        let mut work = 0;
        let mut sum = 0.;
        tr_data::studio_inference::stationary_bootstrap(values.len(), 500, seed, &breaks, |indices| {
            // Consume every sampled value, rather than timing index generation alone.
            sum += indices.iter().map(|&i| values[i]).sum::<f64>() / indices.len() as f64;
            work += indices.len();
        });
        assert!(bb(sum).is_finite());
        assert_eq!(work, values.len() * 500);
        work
    });
    let columns = fixtures::inference_clocks(if full { 120 } else { 60 });
    let breaks = vec![false; columns[0].len()];
    add!("inference.maxt_78x500", move || {
        let band = bb(tr_data::studio_inference::session_max_t(bb(&columns), &breaks, 500, seed));
        assert_eq!(band.draws, 500);
        assert!(band.critical.is_some_and(|v| v.is_finite() && v > 0.));
        assert_eq!(band.intervals.len(), 78);
        assert!(band.intervals.iter().all(Option::is_some));
        band.intervals.len() * band.draws
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
