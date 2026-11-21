# Relationship Studio

`CORR` (alias `RBETA`) opens Relationship Studio for the focused security.
`AAPL CORR` uses SPY; `AAPL CORR QQQ` changes the benchmark.
`AAPL CORR QQQ 1H` uses intraday bars; `CORR 30m` keeps SPY.
Daily is the default. The Asset and Benchmark / interval fields and Reload / Apply
buttons recompute the report. The pane and its inputs survive workspace saves.

`CORR` and `RBETA` are also listed tickers. Only the bare word or the word plus
valid CORR arguments (`CORR QQQ 1H`) opens the studio; `CORR US Equity`,
`RBETA Equity` and any other arguments (`CORR 1W`) load the ticker instead.

All series (asset, benchmark, peers) load **concurrently**, at most four at a
time, and successful loads are cached per (symbol, interval) for 15 minutes
(daily) or one bar clamped to 1–10 minutes (intraday), so re-opening CORR is
instant. Failures are never cached and a failed peer shows as unavailable;
nothing is substituted. Reload bypasses the cache.

The pane shows full-sample OLS, a return scatter with the latest 20 observations
highlighted, rolling 20/60/120-return beta and correlation, log price ratio and its
60-price z-score, cumulative regression residual return, and a correlation matrix.
Charts have gridlines, axes and hover values. The matrix scrolls horizontally in
narrow docks and shows the observation count for every cell.

## Data admission

Daily bars follow the STATS research path: the local Yahoo daily archive, then the
configured verified Whats-News bridge when local history is absent. Intraday bars
use the existing vault-aware `analysis_bars` / `history_sourced` path. A fresh fetch
can extend the local window; failed fetches do not advance its observation clock.
The completed prefix uses `closed_len` and that clock. Daily archive bars use the
archive session close policy (including its grace period).

Entire source series must pass `stats::require_research_bars`, including bars
outside the eventual intersection. Sample, unverified, mixed, nonfinite,
nonpositive-price and duplicate/unordered input is refused. No fill-forward,
interpolation or sample fallback occurs. Explicit sample display mode shows an
empty state even when there is a local archive. Matrix peers that cannot be read
remain visible with an unavailable explanation; no substitute observations enter.
Sources are available by hovering the report header.

Intraday bars join on exact common closed timestamps; daily bars join on the
UTC calendar date (two sources may stamp the same session at different times of
day). Log returns are differences between adjacent common log prices, except
that no return spans a Bar Vault UNVERIFIED JOIN gap in either series; when such
a gap lies inside the common range the pane shows an amber warning. A missing timestamp therefore makes the next
return span the same longer interval for both names. Calendar/timezone differences
can eliminate all overlap. At least four common prices are required. Dropped
noncommon prices are counted in the header. The matrix uses separate pairwise
intersections: counts and return spans may differ across cells, so the matrix is
not necessarily positive semidefinite.

## Definitions and limits

For asset log returns y and benchmark log returns x, OLS fits y = α + βx + ε
with an intercept. Beta = centered covariance / benchmark sum of squares;
correlation is Pearson's r, R² = r². Alpha annualises linearly (a log-return rate).
Classical coefficient standard errors use s² = SSE / (n−2):
SE(β) = sqrt(s² / Sxx), SE(α) = sqrt(s²(1/n + mean(x)²/Sxx)).
The displayed alpha SE is per observation. T-statistics divide coefficients by
these SEs. Residual annual volatility is s sqrt(periods/year). These legacy fields remain ordinary homoskedastic, independent-error estimates.
The separate selected-SE tiles and fan use the covariance methods below. No
risk-free rate or predictive significance is claimed.

Annualisation uses the same convention as Distribution Studio: daily 252 (crypto
365); intraday US cash assets 252 × ceil(390 / interval minutes), counting the
short final bucket (7 hourly bars per session); crypto 365 × 24h / interval;
other assets 252 × 24h / interval. Irregular
intersection gaps, missing bars and non-US markets can make this approximation
unsuitable. Daily data for non-US securities can also have different calendars.
There is no adjustment performed here: corporate-action/adjustment conventions
are inherited from the admitted source. Cross-source adjustment mismatches are
possible; inspect the source labels before interpreting a relationship.

