# Performance gates and frame budget

Run from the repository root, offline on a headless Linux host:

```sh
scripts/perf-gate.sh                         # compute/data + headless paint
cargo perf-gate                             # release compute/data, normalised regression check
cargo perf-gate --filter edge --absolute-only
cargo perf-gate --list
cargo perf-gate --json perf/my-run.json
cargo perf-paint                            # release egui UI + tessellation, no GPU
```

Both aliases include `--offline`. No network, display server, adapter or GPU is needed.
The script stops at the first failed gate. Linux CI runs both gates, with absolute
ceilings for shared runners. The existing Criterion auction benchmark was also
confirmed available with `cargo bench -p tr-data --bench auction --offline --no-run`.
The new gate deliberately uses a dependency-free timing harness; a Criterion mirror
is phase 2.

## Budgets and the gate

| Class | Intended latency |
| --- | --- |
| Frame | 8 ms CPU inside the app's `ui()`; 2 ms per chart pane |
| Interactive | 50 ms for recompute on a bar close/tick |
| Command | 500 ms |
| Load | 50 ms for a chart cache read plus pyramid build |

A 60 Hz frame is 16.67 ms. The UI target reserves the remaining time for
tessellation, GPU and present. These are CPU targets, not a claim of 60 fps on any
particular display. The headless test covers one pane, not an entire 2×2 desk.

`perf/budgets.json` holds reviewed compute/data ceilings in microseconds.
`perf/paint-budgets.json` holds the UI, tessellation and total ceilings for each
paint scenario. Per-layer steady ceilings have a single source,
`app/src/perf.rs::Layer::budget_ms()`; pan allows 4× those ceilings. The app HUD
uses the same layer ceilings. Changes take effect on the next run.

The harness warms each closure for at least 50 ms and three calls, probes a batch
of at least 200 µs, then takes 9–201 samples targeting 300 ms. Median, nearest-rank
p95, minimum and sample count are recorded. The median is the hard gate. A failed
case gets two retries; the best median counts. Panicked closures or a zero work
token are harness errors. Tokens represent actual rows, cells, eligible observations
or draw commands, with explicit admission assertions for reports. All full and smoke
cases are exercised by ordinary tests. Missing/stale budget IDs fail the suite.

Absolute failure: median > ceiling × slack. With a baseline and `relative: true`,
the ratio is `(median / calibration) / (baseline median / baseline calibration)`.
A ratio >1.5 fails only when the scaled difference also exceeds 20 µs. A ratio
>1.2 warns; <0.7 suggests reviewing the baseline. Tune is exempt from the relative
check because it scales with available threads. New cases still obey the absolute
ceiling. Paint uses absolute checks only.

Exit codes for `perf-gate`: **0** pass, **1** over budget/regression, **2** harness
error (including empty filters and bad configuration). The paint test fails through
Rust's test runner. Ordinary debug tests do not assert wall-clock budgets.

| Environment | Meaning |
| --- | --- |
| `CARGO_TARGET_DIR` | Artifact root; results go in `<target>/perf/latest.json` and `paint.json` |
| `TR_PERF_SLACK` | Positive finite absolute budget multiplier; default 1.0, override printed loudly |
| `TR_PERF_ABSOLUTE=1` | Disable compute relative checking; paint is always absolute |
| `TR_PERF_SAVE=1` | Merge successful paint measurements into the existing baseline |
| `TR_PERF_ALLOW_DEBUG=1` | Diagnostic paint run only; release is required otherwise |
| `TR_FPS=1` | Open the F12 timing HUD at startup |

`--json` adds another compute report without suppressing `latest.json`. Build
artifacts honor `CARGO_TARGET_DIR`; no hard-coded target path is needed.

## Baseline workflow

```sh
cargo perf-gate --absolute-only --save-baseline
TR_PERF_SAVE=1 cargo perf-paint
scripts/perf-gate.sh
```

Run on an otherwise idle host, review medians, p95 and calibration, then review the
baseline diff. The calibration is the median of nine fixed workloads: fill 1 Mi
f64 values with xorshift, sum their square roots, sort 200k u64 values. It normalises
CPU speed approximately; it cannot account for every cache, scheduler or core-count
difference. Do not automatically update the baseline in CI. Filtered runs cannot
save a baseline; failures cannot save one either. Compute refresh preserves paint
entries. Paint baseline entries are for review only and have no relative gate.

