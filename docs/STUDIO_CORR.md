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
training and 10 test observations and a nonzero test baseline. No refitting or
parameter selection uses the test set. Overlapping horizons share up to h−1
boundary prices across this simple split; the UI discloses this limitation.
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