Up/down capture is 100 × sum(y) / sum(x) on observations where x > 0 / x < 0.
Upside/downside beta refits an intercept on each subset (at least three returns).
Flat benchmark observations belong to neither subset. Undefined values show `—`,
including zero-variance correlations and t-statistics with zero standard error.
The scatter fan is the selected covariance’s pointwise 95% mean-response CI,
`ŷ(x) ± 1.96 sqrt((1,x)V(1,x)ᵀ)`. It does not include residual variance and is
not a prediction interval.

The ratio panel uses log(asset close) − log(benchmark close). Its z-score
compares today's ratio with the mean and sample standard deviation of the
**previous** 60 common prices (current observation excluded, so no point uses
itself or later data). It is not a stationarity or cointegration test. Cumulative
idiosyncratic return is exp(sum(full-sample OLS residuals)) − 1, starting at zero.
It uses the full-sample fit (future information for earlier points); the terminal
sum is approximately zero because OLS includes an intercept. It is descriptive,
not a trading-strategy backtest. Overflowed residual paths remain unavailable.

The fixed matrix set is SPY, QQQ, IWM, TLT, GLD, USO and UUP, plus the selected
asset and benchmark, deduplicated. Unavailable peers stay in the matrix. Every
cell requires at least three common returns and nonzero variance.

## Validation

Deterministic tests cover known OLS coefficients, R²/correlation, standard errors,
t-statistics, captures, rolling windows, subset betas, z-scores, cumulative
residuals, disjoint/misaligned timestamps, too few observations, invalid/NaN
prices, provenance refusal, zero variance, missing matrix peers, command aliases,
settings persistence, and stale/unverified response refusal. Tests perform no
network requests. No dependencies were added.

## Robust relationship fan and inference kit v2

The SE chip selects **Classical**, **HC3**, **HAC Bartlett fixed 5**, or **HAC
Bartlett automatic**. Default is fixed-5 HAC. These ephemeral controls and the
1/5/20-observation return-horizon chips do not change saved layouts/preferences.
The original one-observation OLS point estimates and classical errors are unchanged.
Signed coefficient numbers use sign ink; covariance/series identities use categorical
colours. No significance hue is used. The matrix retains its printed [−1,1] key.

For design rows z=(1,x), residual e and score g=ze, let A=Σzzᵀ. The covariance
is V=A⁻¹SA⁻¹. Bartlett HAC uses S=Σggᵀ+Σ(l=1…L)(1−l/(L+1))Σ(g_t g_(t−l)ᵀ+
g_(t−l)g_tᵀ), with L=min(5,n−3), or min(floor(4(n/100)^(2/9)),n−3).
HC3 uses S=Σzzᵀ e²/(1−h_ii)², h_ii=zᵀA⁻¹z. There is no extra small-sample
multiplier on these robust sandwiches. The generic inference kit also supports
k-column designs; the two-column rolling path uses the same arithmetic with
bounded allocations. Methods follow Newey–West (1987/1994), MacKinnon–White
(1985), and Hansen–Hodrick (1980).

Five/twenty-observation log returns use overlapping common-price windows, omitting
every window spanning a vault join. Default overlap covariance is the uniform
kernel through h−1 lags. A non-PSD uniform meat falls back to Bartlett at the same
bandwidth, explicitly labelled. Classical and HC3 remain selectable comparisons
and do not account for serial overlap. Annualisation uses periods/year divided
by h. “Bars” means common observations: irregular missing timestamps can lengthen
elapsed time. Matrix peer correlations keep the original one-observation definition.

Selected coefficient tiles show θ±1.96 SE. Inference needs n≥30 and nondegenerate
residual/coefficient variance. Singular/flat/sparse input stays unavailable. The
two-sided p uses a standard normal approximation; the **fixed m=2 BY family** is
alpha=0 and beta=1, with unavailable p=1. Every covariance/horizon report declares
the same two hypotheses before selection. Repeated comparisons across settings,
horizons, symbols and dates are outside this displayed family. Classical errors
remain unchanged; the newly displayed classical intervals/tests are normal
approximations, not finite-sample Student t tests.

