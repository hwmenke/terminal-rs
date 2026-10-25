# Statistics and conditional returns

`STATS` (or `AAPL STATS`) opens a research pane that compares a name's historical
risk and returns. `COND` (or `AAPL COND RSI(14) < 30`) opens the same pane on the
**Conditional** tab so you can see what followed a filter such as low RSI or price
above a moving average.

The pane uses **verified daily bars** from the local Yahoo research archive, then
the connected Whats-News bridge if the local series is empty. Sample, unverified
or display-cache prices are refused. There is no silent fallback.

## Commands

| Command | Action |
|---|---|
| `STATS` | Overview for the focused name |
| `AAPL STATS` | Overview for AAPL |
| `COND` | Conditional tab (baseline = every completed bar) |
| `AAPL COND RSI(14) < 30` | What followed oversold RSI on AAPL |
| `CDIST` | Alias for `COND` |
| `NEW STATS` / `SPLIT STATS` | Add the pane |
| `EXPORT STATS` | JSON with source, as-of date and provenance |

Chips in the pane apply the common filters: RSI OS / OB, close vs SMA(20) or
SMA(50), SMA(20) > SMA(50), and RSI OS plus price above SMA(50). Type any
`left op right` pair (`<`, `<=`, `>`, `>=`). Combine with `AND`. Features look
**backward only** (RSI, SMA, EMA, ROC, KAMA, MACD, OHLCV). Forward returns are
the only place a future bar is read.

## What the numbers mean

### Overview

| Figure | Definition |
|---|---|
| Ann. vol | Sample stdev of daily close-to-close returns × √252 |
| Sharpe | Mean daily return / stdev × √252 (no risk-free rate) |
| Max DD | Worst peak-to-trough on compounded daily returns |
| Avg daily | Mean daily close-to-close return |
| Win rate | Share of days with a positive return |
| N | Completed daily returns (first bar has none) |
| Date range | First and last archived session in the series |
| RSI / vol / KAMA deciles | Mean **completed** next-day or next-5-day return in each decile |
| KAMA crosses | Mean completed 1-day / 5-day return after a close crossing KAMA |
| Seasonality | Mean daily return in that calendar month × 21 (a month-ish total) |

Deciles need enough distinct values; otherwise the table says so.

### Conditional vs baseline

For each horizon **H** (default 5 and 10 trading days):

- **Forward return** on bar *t* is `close[t+H] / close[t] − 1`.
- Only **completed outcomes** count. The last H bars have no close yet and are
  excluded from both the conditional set and the baseline.
- **Baseline** is that same completed forward return on every bar, not only the
  matches. Compare the two columns: the condition is interesting only when it
  differs from the unconditional distribution.
- **N** is the number of completed outcomes.
- **Effective N = N / H**. Overlapping H-day windows share future days, so the
  raw count overstates independent samples. Effective N is the non-overlapping
  equivalent. It is a label, not a discarded sample — every completed match
  still appears in the histogram and the percentiles.

A bar whose feature is still warming up (NaN RSI, short SMA) fails the
comparison and is not a match.

## Provenance

The header shows **REAL / CACHED**, the source string, the as-of date and the
session range. `EXPORT STATS` writes the same fields. A sample-tagged file is
named `SAMPLE` and is not restored into the archive or this pane.

## Out of scope

Backtests and lookalike search are later steps. Adaptive Trend overlays are
documented in [ADAPTIVE_TREND.md](ADAPTIVE_TREND.md).
