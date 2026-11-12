# Trend Cone (Edge v2)

`CONE` or **Alt+C** toggles the cone on the focused chart. `AAPL CONE` opens it
for AAPL. `CONE ON` / `CONE OFF` are explicit switches. `CONE 20 5` enables a
20-bar horizon with mean bootstrap block length 5. `TRENDCONE` and
`TSTACK CONE` are aliases. Horizon accepts 1–120 bars; mean block accepts 1–100.
The defaults are 20 and 5, on both intraday and daily charts. Settings save with
the chart layout; older layouts start with the cone hidden.

The fan starts at the **last closed close**, with a saturated regime colour
(up = green, down = red, flat = cyan, using the active theme). Four gradient
strips cover P10–P90, with a stronger P25–P75 centre, a bold median and horizon
price labels for all five quantiles. A grey unconditional fan sits underneath.
The viewport reserves room to the right when enabled. Pan/zoom normally;
**End** or reset restores the space. Automatic price scaling includes visible
cone prices; a manual price scale remains under your control. Future positions
are **bar counts**, not predicted session timestamps. A 20-bar intraday cone
can cross sessions; a daily cone spans 20 subsequent observations.

The card beside the fan shows the regime, horizon, sample count, closed-through
time and **walk-forward nominal → empirical coverage with n** for the central
50% and 80% bands. The grey baseline has its own coverage on the same origins.
Hover the card for the formula, seed, bootstrap settings, baseline path count,
quantile CDF coverage at 10/25/50/75/90%, provenance and limitations.

## Inputs and refusals

- Uses the pane's loaded, native-interval bars, including research-archive daily,
  weekly and monthly bars. No extra history requests or synthetic prices.
- `Bar::require_verified` gates the entire input, including the pending bar.
  Sample and unverified bars are refused even under `TR_PROVIDER=sample`.
  Like EDGE, mixed real/cache provenance, nonascending timestamps and invalid
  completed OHLCV are refused. Uniform verified cache history is allowed.
- `closed_len(tf, bars, class, as_of)` selects the completed prefix. Live charts
  use their last successful feed observation time; archive charts use now.
  Successor bars close their predecessors. Session close, half days, DST and
  calendar intervals follow the existing shared helper.
- The forming bar supplies neither TSTACK state, path samples, calibration
  outcomes nor anchor price. Revising only its price does not recompute the cone.
- Fewer than 30 completed paths in the current regime produces a clear
  insufficient-history message. There is no unconditional replacement for a
  missing conditional cone. Shorten the horizon or load more verified history.
  A warmed-up TSTACK state is required. Coverage may remain unavailable (`n=0`)
  while the forward fan already has sufficient training paths.
- Missing observations are not filled or interpolated. A horizon is a count of
  actual loaded observations; gaps and overnight moves remain in the returns.

## Math

Let the closed bars be indexed `0..N`, last closed index `a=N−1`, horizon `H`,
and `s_i` be the TSTACK medium regime at close `i`. TSTACK is computed causally
on the closed prefix using the chart's Adaptive Trend parameters and session
reset setting. States with nonfinite confidence are warm-up and excluded.

A completed forward path originating at `i` is the vector

```
R_i(k) = ln(C_(i+k)) − ln(C_i),  k = 1,...,H
```

Only origins with `i+H ≤ a` enter. The conditional library contains rows whose
origin regime equals `s_a`; the grey library contains all warmed-up origins.
No later state is used to select a path.

The **stationary bootstrap of Politis–Romano (1994)** resamples these whole path
rows, ordered by their historical origin, separately within each library:

1. Start at a uniformly drawn row index, using the existing SplitMix64 RNG.
2. Draw block length `B ~ Geometric(p=1/L)` on `{1,2,...}`; `E[B]=L`.
3. Copy consecutive row indices, wrapping circularly at the library's end.
4. Restart uniformly at each block boundary until 2,048 rows have been drawn.

Conditional adjacency means consecutive eligible rows in that regime's library.
Each drawn row retains its entire observed H-step path. This is a **predictive
path distribution**, not a bootstrap distribution of sample means. It does not
splice independent increments together or scale volatility by square-root time.
Block length controls dependence in the resampled row sequence; marginal
predictive quantiles converge to the empirical library quantiles with more draws.

For each step, sort the sampled cumulative log returns. Quantiles use linear
interpolation at `(draws−1)q` for `q ∈ {0.10,0.25,0.50,0.75,0.90}`. Plot

```
P_q(k) = C_a × exp(Q_q[R*(k)]),  P_q(0) = C_a
```

The seed defaults to hexadecimal `5452454e44434f4e`; the unconditional library
uses a separate deterministic stream. No time-dependent random seed is used.
Per-step quantile ordering is guaranteed; these are pointwise bands, not a joint
80% probability that an entire future path stays inside the fan.

## Walk-forward coverage: no look-ahead

Evaluation origins sit on a fixed H-bar grid from the start of loaded history,
so scored outcome windows do not overlap. Use up to the latest 64 completed
origins whose regime matches the current regime. At each close `t`:

1. The expanding training library contains only origins `i` with `i+H ≤ t`.
2. Select paths by the state known at their origin; require 30 conditional rows.
3. Bootstrap using the configured seed XOR the origin timestamp, without any
   future-dependent seed or calibration. Compute horizon quantiles in log space.
4. Once `t+H` has closed, compare its realized log return to the forecast bounds.

For central 50% and 80%, `coverage = inclusive band hits / n`. The card prints
nominal versus empirical, with the scored count n. Hover also gives
`count(realized ≤ Q_q) / n` per quantile edge. Ties count as inside a band and
below an edge; flat data can therefore have 100% coverage at every edge.
The baseline uses the same evaluation origins and availability mask.
Appending data cannot change a retained historical forecast at a fixed origin.
Paging older history changes the grid's start and the training library.

These are historical calibration scores, not promises. Training paths overlap
and remain dependent; 30 paths does not imply 30 independent observations.
Small calibration counts, regime changes and limited provider history can
produce unstable coverage. There is no fitted coverage adjustment or invented
score when no completed evaluation exists.

## Execution and cache

`Request::TrendCone` / `Response::TrendCone` carry a full input key. TSTACK,
path construction, stationary sampling, quantiles and walk-forward scoring run
in `spawn_blocking`, with no network. The chart memoizes the input fingerprint
by bar-Arc identity. The key includes generation, timeframe, closed count,
all completed OHLCV/timestamps, every bar's provenance, cone settings and
Adaptive Trend parameters. A forming-only price tick reuses the result; an old
price correction, provenance downgrade, parameter edit or closed-bar transition
invalidates it. Changed requests are throttled to 300 ms, with old geometry
removed immediately. Stale responses are discarded.

Meshes and labels cache by report revision, viewport, scale, rectangle and theme.
Painting performs no bootstrap, TSTACK calculation, I/O or provider access.

## Verification

Offline synthetic tests cover deterministic seeds, hand-counted band/CDF coverage,
exact flat-path walk-forward coverage, forming-bar exclusion on intraday/daily,
verified-data refusal, older price/provenance cache invalidation, quantile ordering,
origin-regime conditioning, purged non-overlapping walk-forward forecasts, and
geometric block means/survival/circular wrap. Worker and headless chart tests
cover refusal without numbers or fan geometry, unchanged-input caching, stale
responses, two gradient fans with horizon labels and coverage, toggle behaviour,
commands, hotkey uniqueness and backward-compatible layout serialization.