The 60-return rolling beta band uses the selected method and only the latest
1,000 endpoints. Windows crossing joins are withheld and adjacent polygons break
there. Appending observations cannot change an earlier retained window. The
rolling MathCard names its own window bandwidth; these are pointwise descriptive
bands with no significance badges. Legacy 20/60/120 curves are retained and gap
windows withheld. Ratio z60 also withholds windows crossing a join.

The **holdout** fits the first floor(0.8n) observations, freezes alpha, beta and
training mean, then scores the remaining observations. `R²_oos = 1 − SSE_test /
Σtest(y−mean_train)²`; negative values read **no skill**. It needs at least 30
training (after the purge) and 10 test observations and a nonzero test baseline. No
refitting or parameter selection uses the test set.

**Purge (correction, slice 5).** At h > 1 consecutive pairs overlap by h−1 bars. The
last h−1 of the nominal 80% training pairs share bars with the first test pair, so they
are now **purged** from the fit and from the training mean (`Holdout::purged`, shown as
"train | purged | test"). The test block and `split_ts` stay at the nominal boundary.
At h = 1 nothing is purged and results are unchanged. Published h = 5 and h = 20 OOS R²
values from earlier builds were computed without the purge and will move (the earlier
split let training pairs share bars with the first test pair); h = 1 values, all in-sample statistics and all rolling
bands are bit-identical.
The train/test strip is descriptive, not an execution or strategy backtest.

The **Theil–Sen** slope is the exact median of finite pairwise slopes on at most
the latest 2,000 observations, excluding equal-x pairs. It is a descriptive robust
cross-check with no p-value or interval. This bounded fit is worker-only.

Reports are immutable Arcs in a bounded ReportCache. Keys include full admitted
closed-bar digests, benchmark, peer errors, source/provenance, observation clock,
gaps, interval, horizon and method parameters. Complete admission precedes cache
reuse. Sample/refusal clears results and stale generations cannot overwrite the
current pane. All new statistic MathCards have stable titles/ids, formulas,
inputs, n, source, span, CLOSED THRU, methods and unavailable explanations.

Tests include direct 2×2 HAC and hand HC3 references, h=3 uniform variance,
non-PSD fallback labelling, fan transformation, unchanged OLS/classical errors,
chronological frozen holdout, Theil–Sen outlier resistance, overlap horizons,
causal rolling prefixes, gap breaks, cache invalidation and actual hover cards
with finite narrow/wide geometry. See PERF.md for measured gates and limitations.

## Studio shell route

`STUDIO lens=relate benchmark=QQQ` embeds the same CORR state and immutable Arc
report below a native chart. No regression or covariance implementation is copied.
The shell's context inspector owns symbol, interval and benchmark; the existing
horizon/method views retain their conventions. An accepted Relate roundtrip
reuses the exact Arc with zero compute requests. Globally allocated request
tokens isolate recreated children and context changes. See [Studio](STUDIO.md).

## Relate transforms and fair-value bands (Studio slice 4)

```text
STUDIO lens=relate benchmark=QQQ transform=logreturn h=5     # h ∈ {1,5,20}
RELATE transform=level|change|logreturn|loglevel [h=1|5|20]
RELATE beta=rolling|expanding   RELATE bands=on|off   RELATE matrix=common|pairwise
```

`RELATE` with arguments updates the focused Studio's Relate inputs; bare `RELATE`
opens `STUDIO lens=relate`. Invalid arguments and `RELATE US Equity` fall through to
the ticker. `CORR` / `RBETA` are unchanged: `relate: None`, pairwise matrix and the
legacy common-timestamp returns; their JSON export gains no key. Inside a Studio,
`h=` belongs to Relate only when the same command says `lens=relate` (`lens=when h=6`
still routes to When, and h outside {1, 5, 20} is refused).

**Strict convention.** Relate aligns as legacy `aligned()` (daily on the UTC date,
intraday on the exact timestamp) and keeps each side's bar index. A transform at
`t` over `h` exists only when both series' own bars are exactly `h` apart, no
expected-grid break (`expected_breaks_for`: supported cash calendars, an explicit
UTC grid otherwise) lies inside `(t−h, t]`, and no vault gap is spanned. Invalid
points are `None`, the next valid point has `break_before`. Receipts print which
convention applies; unavailable stays unavailable (no interpolation or carry).

