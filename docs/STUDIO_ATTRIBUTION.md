# Studio factor attribution (ATTR)

Splits the move over a chosen window, for a **saved frozen model** (RESID), into

> **actual = drift (intercept) + Σ factor contributions + residual**

exactly (to floating-point rounding), in target units. The model is never refit and no data are
loaded: attribution is a pure function of the saved `FrozenFit`, its scored `ResidualReport` and the
window. It answers how much of the move the frozen exposures account for; it makes **no causal claim**.
Real verified data only: sample / offline mode clears the pane and sends nothing.

## Commands

```text
STUDIO attr                                   focused RESID model if any, else the saved-model list
STUDIO attr model=AAPL-v-SPY-v1               latest complete move (obs=1)
STUDIO attr model=AAPL-v-SPY-v1 obs=20        last 20 target observations
STUDIO attr model=AAPL-v-SPY-v1 from=2026-03-02 to=2026-03-20
STUDIO attr from=2026-03-02T14:30 to=2026-03-02T20:00   (focused RESID / ATTR model; UTC)
STUDIO screen=attribution …                   alias
ATTR model=… | obs=… | from=… | to=…         alias (first word must be a key)
NEW ATTR                                      empty pane
```

`ATTR`, `ATTR US Equity`, `ATTR 1h` and `ATTR DIST` still load the ticker; an `ATTR` key word with bad
arguments is an error, never the ticker. `obs` is `1..=1000`; `from` and `obs` are exclusive; each key
once. `YYYY-MM-DD` means that UTC day at 23:59:59; `YYYY-MM-DDTHH:MM[Z]` is that UTC minute. Bounds
resolve **on or before** on the scored grid and the resolved stamps are printed. A focused
`STUDIO attr obs=…` changes only that pane's window (one new attribution, no rescore).

## Modes and the identity

* **Sum** (target Change / LogReturn, horizon h): a stride-h chain of rows `t_i = b − i·h`,
  `a = b − m·h`, so `A = Σ y_t` telescopes to the target's move over `(a, b]` (summing all
  overlapping rows would count each bar up to h times). `b − a` must be a multiple of h and every
  chain row must exist; otherwise the window is **unavailable** with the exact reason (nothing is
  interpolated, carried, skipped or compressed). Drift `c₀ = β₀·m`.
* **Endpoint** (target Level / LogLevel): `A = y_b − y_a`, `d = (0, x_b − x_a, …)`; no drift.
* `c_j = β_j·d_j`, `Â = Σ c`, `R = A − Â`. Cross-checks: `R = Σ e_t` (or `e_b − e_a`) and `Â = Σ ŷ_t`
  from the scored report, to 1e-12·max(1,|A|).
* Each factor's *move* column is `from → to` when the summed regressor telescopes to one move, else
  `N/A: <reason>` with a caution tick (horizon mismatch, a level inside a sum, a return inside an
  endpoint difference); its contribution still reconciles.
* Drift share `|c₀| / (|c₀| + Σ|c_j|) ≥ 0.5` shows DRIFT-LED. There is no "% explained".

## Units

Native target units in the report; ×1e4 only at display: log targets `log bp`, Change `Δ price`, Level
`price`. Log targets also print `simple = expm1(A)` as a comparison; log contributions are never
converted one by one.

## Uncertainty (both computed; the toggle only switches the display)

* **Coefficient SE** `√(dᵀVd)` for each stored covariance `V` (all cross-terms). Per-term SEs
  `|d_j|√V_jj` are **not additive**. The estimator label is the one stored at save time (HAC / overlap
  fallbacks included); Classical and HC3 are comparisons that ignore serial dependence. Models saved
  before slice 7 show "coefficient SE unavailable … Refit in Lab and save a new version".
* **Predictive SE** `√(dᵀVd + Var_W)`, `Var_W` the window's residual variance from `σ²_saved` (df-corrected)
  and the training residuals' autocorrelation `ρ̂` (never across a training break): Bartlett-tapered
  sum at lags `s·h` (sum mode) or `2σ²(1 − ρ̂(ℓ))` (endpoint). Fewer than 30 pairs → `LOW N`, the
  predictive SE is withheld and the coefficient SE stays. Conditional on the observed factor values
  (factor-forecast uncertainty excluded); assumes the window's innovations are independent of the
  training error and the residual process is stable. No p-value, star or verdict colour.

## IN-SAMPLE / OUT-OF-SAMPLE / MIXED

Rows used are counted by slice-6 segment: all TRAIN → `IN-SAMPLE` (labelled, not refused), all SCORED →
`OUT-OF-SAMPLE`, otherwise `MIXED train/embargo/scored`. Embargo rows are never called out-of-sample.
A window anchored before the scored grid (latest 5,000 target bars) is unavailable.

## Cache and keys

`attribution_key` hashes the scored report's key (every input bar digest, gap, source, closed boundary
and the model digest), the model digest, the window and the method. No wall clock.

## Pane

Ribbon (FROZEN FIT · attributed, not refit, trained thru, window, label), equation line, fixed 240 px
waterfall (drift, factors, residual, predicted and actual totals; factor j ≥ 6 repeats a colour and
gets a dashed outline), term table, tiles, through-time chart, caveats. Every statistic has a
MathCard (`attr.actual`, `attr.drift`, `attr.contribution`, `attr.residual`, `attr.coef_se`,
`attr.pred_se`, `attr.window`). A window change or reload keeps the old report as PREVIOUS SNAPSHOT.

## Cut list

Cut: per-factor ±SE whiskers on the waterfall; gap-breaking in the through-time lines (endpoint
windows draw through missing rows; the cross-section never does). Everything else in the spec's cut
order was kept. The through-time chart does not decimate by min/max per pixel, only by column sampling.
