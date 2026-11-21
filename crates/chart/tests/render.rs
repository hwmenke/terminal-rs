//! Renders real frames offscreen (any adapter, including software Vulkan) and checks pixels.

use tr_chart::gpu::{Frame, LineKind, Reveal, Style};
use tr_chart::headless::Headless;
use tr_chart::scene::{ChartType, Overlay, PricePane, SeriesGpu, line_ref, price_pane, sub_pane};
use tr_chart::viewport::Viewport;
use tr_core::{Timeframe, indicators, sample};

fn dump_ppm(path: &str, w: u32, h: u32, rgba: &[u8]) {
    let mut out = format!("P6 {w} {h} 255\n").into_bytes();
    out.extend(rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]));
    std::fs::write(path, out).unwrap();
}

fn count(px: &[u8], pred: impl Fn(&[u8]) -> bool) -> usize {
    px.as_chunks::<4>().0.iter().filter(|p| pred(p.as_slice())).count()
}

#[test]
fn renders_candles_volume_lines_and_hist() {
    let Some(mut h) = Headless::new(800, 400) else {
        eprintln!("no wgpu adapter; skipping");
        return;
    };
    eprintln!("adapter: {:?}", h.adapter_info.name);
    let bars = sample::generate("AAPL", Timeframe::D1, 2_000, 1_790_000_000);
    let series = SeriesGpu::build(&bars);
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let overlays = [Overlay {
        line: line_ref(&indicators::sma(&closes, 20)),
        color: [0.0, 0.6, 1.0, 1.0],
        kind: LineKind::Line,
        scale: 1.0,
    }];
    let v = Viewport::fit_last(bars.len(), 150, 800.0);
    for ct in ChartType::ALL {
        let out = price_pane(&PricePane {
            series: &series,
            chart_type: ct,
            show_volume: true,
            overlays: &overlays,
            viewport: &v,
            height_px: 400.0,
            log: false,
            manual_scale: None,
            style: Style::default(),
        });
        h.render(&mut [Frame { pane: 1, frame_no: 0, cmds: out.cmds }]);
        let px = h.read_pixels();
        if let Ok(dir) = std::env::var("TR_DUMP_DIR") {
            dump_ppm(&format!("{dir}/{ct:?}.ppm"), 800, 400, &px);
        }
        let green = count(&px, |p| p[1] > 150 && p[0] < 100);
        let red = count(&px, |p| p[0] > 200 && p[1] < 100);
        let blue = count(&px, |p| p[2] > 200 && p[0] < 50);
        eprintln!("{ct:?}: green {green} red {red} blue {blue}");
        assert!(blue > 300, "{ct:?}: SMA overlay not drawn");
        match ct {
            ChartType::Candles | ChartType::Bars => assert!(green > 500 && red > 500, "{ct:?}"),
            ChartType::Line | ChartType::Area => {
                let amber = count(&px, |p| p[0] > 200 && p[1] > 120 && p[1] < 200 && p[2] < 60);
                assert!(amber > 300, "{ct:?}: price line missing ({amber})");
            }
        }
    }

    // MACD histogram in a sub-pane.
    let m = indicators::macd(&closes, 12, 26, 9);
    let hist = [Overlay { line: line_ref(&m.histogram), color: [1.0; 4], kind: LineKind::Hist, scale: 1.0 }];
    let out = sub_pane(&hist, &v, 400.0, None, Style::default());
    h.render(&mut [Frame { pane: 2, frame_no: 0, cmds: out.cmds }]);
    let px = h.read_pixels();
    assert!(count(&px, |p| p[1] > 150 && p[0] < 100) > 200);
    assert!(count(&px, |p| p[0] > 200 && p[1] < 100) > 200);
}

#[test]
fn reveal_sweeps_in_from_the_left_and_fades() {
    let Some(mut h) = Headless::new(800, 400) else {
        eprintln!("no wgpu adapter; skipping");
        return;
    };
    let bars = sample::generate("AAPL", Timeframe::D1, 2_000, 1_790_000_000);
    let series = SeriesGpu::build(&bars);
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let overlays = [
        Overlay {
            line: line_ref(&indicators::sma(&closes, 20)),
            color: [0.0, 0.6, 1.0, 1.0],
            kind: LineKind::Line,
            scale: 1.0,
        },
        Overlay {
            line: line_ref(&indicators::ema(&closes, 50)),
            color: [1.0, 0.3, 0.3, 1.0],
            kind: LineKind::Dots,
            scale: 1.0,
        },
    ];
    let v = Viewport::fit_last(bars.len(), 150, 800.0);
    let mut draw = |reveal: Reveal, pane: u64| {
        let out = price_pane(&PricePane {
            series: &series,
            chart_type: ChartType::Candles,
            show_volume: true,
            overlays: &overlays,
            viewport: &v,
            height_px: 400.0,
            log: false,
            manual_scale: None,
            style: Style { reveal, ..Style::default() },
        });
        h.render(&mut [Frame { pane, frame_no: pane, cmds: out.cmds }]);
        h.read_pixels()
    };
    // Lit pixels (candles, volume or the SMA) in columns [x0, x1).
    let lit = |px: &[u8], x0: usize, x1: usize| {
        px.as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .filter(|(i, p)| (x0..x1).contains(&(i % 800)) && p[..3] != [0, 0, 0])
            .count()
    };

    let half = draw(Reveal::sweep(0.45, v.left(), v.visible_bars()), 1);
    if let Ok(dir) = std::env::var("TR_DUMP_DIR") {
        dump_ppm(&format!("{dir}/reveal.ppm"), 800, 400, &half);
    }
    let (left, right) = (lit(&half, 0, 250), lit(&half, 450, 800));
    eprintln!("half revealed: left {left} right {right}");
    assert!(left > 2_000, "bars behind the front are drawn ({left})");
    assert_eq!(right, 0, "nothing past the front, lines and dots included");

    let full = draw(Reveal::DONE, 2);
    assert!(lit(&full, 450, 800) > 2_000);

    let faded = draw(Reveal { opacity: 0.0, ..Reveal::DONE }, 3);
    assert_eq!(lit(&faded, 0, 800), 0, "opacity 0 draws nothing");
}

