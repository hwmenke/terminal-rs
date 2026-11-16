# ATR Stop Cloud and Stop Odds

`STOPS` shows a stepped ATR envelope around Adaptive Trend's medium baseline
(MB), three ratcheting stop guides and an empirical Stop Odds panel. The cloud
sits under the candles and does not change autoscale. It is chart analytics:
no order, trade plan, journal entry or alert is created.

## Controls and saved settings

- `STOPS`, `SCLOUD`, `TSTACK STOPS`, **Alt+S**, or the chart context menu's
  **Stop Cloud** checkbox toggle the overlay independently of Trend Stack.
- `AAPL STOPS` loads AAPL and enables it. Explicit securities still work:
  `STOPS US Equity`, `SCLOUD Equity`, and `STOPS US Equity 1h` load those tickers.
- `STOPS ON|OFF` enables or hides the overlay.
- `STOPS K 1 2 3` sets one to three strictly ascending ATR multiples, each
  between 0.25 and 6. Defaults: 1, 2, 3. One guide has no inter-guide fill.
- `STOPS H 12 48` overrides the two horizons (1 ≤ short < long ≤ 250 bars).
  `STOPS H AUTO` restores interval defaults.
- `STOPS ODDS OFF` hides the panel and compact chip while keeping the cloud.
  `STOPS ODDS ON` restores them.

Enabled state, multiples, horizon overrides and odds visibility save with the
layout. Old layouts load with Stop Cloud off. Partial settings use defaults;
invalid stored calculation parameters fall back to defaults. The chart's
ADTREND parameters and `TSTACK SESSION ON` apply to Stop Cloud too.

## Formulas

True range is `max(H, previous C) − min(L, previous C)` (first bar: `H − L`).
ATR is Wilder's RMA, seeded with the mean of the first `atr_n` finite values,
then `ATR_i = ATR_(i−1) + (TR_i − ATR_(i−1))/atr_n`. The ADTREND default is 20.
Intraday uses the chart's session ATR, dropping the overnight gap at New York
session starts. Daily and higher intervals use ordinary true range. MB, ATR
and medium regime come from the same base-lane calculation as Trend Stack.
Only states with finite confidence are valid; session warm-up remains blank.

For each multiple `k`, the envelope is `MB ± k·ATR`. In a long regime,
`T_k(i) = max(T_k(i−1), MB_i − k·ATR_i)`; in a short regime,
`T_k(i) = min(T_k(i−1), MB_i + k·ATR_i)`. A regime flip resets the guide to
its raw envelope level. At k=2.25 this is the existing Adaptive Trend MRT.
Flat regimes have an envelope and no stop side.

At the last closed bar c, the current distance is
`d_k = sign·(C_c − T_k(c))/ATR_c`, in ATR units. Past origins use that same
distance: `S_t = C_t − sign·d_k·ATR_t`. This conditions the estimate on the
line currently shown rather than pooling stops with different distances.

For each matured origin t, inspect only bars `t+1..=t+H`. A long touch is any
`L ≤ S_t`, and a short touch any `H ≥ S_t`. Equality and gaps through a level
count. This is a touch probability, not an execution or fill-price estimate.

With the future window's minimum low lo and maximum high hi:

| Regime | MAE | MFE |
|---|---|---|
| Long | max(0, (C_t − lo)/ATR_t) | max(0, (hi − C_t)/ATR_t) |
| Short | max(0, (hi − C_t)/ATR_t) | max(0, (C_t − lo)/ATR_t) |

Median and p80 use linear interpolation on sorted excursions. Touch odds are
`hits/n = count(MAE_t ≥ d_k)/n`. Wilson 95% score intervals use z=1.96:

```
centre = (p + z²/(2n)) / (1 + z²/n)
half   = z·sqrt(p·(1−p)/n + z²/(4n²)) / (1 + z²/n)
interval = [max(0, centre−half), min(1, centre+half)]
```

## Origins, horizons and honest sample counts

Candidates have the current regime sign and a finite, positive origin ATR.
Scan forward; accept t only when `t ≥ last_accepted + H`, and mature it only
when `t+H ≤ last_closed`. Thus the forward windows are disjoint. The grid
uses only information available at each origin. An accepted bad window is
counted as skipped and is never replaced by a nearby origin. Matured windows
from the current episode are included. An episode is a maximal consecutive
run of one sign; the episode count includes only runs contributing outcomes.

| Interval | Short H | Long H |
|---|---:|---:|
| 1m | 15 | 60 |
| 5m | 12 | 48 |
| 15m | 8 | 26 |
| 30m | 4 | 13 |
| 1h | 7 | 35 |
| 4h | 2 | 10 |
| D | 5 | 20 |
| W | 4 | 13 |
| M | 3 | 12 |

Horizons count loaded bars and may cross sessions. The hover card reports
crossing counts, skipped windows and origin-to-outcome date spans. On intraday
equity/ETF/index bars, spacing greater than one interval within the same New
York date is a hole. For other asset classes, any such spacing is a hole.
Calendar closures, DST and half days use the existing chart calendar and
closed-bar rules. Extended hours are included when loaded, labelled in hover.

## Reading the panel

The regime colour identifies the stop side. Each row gives the last closed
guide level, its distance in ATR units and touch odds at both horizons, with a
Wilson interval and a small probability/interval bar. MAE/MFE medians and p80
are below, with n and contributing episode counts. The panel avoids the cone
card, fan and Edge card when a full corner fits; smaller or crowded panes use
a one-line chip. Hover either view for the full math, counts, dates and reasons.

- **THROUGH** in amber means the last closed close is already at or beyond the
  guide (`d ≤ 0`). No probability is claimed.
- **— n=… < 20** or **— episodes=… < 5** means the empirical sample is too
  small. Excursion summaries are also hidden below these thresholds.
- **— ATR is zero** means there is no usable current ATR distance.
- **FLAT** has no stop side; warm-up requests more verified history.
- A computing message appears until the worker returns; a refusal contains a
  reason and paints no cloud, guide values or probability digits.

Hover also includes the reference estimate at `d=k` ("From close") and an
all-overlapping-origins diagnostic, explicitly excluded from the displayed
Wilson interval. Frozen-stop touch odds are a lower bound for a stop that
continues ratcheting along the same path and side.

## Data rules and limits

Only verified real/cached history is accepted. Sample and unverified inputs
are refused, including under `TR_PROVIDER=sample`. Mixed real/cache history
requires the existing valid vault stitch witness in the worker before its
provenance can be normalised. The pure report refuses mixed provenance.
Malformed archived OHLCV is refused by the shared research gate; unusable
windows and intraday holes are dropped by the window evaluator. No prices
are fetched, invented, interpolated or filled by this feature.

The forming bar never enters ATR, regime, guides or outcomes. Its last closed
guide levels continue as dashed provisional lines until closure. Live closure
uses the feed's last observation time, so a stale partial bar stays forming.
Older revisions, parameter changes and symbol/interval changes invalidate
results. Only unchanged closed inputs hit the worker report cache. Statistics
run off the UI thread; paint caches only map report values to geometry.

Disjoint windows remove direct overlap, but volatility clustering leaves
residual dependence. Regime ages are pooled. Wilson intervals describe this
historical sample, not guaranteed future performance. Weekly and monthly
histories often cannot supply enough independent windows or episodes.
