# SCAN: broader, dependable archive scanners

Run `SCAN <setup>` using any existing setup: `DARVAS_BREAKOUT`, `DARVAS_BOX`,
`DARVAS_FAIL`, `NEAR_HIGH`, `VOL_SURGE`, `EP`, `BREAKOUT_QUEUE`, `RSI_OB`, `RSI_OS`.
All nine work with the local Yahoo archive, read-only Whats-News SQLite, and HTTP
history. No market-display cache or sample fallback participates.

The pane starts with stale/undated names hidden. **Include stale / undated** exposes
them with their dates/status. Freshness follows the existing US session calendar,
including holidays, DST, early closes and the archive's 15-minute post-close grace.
It is reevaluated when viewing results, independently of calculation caching.

Use **Strength, volume, range & timeframe filters** to enable numeric bounds and
combine daily, weekly and monthly conditions. Bounds are inclusive; multiple
conditions must all pass. Reset restores defaults. Sort in either direction by
symbol, Rust setup score, 21-bar return, daily RSI14, latest volume, volume ratio or
ADR20. Missing numbers sort last; equal numeric values sort by symbol. Keyboard
selection, Enter/chart linking and watchlist saving follow the displayed order.

**Skipped / excluded symbols** explains missing/invalid/short history, stale dates,
missing metrics and unmet filters. A missing weekly/monthly lookback does not hide
an otherwise valid daily candidate until that timeframe's condition is requested.
Text search omissions and non-matching setups are not data failures.

## Shared Rust calculation contract

Implementation: `crates/data/src/whats_news/scanner.rs`. Both archives and HTTP
history use it for setup membership, metrics, filters and sorting. Minimum daily
coverage is 30 actual bars. There are no shortened lookbacks, padded candles,
substitute prices, or synthetic monthly observations.

| Metric | Definition |
| --- | --- |
| RSI14 | First 14 close changes seed simple average gains/losses; subsequent changes use Wilder 1/14 smoothing. All gains = 100, all losses = 0, entirely flat = 50. |
| Strength | `(latest close / close 21 bars earlier - 1) × 100`. RS competition ranks compare eligible names with the same latest bar date, before setup matching and UI filtering. Ties share rank. This is an archive-universe comparison, not a published rating or SPY-relative line. |
| Change / gap | Latest close / previous close, or latest open / previous close, minus one, in percent. |
| Volume ratio | Latest volume / mean of the preceding 20 bars, excluding the latest. Zero denominator means unavailable. |
| 5/20 volume | Mean of latest 5 volumes / mean of latest 20, including the latest. Kept separate from the original service's identically named field. |
| ADR20 | Mean of `(high - low) / close × 100` over exactly the latest 20 bars. |
| Distance to high | `(latest close / maximum high of latest 20 bars - 1) × 100`. |
| Score | One each for near high, volume surge and EP; add two for Darvas breakout, or one for a confirmed box within 2% of the 20-bar high. |
| Near / surge / EP | Within 5% of the high; volume ratio ≥ 1.5; opening gap ≥ 4% and surge. |
| Darvas | Existing 20-bar high and three-bar confirmation. Confirmation ends before the latest outcome bar so a fresh low cannot move its own box bottom. |
| D/W/M conditions | RSI14 and close ≥ (or <) SMA20. Require 20 actual bars of that timeframe. |

Thresholds use full precision; rounding is display-only. Weekly buckets start on
Monday and monthly buckets on the first calendar day, using calendar years and
actual month lengths. OHLCV aggregation uses first open, maximum high, minimum low,
last close and summed volume. It derives from the full daily archive, never a
260-day tail or stored weekly candles. Incomplete leading/trailing calendar periods
are omitted from W/M conditions; no missing periods are filled.

HTTP requests the service's unfiltered setup rows, then scans registered histories
with the shared Rust contract. Expand a candidate's **conditions / source fields**
to inspect original service score, rank/population, return, change, setup list/count,
volume field, ADR, gap, distance to high, regimes and box state/levels. These retain
service definitions and are labelled separately because seeds, rounding, universe
and box confirmation can differ. Missing service fields display a dash. The table
and numeric filters always use Rust metrics.

## Reads, caching and trust

SQLite reads registered daily histories in a batch within one read transaction;
coverage validation also uses a batch rather than one history read per symbol.
HTTP services expose per-symbol OHLCV, so those reads are bounded to 12 concurrent
requests. A bounded exact-content cache reuses unchanged scans and per-history
calculations; corrections with unchanged dates/counts, deletions, provenance
changes and different histories invalidate reuse. Source labels are attached to
each fresh snapshot outside the calculation cache.

Explicit sample/unverified provenance remains a hard refusal, including short or
invalid histories and supplementary service fields. Numeric/date failures and
insufficient verified history produce named skip reasons. Nothing writes to the
Whats-News archive or changes its ingestion/schema.
