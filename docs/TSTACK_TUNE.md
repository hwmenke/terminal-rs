# TSTACK Plateau

`AAPL TSTACK TUNE [5m]` opens a bold 7×7 parameter-sensitivity grid. `TSTACK TUNE`
uses the focused symbol and chart interval, or daily when no chart is focused.
All existing TSTACK aliases accept `TUNE`. `TUNE`, `TUNE 5m`, and `TUNE US Equity`
remain ticker commands. The palette lists `TSTACK TUNE`.

The grid varies medium-baseline efficiency-ratio length (columns: 8, 12, 16, 20,
24, 32, 40) and minimum regime hold (rows: 2, 4, 6, 10, 14, 20, 30). Other settings
come from the focused matching chart's ADTREND registry configuration, including
session reset, or the Adaptive Trend defaults. The captured current setting is
always in the grid; a nonstandard value replaces the middle axis value. Captured
settings, symbol and interval persist with the layout.

Amber outlines identify the current setting; cyan outlines identify the strongest
supported 3×3 neighbourhood; white identifies the inspected cell. Signed Edge Δ
uses the shared diverging heat ramp and an explicit bp key. Grey hatching means
FDR, effective sample size, or fold support failed, or no test was available.
Numbers remain inspectable even when unsupported; missing statistics show a dash.
Click a cell or use arrow keys while the pane is focused to inspect it. Hover
shows parameters, delta, Welch t, raw rotation p, grid q, regime observation count,
effective count, purged-fold agreement, outcome dates and robustness. There is no
parameter application: the existing chart does not expose ER/hold adjustment.
Reload rereads data. The previous verified grid stays visible, explicitly labelled
as a previous snapshot, while recomputing or after a failed refresh. Sample mode
clears it and invalidates pending responses.

## Implementation and verified data flow

New files:

- `crates/data/src/tstack_tune.rs`: admission, closed sample selection, bounded
  parallel grid, Edge Proof reuse, family-wide FDR, robustness, smoothing and tests.
- `crates/data/src/honest_z.rs`: closed ZBAND diagnostics, robust VR, empirical tails,
  snapshot labels and offline tests.
- `app/src/app/tstack_tune.rs`: pane state, chart configuration capture, request /
  response handling, bold grid, meter, hover/inspection and headless tests.
- `docs/TSTACK_TUNE.md`: behaviour, math, data guarantees and validation.

Modified files:

- `crates/data/src/lib.rs`: export the pure calculation module.
- `crates/core/src/indicators/adaptive.rs`: serialize existing parameter types so
  layout restores the exact captured configuration.
- `app/src/worker.rs`: Request/Response, loading, admission and report-cache key;
  closed-clock ZBAND building; offline integration and cache tests.
- `app/src/chart_view.rs`: expose the existing parameter getter within the app;
  send indicator observation metadata; paint Honest Z labels/hover and grey bands.
- `app/src/command.rs`: `TstackTune` command, description and collision tests.
- `app/src/app.rs`: pane registration, dispatch, persistence, refresh, response routing.
- `app/src/app/panels.rs`: pane rendering dispatch.
- `app/src/app/palette_view.rs`: discoverability.
- `app/src/layouts.rs`: persisted symbol, interval and captured configuration.
- `README.md`: command table and guide link.
- `app/src/app/ticker_switch_tests.rs`: route the headless harness through the same
  closed-clock indicator builder as production.

Intraday requests reuse `worker::analysis_bars`, the existing vault-aware sourced
history route. Like `MarketData::history_for_analysis`, this checks
`Bar::require_verified`; it uses `history_sourced` to retain source identity for
vault admission. It never calls a display/sample fallback. Daily/weekly/monthly
requests reuse the verified archive loader `studio_dist::daily_bars`; larger
intervals use existing native aggregation of those daily bars. Archive fetches
and vault SQLite work stay in the worker. No GUI/server was launched.