| Transform | Target `(y, x)` | Unit |
| --- | --- | --- |
| Level | `(P_t, X_t)` | native |
| Log level | `(ln P_t, ln X_t)` | ln price |
| Change h | `(P_t−P_{t−h}, X_t−X_{t−h})` | Δ native |
| Log return h | `(ln P_t/P_{t−h}, ln X_t/X_{t−h})` | log-return |

**Frozen training fit.** OLS on the chronological first 80% of valid pairs
(`train_n = 4n/5`), σ = √(SSE/(n−2)) in target space; refused when n_train < 30, the
benchmark has no variance, or σ ≤ 64·ε·max|y|. Points after `train_end` are out of
sample; earlier points are drawn dimmed and labelled *in-sample descriptive*. The h−1
rows right after `train_end` (their h-bar support overlaps the fitted prefix but they
were not fitted) are a third **embargo** state (`FairValuePoint::embargo`; lighter dim,
card text "embargo … not fitted"). **Correction (slice 5):** they were previously
labelled in-sample; numbers and bands are unchanged, only the label and dimming moved.
Bands add
`kσ` (k = ±1, ±2) **in target space and then invert**: level `ŷ+kσ`; log level
`exp(a+b ln X+kσ)` (a conditional **median**, no σ²/2 term); change `P_{t−h}+a+bΔX+kσ`;
log return `P_{t−h}·exp(a+b r^X+kσ)`. Residuals are never cumulated. Change and
log-return fair values use the realised benchmark move to `t`: contemporaneous, not a
forecast. Hover (MathCard `relate.fair_value`) distinguishes the residual band (±kσ
only) from the mean-response CI `ŷ ± 1.96·se_m` and the prediction SE
`√(σ²+se_m²)`, and prints formula, substituted α, β, x, anchor, σ, n, span, source,
observation clock, CLOSED THRU and any unavailable reason. Level and log-level fits
are labelled *descriptive: levels may be nonstationary*. No p, q, colour or cheap/rich
verdict is shown, only the descriptive `z = (y−ŷ)/σ`.

**Expanding β** (`relate.beta.expanding`, descriptive, no uncertainty): one-pass Welford
co-moments, β = Cxy/Cxx for n ≥ 3 and Cxx > n·(16ε)²; a break resets; the latest 1,000
endpoints are kept; it is prefix-invariant. **Common panel** (`corr.matrix.common`): one
complete-case set whose returns are consecutive in every admitted row's own bars, so every
cell shows the same n (printed as “same n=… all cells”).

**Worker and cache.** Everything runs in the existing `research_horizon` blocking task and
rides inside `Response::StudioCorr` as `CorrReport.relate` (≤ 5,000 overlay points, ≤ 1,000
expanding endpoints), so the standalone pane and the hub share one stale-response route.
Relate keys use the `studio_corr_v3` namespace and hash the full spec (band visibility
excluded: it is an overlay toggle), the 0.8 split, the strict grid and the `relate.v1` method
schema on top of every legacy input; `fit_id` hashes the key, `train_end` and the transform.
Legacy `compute()` never builds `relate`. The chart paints the overlay only for an accepted
report with the same symbol and inputs, a fit, and never in Quick-canvas mode; a context change
drops the child on the same frame and a late response with the old token is ignored.

Saved layouts stay backward compatible: `StudioInputs` gains `relate_transform`,
`relate_beta`, `relate_bands`, `relate_matrix`, and the standalone CORR pane gains `horizon`
(F10: it was silently reset to 1) and `relate`, all `#[serde(default)]`.

### Scout fold-ins landed with this slice

F1 cached CORR paint geometry (`Arc<Mesh>` + prepared lines, visibility-gated, hover from cached
screen positions); F2(a)+(b) lean bit-identical rolling fits (`beta_corr`, prefix break count,
`rolling_beta_interval`); F3 the idiosyncratic cumulative path is unavailable at h>1 (overlapping
returns cannot be cumulated); F4 bare `LAB` / `RESID` / `ATTR` load their tickers, the routes
are `STUDIO lab|resid|attr`; F5 `WHEN` / `QUICK` shadow rules; F6 the Backtest pane refuses in
sample mode locally with no worker request; F10 persisted CORR horizon.