For initial calibration, retain each planned ceiling when it is at least 3× the
measured median; otherwise round 3× the median upward. If that exceeds the class
cap, keep the honest ceiling and mark `over_target`. Do not hide an expensive
operation by changing its implementation, fixture size or admission rules.

## Paint scenarios and F12

The release-only `perf_gate_paint` test runs `egui::Context::run_ui` at 1600×900,
with a 1580×880 chart. It warms 20 frames and samples 120 moving-pointer steady
frames and 120 frames alternating a 24 px pan. UI and `ctx.tessellate` are timed
separately. Wgpu callbacks remain inert CPU callback primitives. The intraday
scenario uses 5,000 BTCUSD M5 bars ending in the current bucket, native stack lanes,
cloud, RAIL, ZBAND, stops, cone, Edge, four AVWAP anchors and volume. The auction
scenario uses 7,800 AAPL M1 bars, 20 sessions, four AVWAP anchors and no stack.
Reports are computed before timing and delivered through the normal `on_*` methods.
Text/geometry and timing guards reject empty or refused layers. A failing scenario
is sampled up to three times; the best median for each metric counts.

F12 toggles the foreground HUD and thread-local layer scopes. The HUD shows UI
EMA/p95/max, frame interval and fps, per-layer EMA/p95/ceiling, unscoped chart time,
and recent worker job rows. Layer values sum across all painted panes. Nested
chart, dock and frame times must not be added together. Disabled scopes perform
one thread-local boolean read and do not read the clock or allocate.

The HUD's CPU time includes the HUD itself and has a one-frame reporting delay.
`dt − ui` also includes scheduling/waits; it is **not** a GPU measurement. Worker
wall time wraps request handling (including I/O/waits); streaming follow-up tasks
are separate. CPU timing runs inside cache misses for StackEdge, TrendStack,
StopCloud, TrendCone, AVWAP, AuctionMap, Tune, honest backtests and cached studies;
indicator construction and CPU pyramid builds are also timed. Each kind/phase keeps
64 recent samples plus its count/EMA/max. Jobs are collected even with the HUD shut,
so opening it shows recent work. Instrumentation preserved formulas and persisted settings/layout fields. Slice 1
adds the ephemeral session-summary revisions described below.

## Adding a case

Add a seeded fixture in `crates/perf/src/fixtures.rs`, a public-API closure with a
nonzero meaningful work token in `suite.rs`, and the exact ID/ceiling in
`perf/budgets.json`. Exercise it at `Scale::Smoke`, calibrate a full release run and
review the baseline. Fixture bars are intentionally tagged verified to exercise
admission paths; they are fabricated **benchmark inputs only**. `tr-perf` is solely
a dev-dependency of the app and never a normal/transitive app dependency. The
`fixtures_never_ship_in_the_app` test enforces the manifest boundary.

## Scope and limitations