`tune_cached` runs in `spawn_blocking`: validate **all** inputs, including forming
bars; normalize mixed cache/live tags only with `vault::research_bars`' stitch
witness; calculate `closed_len` using the feed observation clock (no wall-clock
promotion on a vault-only response); then key/cache the calculation. Keep the
latest at most 5,000 closed bars. The calculation validates again and refuses
fewer than 256 closed bars, fewer than 200 common warmed bars, a warming final
closed bar, invalid OHLCV/parameters, and an unverified vault join in the window.
Missing outcomes and insufficient null samples remain unavailable. No synthetic
prices enter production; fabricated fixtures exist only under `#[cfg(test)]`.
Regular overnight/weekend transitions retain the existing Edge Proof semantics:
the horizon counts observed native bars, not elapsed clock intervals.

All cells use the exact same input and the intersection of their finite-confidence
warm-up masks (including session resets). The current configuration's final closed
medium regime is held fixed across all 49 cells: no per-cell regime shopping.
Conditional counts may differ because parameters change regime membership. Four
chronological purged folds are reused from Edge Proof; these are descriptive
stability checks, not out-of-sample parameter selection.

No disk/network I/O, statistics, digesting, or grid computation occurs in paint.
The pane sends ordinary worker messages; generation checks drop obsolete results.
The grid calculations use scoped standard-library threads in chunks of 13 (at
most four), since this workspace has no direct rayon dependency. Parallelism is
bounded per report, not globally across panes.

## Math

For completed horizon h=5 returns `R(i,h)=C(i+h)/C(i)-1`, using only common warmed
origins whose forward outcome is complete:

`Edge Δ = mean(R | medium regime = reference regime) − mean(R)`.

Reuse `edge::regime_edge` without a different estimator. Its circular block
rotation test moves causal regimes/entry flags against fixed returns and fixed
baseline. `p=(1 + #(|Δ_rot|≥|Δ|))/(1 + #valid rotations)`; at least 99 valid
rotations are required. Reported t is the existing descriptive baseline-relative
Welch t, not the source of the null p. Effective n and purged fold deltas use the
existing Edge Proof overlap rules.

Recompute BH across the **49 grid tests**, replacing unavailable p with 1 in the
family size: sort p ascending, then `q_(i)=min(1,min_(j≥i)(49 p_(j)/j))`.
Unavailable cells retain no q. Support requires q≤0.10, effective n≥30, nonzero
delta and at least three of four same-sign purged fold deltas. BH is valid under
independence or positive dependence; arbitrary grid dependence is not guaranteed.
Searching symbols, intervals or configurations creates additional families this
single-grid correction does not account for.

For a supported cell with nonzero edge d and immediate existing neighbours N
(exclude self; 3 at a corner, 5 on an edge, 8 inside):

`support = #(supported neighbours with sign(edge)=sign(d)) / |N|`

`robustness = support × clamp(median(all neighbour deltas)/d, 0, 1)`.

Missing neighbour deltas or an unsupported center give zero robustness. This
penalizes isolated peaks even when their raw delta is large. The most robust
neighbourhood has maximum score≥0.50; ties prefer the first in grid order, never
the largest edge. Mark its same-sign supported members within the 3×3 area.
Absent qualifying regions print `NO SUPPORTED PLATEAU`. The score is a descriptive
local stability measure, not a probability or evidence of profitable execution.

The meter uses the actual medium-baseline HLC3 series and captured configuration:

`ER_i = |x_i − x_(i−L)| / Σ_(j=i−L+1)^i |x_j − x_(j−1)|` (0 for a flat window).

`a_i = 2/(slow+1) + ER_i [2/(fast+1) − 2/(slow+1)]`.

KAMA uses `α_i=a_i²`; ADMA uses `α_i=a_i`, clamped as in `adaptive_ma`.
The effective EMA-equivalent length is `2/α_i−1`. Show observed min/max across the
closed series, exclude seeds, and restart ER windows at the same session boundaries
as chart TSTACK when reset is enabled. The meter uses a logarithmic length axis.
A monotonic default KAMA series gives 3.5 bars; a flat one gives 1859.5 bars.

## Cache

Reuse Bar Vault's bounded `ReportCache` (256 reports; concurrent identical keys
compute once). `ReportKey` contains report kind, canonical symbol, interval, source
label, closed count/endpoints, full selected-bar OHLCV/provenance digest, vault
epoch, code version and hash of **all** captured parameters, asset class and join
gaps. Failures are never cached. A correction anywhere in the selected history,
parameter/source/epoch change or newly closed bar misses. A forming-only price
change hits; contaminated forming input is refused before any cache hit. Trimming
to 5,000 closes bounds memory and calculation cost equally for all cells.

