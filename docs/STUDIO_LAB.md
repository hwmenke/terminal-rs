# Regression Lab

`STUDIO lab` fits a multi-factor OLS on verified real data and reports it with the same receipts
and MathCards as the other Studio lenses. It is a descriptive and out-of-sample-scored model,
not a trading signal. There are no journal, alert, trade-plan or execution features here.

`LAB` is a listed ticker, so bare `LAB` still loads the LAB equity. The Lab is reached through
`STUDIO lab …`, the sidebar row "Regression Lab" and `NEW LAB` (current symbol).

## Commands

```text
STUDIO lab                                         # Lab pane, focused symbol, default factor (SPY, or QQQ for SPY)
AAPL STUDIO lab 5m factors=SPY,TLT                 # symbol + interval + factors
STUDIO lab factors=SPY,TLT:change:5:1              # compact factor SYM[:transform[:h[:lag]]]
STUDIO lab factor=TLT transform=change h=5 lag=1   # edit group: edits a focused Lab, else opens one
STUDIO lab target=logreturn:5 covariance=hac-auto holdout=20%
STUDIO lab remove=TLT
```

Grammar (words after `lab`, keys case-insensitive; `tr_data::studio_lab::LabArgs::parse`):

- `<tf>` at most once; daily or intraday, as everywhere in Studio.
- `target=T[:h]`, `factors=f1,…` (replaces the whole list), `factor=SYM` opens an edit group in which
  `transform=`, `h=` and `lag=` may each appear once, `remove=SYM`.
- `covariance=classical|hc3|hac5|hac-auto` sets the *display* default only; all four are always computed.
- `holdout=N%` with N in 10–40.
- Errors: `transform=`/`h=`/`lag=` with no preceding `factor=` ("use target=… for the target"), more than 7
  factors, a duplicate factor, a factor equal to the target, lag > 250, h not in {1, 5, 20}, unknown or
  repeated words. `Lab h must be 1, 5 or 20`.

`STUDIO lab` with no further words keeps the legacy route (`StudioRoute::Lab`); with words that parse it is
`Function::StudioLab`; with words that do not parse the command is rejected.

## Model

Transforms are the Relate transforms (`level`, `loglevel`, `change`, `logreturn`; h ∈ {1, 5, 20}). Each factor
has its own transform and lag ℓ ∈ 0..=250. Default target is `LogReturn{h:1}`; default factor lag is 0.

**Grid.** The grid is the *target's own bars*. A factor value at target bar t with lag ℓ is the factor transform
evaluated at target bar t−ℓ, joined by timestamp (UTC date for daily). It is never later than t. A row is kept
only when the target span and every factor span are clean: no expected-calendar break
(`expected_breaks_for`: US cash calendar for cash classes, an explicit UTC grid otherwise), no vault gap, endpoints
exactly h bars apart in the factor's own bars. Missing cells drop the row; nothing is imputed, interpolated or
carried, and drops are counted by reason ("TLT missing timestamp", "TLT lag crosses break", …).

**Fit.** Training rows only; k = p + 1. Columns are standardised (training μ, population s). Pivoted
Householder QR (largest remaining column norm, ties to the lowest index). Rank = #{|R_ii| > 1e-7·|R_00|};
rank < k → *unavailable*. κ∞(R) = ‖R‖∞‖R⁻¹‖∞ > 10⁶ → *unavailable*. Constant columns, n ≤ k and non-finite
values are also refused, never silently dropped. Coefficients are un-standardised to original units.

**Inference.** `regression_covariance` runs on the standardised design (the raw level columns would look
collinear with the intercept) and the covariance is un-scaled with V = T·V_z·Tᵀ. Four estimates are always in
the report: Classical, HC3, HAC 5 and HAC auto. With overlapping horizons (H = max of target and factor h > 1)
HAC maps to the uniform (H−1) kernel; the covariance label prints the lag/kernel and any fallback. The chips
only select which block is shown. p = 2(1−Φ|z|); q = Benjamini–Yekutieli over the slope p-values (the
family "LAB slopes β_j=0 (all requested factors)"; unavailable members enter at p=1; the intercept is outside).

