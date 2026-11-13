# Relationship Studio

`CORR` (alias `RBETA`) opens Relationship Studio for the focused security.
`AAPL CORR` uses SPY; `AAPL CORR QQQ` changes the benchmark.
`AAPL CORR QQQ 1H` uses intraday bars; `CORR 30m` keeps SPY.
Daily is the default. The Asset and Benchmark / interval fields and Reload / Apply
buttons recompute the report. The pane and its inputs survive workspace saves.

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

Only exact common closed timestamps are kept, and log returns are differences
between adjacent common log prices. A missing timestamp therefore makes the next
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
these SEs. Residual annual volatility is s sqrt(periods/year). These are ordinary
homoskedastic, independent-error estimates; no robust/HAC correction, p-values,
risk-free rate or predictive significance is claimed.

Daily annualisation uses 252. Intraday annualisation assumes a US cash session:
252 × 390 minutes / interval minutes (6.5 hourly periods per session). Irregular
intersection gaps, missing bars and non-US markets can make this approximation
unsuitable. Daily data for non-US securities can also have different calendars.
There is no adjustment performed here: corporate-action/adjustment conventions
are inherited from the admitted source. Cross-source adjustment mismatches are
possible; inspect the source labels before interpreting a relationship.

Up/down capture is 100 × sum(y) / sum(x) on observations where x > 0 / x < 0.
Upside/downside beta refits an intercept on each subset (at least three returns).
Flat benchmark observations belong to neither subset. Undefined values show `—`,
including zero-variance correlations and t-statistics with zero standard error.
The scatter band is the fitted mean ±2 residual standard errors, not a confidence
or prediction interval.

The ratio panel uses log(asset close) − log(benchmark close). Its trailing
60-common-price z-score uses sample standard deviation and includes the current
observation. It is not a stationarity or cointegration test. Cumulative
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