Phase 1 retains all 22 compute/data cases, both paint scenarios and all listed
worker CPU sites. The only cut is relative-baseline checking for paint (the first
item in the plan's cut order); absolute total/per-layer checks remain. Phase 2's
Criterion mirror, four-pane/whole-app headless gates, remaining worker sites,
sparklines and CSV export remain deferred. Existing ignored timing/live tests are
unchanged.

Wall-clock gates need a reasonably quiet host. Absolute ceilings carry calibration
headroom and retries absorb occasional interference; prolonged contention can still
fail. The crypto scenario follows wall time and may straddle an M5 rollover; its
render/admission guards catch a missing layer. SQLite is in-memory here, so the data
budget does not certify disk or network latency. GPU upload, rendering and present
remain outside these headless measurements; use the existing `framebench` for GPU
work.

## Pre-slice 1 calibration on this box (historical)

Recorded 2026-11-16, release profile, Intel(R) Xeon(R) Processor, 8 available cores, linux. Idle-host calibration median: **6000.22 µs** (nine runs). The initial run overlapped a build and was discarded for compute baseline purposes; this table uses the subsequent idle run. Baseline contains raw paint measurements and all compute summaries.

| Compute/data case | Median µs | p95 µs | Ceiling µs |
| --- | ---: | ---: | ---: |
| `data.vault_read_5k` | 905.00 | 1002.82 | 15000 |
| `data.vault_stitch_7800` | 3444.15 | 3708.46 | 25000 |
| `data.vault_ingest_390` | 1486.10 | 4309.98 | 60000 |
| `data.bars_digest_23k` | 1245.69 | 1338.44 | 5000 |
| `data.series_gpu_23k` | 272.21 | 312.86 | 5000 |
| `scene.price_pane_all` | 4.48 | 4.95 | 500 |
| `scene.price_pane_200` | 1.28 | 1.48 | 300 |
| `stack.trend_stack_5k` | 1037.12 | 1286.22 | 20000 |
| `edge.regime_edge_5k` | 30728.43 | 33823.31 | 93000 |
| `edge.wall_edge_5k` | 11533.10 | 12354.76 | 50000 |
| `nulls.bh_10k` | 217.83 | 240.68 | 3000 |
| `nulls.sign_test` | 67.65 | 73.46 | 210 |
| `clock.build_60d_30m` | 4701.93 | 5073.32 | 50000 |
| `stack.tune_7x7_5k` | 91994.91 | 126257.18 | 500000 |
| `avwap.report_23k_x4` | 474.81 | 536.84 | 2000 |
| `avwap.time_link_span` | 1.01 | 1.12 | 20 |
| `auction.build_20x390` | 1708.19 | 1920.48 | 6000 |
| `auction.composite_20` | 1.28 | 1.34 | 50 |
| `stops.report_5k` | 6848.13 | 7080.14 | 50000 |
| `cone.report_5k` | 6088.98 | 7410.44 | 250000 |
| `bt.report_5y` | 247.12 | 279.51 | 40000 |
| `bt.dsr_family_64` | 17024.24 | 17890.81 | 60000 |

| Paint metric | Median ms | p95 ms | Ceiling ms |
| --- | ---: | ---: | ---: |
| `paint.intraday.steady.ui` | 0.209 | 0.248 | 1.5 |
| `paint.intraday.steady.tess` | 0.392 | 0.616 | 1.5 |
| `paint.intraday.steady.total` | 0.604 | 0.850 | 2.5 |
| `paint.intraday.pan.ui` | 0.477 | 0.532 | 3.5 |
| `paint.intraday.pan.tess` | 0.615 | 0.688 | 2 |
| `paint.intraday.pan.total` | 1.085 | 1.210 | 5 |
| `paint.auction.steady.ui` | 14.996 | 15.925 | 45 |
| `paint.auction.steady.tess` | 0.266 | 0.298 | 1.5 |
| `paint.auction.steady.total` | 15.269 | 16.207 | 46 |
| `paint.auction.pan.ui` | 15.275 | 15.829 | 46 |
| `paint.auction.pan.tess` | 0.278 | 0.305 | 2 |
| `paint.auction.pan.total` | 15.559 | 16.122 | 47 |

| Layer (worst steady scenario) | Median ms | Planned steady ms | Calibrated steady ceiling ms |
| --- | ---: | ---: | ---: |
| Chart total (auction) | 14.965 | 2.00 | 45.00 |
| Legend including cockpit chip / vault footer (auction) | 10.966 | 0.40 | 33.00 |
| Session/review/alert levels (auction) | 3.810 | 0.10 | 12.00 |

All other per-layer ceilings retain their planned values. Pan uses 4× the steady layer ceiling. The calibration rule uses medians, not p95.

### Historical over target

Before Slice 1, the **auction pane missed the intended frame target**: about 15 ms of UI CPU for one pane, even while steady. Its legend/footer and session-level paths dominate; Auction Map geometry itself is roughly 0.013 ms steady. The calibrated gate ceilings of 45–47 ms are honest regression limits, not a claim that the pane meets the 2 ms design target or that a 2×2 desk fits in 8 ms. No optimisation was attempted in this instrumentation row.

`edge.regime_edge_5k` has a 93 ms calibrated ceiling (3× the 30.728 ms median, rounded up), exceeding the 50 ms interactive class cap; it is marked `over_target`. Its measured median is still below that class target. `data.vault_ingest_390` retains the plan's 60 ms ceiling and is also flagged because that ceiling exceeds the 50 ms load class; its measured median is only 1.486 ms. Auction paint UI/total entries carry `over_target` flags. These flags describe target/ceiling debt rather than suppressing failures.

The loaded intraday pane is inside the original frame ceilings: 0.209 ms UI / 0.604 ms total steady, and 0.477 ms UI / 1.085 ms total panning. TUNE measured 91.995 ms against its retained 500 ms ceiling.

## Perf instrumentation validation (historical)

`cargo test --workspace --offline`: **1,516 passed, 0 failed, 9 ignored**. The new
coverage comprises 14 harness/fixture tests, five meter/scope/HUD tests, two chart
recording/shape-equivalence tests, the F12 binding test and the worker wall/CPU test,
plus the explicitly invoked ignored release paint gate. All 22 benchmark closures
also run once at smoke scale in ordinary tests.

The optional strict Clippy check is blocked by pre-existing warnings, beginning at
`crates/core/src/provenance.rs:33` (`needless_lifetimes`); checking only the selected
packages also reports existing application warnings. No statistics/data sources
were edited to address unrelated lint or formatting debt. The new perf modules
produce no Clippy diagnostics after fixing their fixture chunk iteration.

`cargo build --release --offline` completed successfully. The dedicated `tr-perf`
Clippy run (`cargo clippy --no-deps -p tr-perf --all-targets --offline -- -D warnings`)
also passed.

The end-to-end rejection demonstration temporarily set `nulls.sign_test` to
**0.001 µs**: measured median **62.62 µs**, **0 passed / 1 failed**, process exit
**1**, no harness errors. A `finally` block restored the original budgets
byte-for-byte (the sign-test ceiling is **210 µs**). The ordinary injected-slow
test independently checks a 2 ms sleep against a 0.5 ms ceiling.

The final `scripts/perf-gate.sh` run passed: **22 compute/data cases, 0 failures,
0 harness errors**, then **1 release paint test passed, 0 failed, 0 ignored**,
checking **84 aggregate/per-layer metrics**. The compute run emitted one advisory
normalised Cone warning (1.36×, below the 1.5× failure threshold); all ceilings and
regression rules passed. Final reports remain in `$CARGO_TARGET_DIR/perf/`. A
normal-edge `cargo tree` check confirms `tr-perf` is absent from the shipped app's
dependency graph; Cargo.lock adds no external packages or version changes.

## Slice 1: Auction at frame speed

The unchanged release scenario was remeasured before editing: **14.643 ms UI /
14.906 ms total steady**, **14.699 ms UI / 14.972 ms total panning**. Steady
Legend was **10.693 ms**, Levels **3.718 ms**, ChartTotal **14.605 ms**. This
confirmed repeated intraday summaries/date maps as the bottleneck, rather than
Auction geometry. These are fresh measurements; the earlier table is historical.

The worker now builds a complete, admitted `chart_session::SessionSummary` with
legacy numeric summaries, coverage index spans, exact-neighbour gap marks and
invariant text. It validates all original bars through `vault::research_bars`
before taking latest/prior-session slices for helpers whose definitions depend
only on those sessions. Legacy formulas, Auction allocation/value area, fine-data
fetching and level-touch math are unchanged. Live descriptive forming input has
an explicit receipt; CLOSED THRU uses bar ends and the observation clock. No
provider/SQLite work is added by the summary request.

Each pane tracks generation, monotonic bars/stitch receipt revisions, timeframe,
extended-hours mode and observation time. Same-length corrections and new
receipts at the same epoch invalidate. One in-flight job and a 300 ms throttle
coalesce changes; obsolete completions release the slot and cannot overwrite
current values. Identity changes, sample input and refusal clear numeric output.
During recomputation the old receipt says PREVIOUS SNAPSHOT with its own clock;
numeric rows and index maps wait for the new acceptance. Hover/pan/zoom/theme
changes reuse the summary. The exact accepted receipt supplies the vault cells.

The ribbon's concentration descriptors/cards are computed when the Auction
report is accepted; geometry reuses the existing viewport/palette cache. The
printed sequential key shares its thresholds with the tiles. Narrow/wide tests
check finite tiles, key width, cards and footer reservations. Tile click navigation,
extra visual polish, and caching the existing short Auction legend formatter were
cut; the latter is negligible after the session scan removal.

The first successful extended paint run measured **0.440 ms total steady**, **0.633
ms total panning**, and **0.464 ms refresh**. Only after this measurement were the
Auction ceilings tightened to the planned values:

| Mode | UI ceiling ms | Tessellation ceiling ms | Total ceiling ms |
| --- | ---: | ---: | ---: |
| Steady | 1.5 | 1.0 | 2.5 |
| Pan | 2.0 | 1.0 | 2.8 |
| Refresh (acceptance included) | 2.0 | 1.0 | 2.8 |

ChartTotal/Legend/Levels steady ceilings are restored to **2.0 / 0.4 / 0.1 ms**;
the existing 4× pan layer multiplier remains. No existing budget was widened.
`paint.auction_refresh` times repeated precomputed response acceptance **and** the
frame, including formatting. All modes retain 7,800 bars, twenty sessions, four
AVWAP anchors, moving pointer and the original viewport/pan motion, with vault
coverage now also supplied. Guards assert actual session/OR/prior legend content,
legacy profile data, coverage, ribbon/key and no new summary jobs on unchanged
frames. Rebuild work is independently timed in the compute gate:
`chart.session_summary_7800` ≤35 ms (target median <12 ms), and
`auction.concentration_20` ≤0.2 ms. Both closures also run at smoke scale.

Final release measurements (2026-11-16, same 8-core Linux host; calibration **6,042.972 µs**):

| Auction metric | Before median ms | Final median ms | Final p95 ms | Ceiling ms |
| --- | ---: | ---: | ---: | ---: |
| steady ui | 14.643 | 0.170 | 0.188 | 1.5 |
| steady tess | 0.262 | 0.240 | 0.256 | 1.0 |
| steady total | 14.906 | 0.410 | 0.431 | 2.5 |
| pan ui | 14.699 | 0.318 | 0.358 | 2.0 |
| pan tess | 0.267 | 0.255 | 0.269 | 1.0 |
| pan total | 14.972 | 0.572 | 0.617 | 2.8 |
| refresh ui | — | 0.187 | 0.212 | 2.0 |
| refresh tess | — | 0.252 | 0.263 | 1.0 |
| refresh total | — | 0.439 | 0.466 | 2.8 |

Steady ChartTotal / Legend / Levels medians are **0.155 / 0.021 / 0.024 ms**
against **2.0 / 0.4 / 0.1 ms** ceilings. The full session rebuild measured
**5.783 ms median / 6.217 ms p95** against 35 ms; twenty concentrations measured
**2.797 µs median / 3.024 µs p95** against 200 µs. All existing Auction compute
ceilings remain unchanged. The reviewed baseline now records all 24 compute cases
under the current calibration and all 105 actual paint metrics.

Final validation:

- `cargo test --workspace --offline`: **1,557 passed, 0 failed, 9 ignored**.
- `cargo build --release --offline`: **passed**.
- `scripts/perf-gate.sh`: **24 compute/data passed, 0 failed, 0 harness errors**;
  **1 release paint test passed, 0 failed, 0 ignored**, checking **105 metrics**.
- **22 new focused tests** cover helper equality and metadata, full-input admission,
  DST-adjacent spans, exact gap neighbours, observation clocks (including extended
  and UTC-stamped H4 actual ends), concentration arithmetic, correction/paging/stitch
  invalidation, coalescing, stale responses, identity/sample/refusal clearing, dynamic
  hovered OHLC, and narrow/wide ribbon/key/card/footer geometry. Existing legacy
  paint tests explicitly receive snapshots before asserting their unchanged values.
- Clippy completed for the selected data/perf/app packages and all targets. No
  diagnostics point to the new code; existing repository warnings remain.
- The final compute run emitted one advisory `bt.report_5y` normalised slowdown
  warning (**1.41×**, below the 1.5× failure threshold); all absolute and regression
  gates passed. No backtest code or budget was changed.

The p95 values are reported observations; the harness continues to gate medians.
Headless timings cover CPU UI/tessellation, without GPU presentation or a four-pane
desk. Concentration is bin-dependent descriptive allocated volume with no CI or
predictive significance. Existing strict Clippy debt remains separate.