**Holdout.** Chronological: the first ⌊N·(100−pct)/100⌋ complete rows train; the split bar is the first
test bar. Training rows whose *target* support overlaps the first test target are purged (≤ h_y−1 rows; no
test-side embargo). Coefficients, μ, s and ȳ_train are frozen; R²_OOS = 1 − Σ(y−ŷ)²/Σ(y−ȳ_train)²
(Campbell–Thompson). A fit is labelled "forecast style" only when every lag ≥ h_y; otherwise the OOS tile
reads "contemporaneous fit — not a forecast".

**Minimum observations.** n_train ≤ k: no fit. n_train < max(60, 10k): LOW N — coefficients, σ and R²_in are
descriptive, SE/CI/p/q are withheld. n_test < 20: R²_OOS withheld. The LOW N text names required and available
rows.

**Diagnostics** (training residuals, in-sample): VIF from the explicit R⁻¹, a 41-bin e/σ histogram with an
N(0,1) outline, and a 48×24 residual-vs-fitted count grid (not 5,000 dots).

## Real-data guarantees

- Loading is the bounded shared loader (≤ 4 concurrent, shared (symbol, tf) bar cache) over the archive /
  bridge `daily` and `analysis_bars` paths. Every row passes `require_research_bars` on the full supplied
  series before trimming, caching, alignment or keying; `compute` checks again before keying.
- A requested factor that failed to load or failed admission makes the report *unavailable* and is listed; there is
  no partial fit, substitute peer or fallback source.
- Sample mode makes zero provider calls and clears the report and the previous snapshot.
- Only positive-price series are admitted; level/change transforms on legitimately non-positive futures prices
  need a separate signed-price path that does not exist yet.
- `selected_span` does not filter Lab inputs.
- The fit is a full-sample descriptive fit on the *current* complete rows: "split at {100−pct}% of current complete
  rows; recomputation with new bars moves the split."

## Cache, worker and tokens

`Request::StudioLab` / `Response::StudioLab` carry a pane id and a token from `studio_token()`. A response is
accepted only when the token, the model (`LabSpec::key_string`, which excludes the displayed covariance),
the symbol and the interval match, and provenance is verified. Reports are keyed (`ReportCache`, capacity 8)
by every row's symbol, source, gaps, error, provenance and bar digest in spec order, the ordered factor
triples, the holdout, the target transform, `daily`, `observed_at`, `closed_thru` and `lab.v1`. A correction inside an
old bar, or a factor reorder, changes the key; the pane identity and covariance chip do not. Frozen models stay
in the report; persistence is the Save residual button / `STUDIO resid save` (see STUDIO_RESIDUAL.md).

Size bounds: ≤ 5,000 plotted points, 41 histogram bins, 1,152 grid cells, ≤ 8 coefficients, 4 inference blocks.

## UI

Header, receipt line (provenance, CLOSED THRU, observation clock or "observation clock unavailable"; daily data
labels the clock "archive load time"), a 260 px factor editor (target and factor transform/h/lag, holdout %,
"+ factor" with suggestions, Apply — edits never fit per keystroke), fitted-vs-actual and residual plots with
TRAIN | PURGE | TEST regions, and the coefficient table (β, SE, 95% CI whisker on one shared scale, z, p, q, VIF).
β and z use `theme::sign`; whisker strength is the only significance cue; VIF is the table's single heat
group (sequential slate, key printed). Below 600 px the pane stacks table → OOS tile → plots. Hover any
statistic for its MathCard (`lab.fit.coef`, `lab.fit.sigma`, `lab.fit.r2_in`, `lab.holdout.r2_oos`,
`lab.diag.condition`, `lab.diag.vif`, `lab.diag.resid_fitted`, `lab.diag.residual_hist`, `lab.design.rows`).
A changed model keeps the old report as "PREVIOUS SNAPSHOT" with its own receipt until the new one arrives;
a refusal clears it. Plots keep a per-pane min/max envelope cache keyed by (report key, plot width).

Saved layouts store the symbol, interval and model (`PaneSettings::StudioLab`, every field `#[serde(default)]`).
A legacy `StudioRoute{Lab}` layout restores as a default Lab pane.

## Limits

No leverage plots, skew/Jarque–Bera, bootstrap, or retained point clouds. No categorical factor colours on
the plots. RESID is available (STUDIO_RESIDUAL.md); ATTR stays an unavailable route.
