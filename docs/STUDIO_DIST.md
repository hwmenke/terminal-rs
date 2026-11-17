# Distribution Studio

`AAPL DIST` opens Distribution Studio; `RDIST` is an alias. `DIST` uses the
focused symbol. `AAPL DIST 1H` or `DIST 30m` selects intraday returns. The pane
supports 1m, 5m, 15m, 30m, 1H, 4H and 1D; weekly/monthly bars are refused.
`NEW DIST 1H` and saved workspaces also restore the pane.

`DIST` and `RDIST` are also listed tickers. Only the bare word or the word plus a
valid DIST interval (`DIST 1H`) opens the studio; `DIST US Equity`, `RDIST Equity`
and any other arguments (`DIST 1W`) load the ticker instead.

**Bar Vault gaps.** If the intraday series spans a vault UNVERIFIED JOIN gap, the
pane shows an amber warning with the gap dates and no return is computed across
the gap bar pair (the regime transition chain also restarts after it).

The ticker, timeframe and 1Y / 3Y / 5Y / ALL window controls send a fresh worker
request. Each request increments a generation; stale responses cannot overwrite
newer selections. Ticker edits apply on Enter or Refresh. Settings save the ticker,
timeframe and window, never computed prices or credentials.

## Real data and coverage

Daily data follows STATS: local Yahoo research archive first, then the configured
Whats-News archive when local history is absent. Intraday data uses the existing
vault-aware loader (`analysis_bars` → `history_sourced`), stitching verified bars
from a single source with fresh provider history. Cached real bars are allowed;
sample, missing-provenance and contaminated inputs are refused before trimming.
An invalid populated archive is never replaced with another source to hide it.
No prices are fabricated, interpolated, filled forward or fetched in tests.

Sample / demo display mode shows an empty state. A failed load clears old results.
The pane shows provenance, source, observation time, actual date range and counts.
ALL means all history returned by the archive or bounded vault loader, not all
history that exists anywhere. Intraday vault retention and provider depth limit
coverage, particularly at 1m; a requested 5Y window may contain much less data.
The source line includes vault holes/missing sessions when available. Returns
span consecutive available closes, including overnight gaps and missing-bar gaps.
They must not be read as uniformly spaced intraday observations.

Only completed bars are analysed. Daily archive bars use the existing session
close/grace rule. Intraday closure uses the fresh feed's observation clock; a
stale or vault-only result withholds its final bar even if wall time has advanced.
Full input passes `require_research_bars` before any window or closure trimming;
invalid OHLCV, nonpositive closes and nonascending/duplicate timestamps fail.
At least five closed bars (four returns) must remain. Window boundaries use
calendar years measured back from the latest closed bar. The first selected close
anchors the first return; a close before the boundary is not pulled into the sample.

## Statistical conventions

- Returns are `ln(close[t]) - ln(close[t-1])`, labelled by the ending bar timestamp.
  Percent displays are log-return percentages; drawdowns alone use simple price loss.
- Mean is per bar. Standard deviation uses the sample denominator `n−1`; annual
  volatility is sample stdev times the square root of nominal periods per year.
  Daily equities/other assets use 252; crypto uses 365. Intraday US cash assets use
  252 × ceil(390 / interval minutes), including the short final session bucket;
  crypto uses 365 × 24h / interval; other intraday assets use 252 × 24h / interval.
  Annualisation is an approximation, especially with gaps, shortened sessions or
  irregularly available bars. The factor is displayed explicitly.
- Skew and excess kurtosis are population central standardized moments. These
  descriptive estimates are not small-sample bias corrected. Jarque–Bera is
  `n/6 × (skew² + excess²/4)`; its p-value is `exp(−JB/2)`, the asymptotic χ²(2)
  survival probability. Small samples do not justify a normality conclusion.
- VaR 95/99 is minus the interpolated 5th/1st percentile (type-7 quantile).
  CVaR (expected shortfall) is minus the mean of every return at or below that
  same quantile, so CVaR ≥ VaR always. With very small samples CVaR can equal
  the worst observed return. Negative VaR/CVaR means gains, not an absolute loss
  floor. The horizon is one closed bar of the selected interval; nothing is
  scaled by √h to other horizons, and no Cornish–Fisher or parametric
  extrapolation beyond observed tails occurs.
- Tail ratio is `abs(q95) / abs(q05)`; a zero denominator is unavailable.
- Beyond ±2σ / ±3σ reports shares below/above the mean using population σ,
  alongside the theoretical normal total expectations 4.550% / 0.270%.
- The 40-bin histogram counts all observations, including the maximum in the last
  bin. The normal and Student-t curves are theoretical fitted densities scaled
  by `n × bin_width` to compare with counts; they are not generated market data.
  The Student-t method-of-moments fit uses `df = 4 + 6/excess`, and rescales t to
  match population variance. Nonpositive excess kurtosis has no such fit.
- QQ coordinates compare normal quantiles at `(rank + 0.5)/n` with returns
  standardized by their population mean/σ. The reference is 45°. The outer 2.5%
  of observations in each tail are highlighted. Normal quantiles use a deterministic
  rational approximation. Hover shows exact chart coordinates or histogram counts.