## Scope decisions and cut order

Honest Z is **included**. ZBAND already had the indicator worker and shared
geometry/legend path; passing the existing observation clock and vault witness
through that request lets diagnostics and grey styling fit without another
loader, service or UI-thread calculation. Its details and limits are below.

Other deliberate limits: one predefined h=5 horizon; one captured current regime;
fixed seven-value axes; click inspection only because ER/hold are fixed in chart
TSTACK. No global worker CPU queue, parameter optimizer, new dependencies, journal,
trade-plan, alert, secrets, service or deployment changes. Preserve verified gates,
closed-clock handling, FDR and worker execution before adding any of those options.

## Verification and acceptance

Verified first by reading: actual `AdaptiveTrendParams` fields/defaults and registry
mapping; `ChartState::stack_params`; command precedence/yellow-key parsing;
`edge::regime_edge`, rotation machinery and `benjamini_hochberg`; `ReportCache`
identity/dedup; `analysis_bars`, `StitchInfo`, `research_bars`, `closed_len`; shared
Studio rendering helpers and heat ramp. Roadmap rows 4 and 8 informed scope. No
reference/run directories were read.

Offline tests cover grid determinism and forming-bar invariance; all 49 trials in
known BH values; plateau versus lone peak and median attenuation; KAMA/ADMA/flat
smoothing; short, invalid, unverified and join refusals; source/vault worker
integration; cache hit/miss for forming/closed changes, corrections, parameters,
source and epoch; parser aliases/invalid arguments/TUNE ticker; persisted captured
settings; stale-response rejection; previous-grid retention; sample denial with
no numerical output; and headless grid labels.

Acceptance: the commands produce a verified grid or an explicit refusal, all cells
share a sample and reference regime, unsupported results are unmistakable, current
and robust regions are distinct, reload retains the previous snapshot, and no
analysis/I/O runs on the UI thread. A 7×7/5,000-bar target is ≤5 s for a cold report
in an optimized build and an immediate memory hit thereafter. The timing test
reports elapsed compute time without a hardware-dependent assertion.

## Honest Z for ZBAND

`ZBAND [length]` keeps its rolling **price-level sample standard deviation**
(`n−1` denominator) and in-window z convention. The chart label names that
estimator. This is not return volatility, an independent Gaussian observation,
or an out-of-sample band breach. All new production ZBAND geometry/readouts use
verified **closed** bars only; the forming slot is NaN, not a solid provisional
band. Missing/unverified inputs and loaded vault joins refuse with no geometry.
At least `length` closed bars are needed for bands. A short or zero-variance
variance-ratio sample prints `VR unavailable` and greys the bands.

The existing `Request::Indicators` now carries asset class, observation time and
optional stitch metadata. `build_chart_indicators` skips the generic ZBAND
calculation, admits/normalizes the inputs using the same vault witness rules,
then calls `honest_z::compute` in the existing `spawn_blocking` task. Other
indicators retain their existing calculations. No new loader, request loop or
network service was introduced. All diagnostic text is prepared in the worker;
paint reads it and the baked overlay colours. The prior built indicators remain
visible until the new worker response arrives.

Diagnostics use up to the last 512 closed prices. For log returns
`r_t = ln(C_t) − ln(C_(t−1))`, with N returns, `μ = mean(r)` and `e_t=r_t−μ`:

- `s1² = Σ e_t² / (N−1)`.
- `m = q (N−q+1) (1−q/N)`.
- `sq² = Σ_(t=q)^N (Σ_(j=0)^(q−1) e_(t−j))² / m` (overlapping windows).
- `VR(q) = sq² / s1²`.
- `δ_j = N Σ_(t=j+1)^N e_t² e_(t−j)² / (Σ e_t²)²`.
- `θ(q) = Σ_(j=1)^(q−1) [2(q−j)/q]² δ_j`.
- `z*(q) = √N (VR(q)−1) / √θ(q)`.