#[test]
fn steps_draw_short_sessions_without_joining_them() {
    let Some(mut h) = Headless::new(800, 400) else {
        eprintln!("no wgpu adapter; skipping");
        return;
    };
    // Two-bar sessions alternating between 25 and 75 (pivots on a 4H equity chart), then a
    // session of one bar at 50.
    let mut levels: Vec<f64> = (0..39).map(|i| if i / 2 % 2 == 0 { 25.0 } else { 75.0 }).collect();
    levels.push(50.0);
    let v = Viewport::fit_last(levels.len(), levels.len(), 800.0);
    let mut draw = |kind: LineKind, pane: u64| {
        let o = [Overlay { line: line_ref(&levels), color: [1.0; 4], kind, scale: 1.0 }];
        let out = sub_pane(&o, &v, 400.0, Some((0.0, 100.0)), Style::default());
        h.render(&mut [Frame { pane, frame_no: pane, cmds: out.cmds }]);
        h.read_pixels()
    };
    // Lit pixels in rows [y0, y1); 25 and 75 sit on rows 300 and 100, 50 on row 200.
    let rows = |px: &[u8], y0: usize, y1: usize| {
        px.as_chunks::<4>().0.iter().enumerate().filter(|(i, p)| (y0..y1).contains(&(i / 800)) && p[0] > 0).count()
    };
    let steps = draw(LineKind::Steps, 1);
    if let Ok(dir) = std::env::var("TR_DUMP_DIR") {
        dump_ppm(&format!("{dir}/steps.ppm"), 800, 400, &steps);
    }
    let (high, low, one_bar) = (rows(&steps, 95, 105), rows(&steps, 295, 305), rows(&steps, 195, 205));
    eprintln!("steps: 75 {high}, 25 {low}, 50 {one_bar}");
    assert!(high > 300 && low > 300, "every session draws its level");
    assert!(one_bar > 10, "a one-bar session draws");
    assert_eq!(rows(&steps, 110, 190) + rows(&steps, 210, 290), 0, "sessions are not joined");
    let lines = draw(LineKind::Line, 2);
    assert!(rows(&lines, 110, 190) > 0, "a line would join them");
}

#[test]
fn zoomed_out_steps_stay_inside_their_group() {
    let Some(mut h) = Headless::new(800, 400) else {
        eprintln!("no wgpu adapter; skipping");
        return;
    };
    // 401 bars at one pixel each draw in groups of two; the last group holds only bar 400,
    // which starts a new session at 75.
    let mut levels = vec![25.0; 400];
    levels.push(75.0);
    let v = Viewport { right: 410.0, bar_px: 1.0, width_px: 800.0 };
    let o = [Overlay { line: line_ref(&levels), color: [1.0; 4], kind: LineKind::Steps, scale: 1.0 }];
    let out = sub_pane(&o, &v, 400.0, Some((0.0, 100.0)), Style::default());
    h.render(&mut [Frame { pane: 1, frame_no: 0, cmds: out.cmds }]);
    let px = h.read_pixels();
    // Bar i spans columns [i + 390, i + 391); 75 sits on row 100.
    let lit = |x0: usize, x1: usize| {
        px.as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .filter(|(i, p)| (95..105).contains(&(i / 800)) && (x0..x1).contains(&(i % 800)) && p[0] > 0)
            .count()
    };
    assert!(lit(790, 791) > 0, "the new session draws on its own bar");
    assert_eq!(lit(0, 790), 0, "and not over the previous session");
}

/// Reuse the same pane while series length, price level, class and chart type change.
/// Exercises actual buffer uploads/bindings; the UI event harness lives in the app.
#[test]
fn switching_instruments_handles_empty_short_and_changed_series() {
    let Some(mut h) = Headless::new(800, 400) else {
        eprintln!("no wgpu adapter; skipping GPU switching test");
        return;
    };
    for (i, input) in [
        "SPY",
        "AAPL US Equity",
        "SPX Index",
        "BTC",
        "BTCUSD Curncy",
        "USDBRL CURNCY",
        "BRL=X",
        "EURUSD Curncy",
        "US10Y Govt",
        "CL1 Comdty",
        "???",
        "",
        "ação",
        &"A".repeat(100_000),
    ]
    .iter()
    .enumerate()
    {
        let Some(sec) = tr_core::Security::parse(input) else { continue };
        for n in [0, 1, 2, 300] {
            let bars = sample::for_security(&sec, Timeframe::D1, n, 1_790_000_000);
            let series = SeriesGpu::build(&bars);
            let viewport = Viewport::fit_last(n, 160, 800.0);
            for chart_type in ChartType::ALL {
                let out = price_pane(&PricePane {
                    series: &series,
                    chart_type,
                    show_volume: true,
                    overlays: &[],
                    viewport: &viewport,
                    height_px: 400.0,
                    log: i % 2 == 0,
                    manual_scale: None,
                    style: Style::default(),
                });
                h.render(&mut [Frame { pane: 1, frame_no: 0, cmds: out.cmds }]);
            }
        }
    }
}