- Zero variance yields zero vol but unavailable standardized moments, JB, QQ,
  density curves and sigma exceedance shares. Undefined metrics show `—`.

## Drawdowns and regimes

The underwater curve marks each close's loss from the running closing-price peak.
The table lists the five deepest **nonoverlapping** peak-to-recovery episodes,
including an unrecovered episode when applicable. A close equal to the peak
recovers the episode. Duration runs from peak to recovery (or latest selected
bar), in both observed bars and elapsed calendar days; it is not trough duration.
Drawdowns are reset at the start of the selected window.

Realized volatility is the annualised sample stdev of the latest 20 log returns.
Regimes have **no look-ahead**. A return's regime compares the volatility of the
**preceding** complete 20-return window with **expanding (point-in-time)
terciles** of every rolling volatility observed up to that point; no later bar
can change an earlier label (tested by prefix invariance). Thresholds need 20
rolling vols, so the first 40 returns have no regime. The vol chart draws the
thresholds as they evolved; "Latest thresholds" are those in force now. Equal lower/upper thresholds are labelled Normal rather than inventing a
split. The ribbon above the drawdown chart is grey during warm-up; the vol chart
shows the actual rolling values and tercile levels. Regime tables include counts,
mean return, annualised sample stdev and positive-return share. The transition
matrix reports adjacent classified-return counts and row-normalized percentages;
rows with no transitions show `— · n=0`.

Per-regime statistics only use returns realised after their label was fixed.
They remain descriptive history, not a trading recommendation or forecast.
The top/bottom return table allows k = 1…10 and shows the available observations.
Charts use theme colours, labelled axes, gridlines and hover readouts. Deep histories
use extrema-preserving line reduction, sampled QQ ranks and a pixel-resolution
regime ribbon; full observations remain in calculations and hover lookups. They stack
in narrow panes; long tables scroll horizontally.

## Validation

Offline deterministic tests cover known moments/JB/tails, fractional shortfall,
normal quantiles and Student-t density, histogram mass, log returns, recovery and
ongoing drawdowns, regime lag/counts/transitions, calendar windows, flat prices,
insufficient input, invalid floats/prices/timestamps, sample/missing/mixed bars,
command parsing, pane settings, generation/provenance gates, pending-bar removal, sourced-vault loading / outage
closure, sample-mode transitions and finite headless chart geometry at 320/1440px.
No new dependencies are required.

## Regime fingerprints

Select **Regimes** beside Overview. Calm / Normal / Stressed retain the existing
causal labels, joined to returns by timestamp. Warm-up and vault gap exclusions
are printed separately. The loader, full-input verification, closure clock and
sample refusal remain unchanged. CLOSED THRU names the final analysed bar.

Three ridges share 48 edges over ±W, where W is the larger absolute type-7
0.5th/99.5th percentile of classified returns, and one maximum bin-mass scale.
Mass uses each regime's n; observations outside the axis remain counted.
Type-7 q05/q25/q50/q75/q95 and at most 20 observations per tail are descriptive,
not confidence limits. Tails count strict exceedances of pooled μ ± 2s, with
sample standard deviation s (n−1). Flat or unclassified inputs show a reason.
Click a regime to select its binned ECDF against other classified returns;
its exact D uses complete tie groups and the smallest maximizing x.

Mean contrast is regime minus its classified complement. Newey–West (1987)
Bartlett HAC uses a declared L=min(20,T−1) on the original bar grid, retaining
missing slots and resetting products at vault gaps. Normal-tail p and pointwise
Δ±1.96 SE require 30 observations on each side and nondegenerate variance.
L=20 follows the volatility label window; results remain bandwidth-sensitive
(Andrews 1991). The three contrasts form fixed BY (2001) family
`dist.regime.mean`; unavailable tests retain p=1.

Distance D has no iid KS p-value. A background cached job circularly rotates
labels against fixed real returns on the compact classified sequence. Shifts
40…m−40 are exhausted or sampled without replacement by seeded partial
Fisher–Yates, at most 499 draws and 12 million observation-draws. Fewer than
99 admissible shifts or draws withholds inference. Add-one p=(1+extremes)/(K+1)
and a 95% surrogate band use the same shifts for all three regimes. A separate
fixed BY family `dist.regime.distance` retains all three slots. Seeds hash the
selected verified bars and gaps. Generation and seed checks reject obsolete
responses; repeated requests share the report cache and make no provider calls.

The time-shift surrogate (Theiler et al. 1992; Phipson–Smyth 2010 add-one rule)
assumes stationarity, preserves each series' dependence and introduces a wrap
seam; vault gaps add seams. A small p mainly reflects volatility persistence,
since the labels depend on past absolute returns. It is not directional edge.
P(r>0) includes zeros in n and carries a Wilson (1927) 95% interval with z=1.96;
that interval assumes independent signs.

Every statistic has a method/source/count/closure MathCard. Categorical series
colours identify the regimes on both tabs. Signed contrast numbers/endpoints
use sign ink; q≤0.10 adds a neutral outline, while LOW N adds a caution tick.
There is no heat surface. Tab/row selection is ephemeral; existing saved settings
remain compatible. Tests pin HAC variances, tie-complete D, Wilson intervals,
BY membership, exact rotation p/q, calibration and real-data/state gates.