The implementation cancels N between the last two expressions; it neither loses
nor duplicates a factor of √N. These are the overlapping, bias-adjusted variance
estimator and heteroskedastic robust test from [Lo and MacKinlay (1988), equations
12 and 19–20](https://finance.martinsewell.com/stylized-facts/dependence/LoMacKinlay1988.pdf).
Heteroskedastic consistency does not remove dependence assumptions or make the
normal approximation exact in small samples. We require ≥20q returns at each of
q=2,5,10. All three VR/z* pairs are printed. The two-sided Bonferroni family level
is 5% (three fixed tests; rounded critical |z*|=2.394). Any positive rejection
(`VR>1`, `z*>2.394`) marks **TREND PERSISTENCE** and greys both cloud and lines.
Significant negative z* is labelled anti-persistence, without claiming price-level
mean reversion. No rejection is explicitly **RANDOM WALK NOT REJECTED**, not a
mean-reversion verdict. Constant returns and flat prices are unavailable.

Empirical tail counts use finite rolling z values ending in the same last-512
window: `#(z>1)/n`, `#(z<−1)/n`, `#(z>2)/n`, `#(z<−2)/n`. Comparisons are strict;
equality is not a breach. Each frequency appears beside its one-sided Gaussian
reference (15.8655%, 15.8655%, 2.2750%, 2.2750%). Counts/denominators, dates,
formulas/conventions and limitations are in the hover card. These observations
are overlapping and self-normalized; the Gaussian values are references, not
claimed expected coverage or significance tests. The preceding rolling window
may start before the diagnostic window when loaded history permits it.

Grey styling reflects the **latest closed snapshot across the visible history**;
it is retrospective styling, not a causal historical signal. The z series and
band values themselves remain causal in-window quantities. Small-sample and
zero-variance diagnostics remain absent rather than receiving fabricated values.
ZBAND does not claim to test cointegration, market efficiency, or execution edge.

Offline coverage includes an exact alternating-return VR/z* vector; seeded random
walk versus AR(1) with population VR checks; strict empirical tail counts; short
and constant-return unavailability; forming-mutation invariance; unverified/join
refusals; and worker checks that trending colours are grey, readouts exclude the
forming slot and sample input paints no bands.

### Validation results

All cargo commands exported `CARGO_TARGET_DIR=/workspace/tr-plateau-target`.
Build/test/clippy commands used `CARGO_NET_OFFLINE=true`. The initial read-only
target issue was resolved by adding that directory to the writable roots; no
alternate target was used.

- `cargo test --workspace`: **1,241 passed, 0 failed, 6 ignored**, aggregated
  across 28 test/doc-test suites. The six existing ignored tests are five live
  network checks and the Trend Cone timing benchmark. All 20 new tests run in
  the ordinary suite. New tests use only fabricated in-memory fixtures; the
  existing full suite also contains local loopback HTTP fixtures, with no live
  market or brokerage requests.
- `cargo clippy --workspace --all-targets`: exit 0. Existing workspace warnings
  remain; no warning locations fall on added lines or in either new math module
  or the new Tune pane. The new range-loop warning was fixed, and the modified
  indicator admission branch was simplified as clippy requested.
- Timing: the offline data test binary's
  `tstack_tune::tests::grid_timing_7_by_7_on_5000_bars` reported **984.1 ms** for
  one cold **7×7 grid on 5,000 synthetic test bars** (4,960 common warmed bars),
  in the ordinary debug/test build. This includes grid calculations, nulls, FDR,
  robustness and smoothing, and excludes data loading and UI paint. The report
  cache was bypassed for that measurement. This is a measured fixture result,
  not a guarantee for all hardware, symbols, intervals or concurrently open panes.
- Both new math modules and the Tune pane were rustfmt-checked;
  `git diff --check` passes. No application GUI or application server was run.

The first compilation found an egui keyboard API rename, and the first headless
paint tests needed explicit texture-delta clearing; both are fixed. Honest Z's
new clock metadata also required updating the existing ticker-switch test
harness. The final complete suite and clippy checks above include those fixes.
No git add, commit, push, secrets, deployment, or new network service was used.

