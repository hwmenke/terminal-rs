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

## Keys (SCAN / UNIV)

| Key | Action |
| --- | --- |
| ↑/↓ or j/k | Select a row in displayed order |
| Page Up/Down, Home/End | Move through the list |
| Enter | Open selected symbol in the archive chart, retaining list focus |
| N | Open TWALL D of the visible 2–24 names, otherwise only the selected name |
| Shift+N | CLOCK or chart + TSTACK |
| Backspace | Return through a live hand-off stack |
| w / Shift+W | Pick a watchlist group / save to last-used group |

Hand-off is inactive in the archive management/history view. Keys require an empty
command line and no focused text editor. See [Next ▸ hand-off](NEXT_HANDOFF.md).

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

`EXPORT SCAN` writes the current results (source, last-bar dates, provenance, filters).
Sample snapshots are named `SAMPLE` and must stay isolated. Scan JSON is not restored
into SCAN. See [Export and recovery](EXPORT_RECOVERY.md). `EXPORT SCAN` does not cover
SCAN VIEW boards yet (status says so).

The scan calculation cache is a 1024-entry LRU keyed by a 128-bit content key
(FNV-1a digest + an independent SipHash over every bar field and the verified bit, plus
length and last timestamp). It stores no bars, and the lock is never held while a name
is computed. O'Neil relative strength (`rs_spy_pct`, `oneil_rs`) is derived from the
**dated** 63-session relative return (`daily_relative_return`: aligned on the symbol's own
sessions, SPY required at both endpoints), so a stale symbol is never compared with a
newer SPY window; a missing SPY endpoint date is unavailable (`None`/false).
`measure_oneil` remains only as the Python positional parity fixture.

## Views: PATTERN, RSI-C

```
SCAN VIEW PATTERN
SCAN VIEW RSI-C [N=<2..21>] [K=<1..20>]      (alias RSIC; defaults N=14 K=5)
```

Two dense, sortable, heat-coloured boards over the same verified research universe as
SCAN (local Yahoo archive merged with the Whats-News archive; local wins). A view never
goes through the setup scan. `VIEW`, `PATTERN`, `RSIC` typed alone are still tickers.
The pane's scan string is the persisted state (`VIEW RSI-C N=9 K=3`; defaults omitted),
so saved layouts restore the view. One engine board (`whats_news::equity_engine`) carries
the daily/weekly pattern *and* the daily/weekly RSI counter for every row; both views read
it from one content-keyed cache (4 boards, LRU, no wall clock in the key). Compute runs on
the worker in at most four scoped threads, merged deterministically.

**Pattern.** Daily (≥64 bars): C against the *prior* 63/21-bar high/low (windows end at
t−1). Breakout `C ≥ H`; else Breakdown `C ≤ L`; else with `p=(C−L)/(H−L)`: From Bottom
`C ≥ H21 ∧ p ≤ 1/3`, From Top `C ≤ L21 ∧ p ≥ 2/3`. Weekly (≥28 completed weeks, window =
last ≤52 weeks, *including* the last completed week): the same with a 0.1% tolerance
(`C ≥ 0.999·H`, `C ≤ 1.001·L`) and 26-week short window; the row states the actual weeks
used and amber-flags fewer than 52. A flat window is a Breakout with no position.

**RSI Counter.** Reference EWM RSI (α=1/n, first-delta seed, needs ≥n+2 closes, a window
with no loss reads 100) for n = 2…21 on the last 400 daily / 120 completed weekly
closes (≥24 closes, ≥10 valid lengths). This is deliberately *not* the scan's Wilder
RSI14 (SMA seed, flat = 50). Shares are count/valid; counts: <20, <10, >80, >90, 50..75
(up), 25..50 (down; exactly 50 is in both). State = first tier hit in the order
OS → OB → up → down: Extreme ≥90%, Strong ≥70%, Tilt ≥55% of valid lengths, decided with
integer arithmetic; otherwise MIXED. Mean, sample sd, and alignment `1 − min(1, sd/25)`.
D/W agreement = both trend up (or both down); PULLBACK = daily OS inside a weekly up
state; Δn/k = RSIₙ(t) − RSIₙ(t−k) (zero counts as accelerating).

Divergences from the Python reference, on purpose: weekly bars are **completed weeks
only** (the reference resamples the partial week too), and states classify on
**unrounded** RSI (the reference rounds 19.995 to 20.00, which is not <20; the terminal counts [19.995, 20) as <20).

**Trust.** Verified bars only: a Sample bar anywhere refuses the whole board; an
unverified (Missing) history becomes that name's invalid row with no substitution;
sample/offline display mode refuses before any DB or bridge call. Admission is per view
and timeframe (date/OHLC validity, NaN volume allowed, no 30-bar rule): a 24-bar name
appears in RSI-C D; a weekly failure leaves the daily cells. Rows keep their own last
date; counts and the default list cover the same-date cohort (tick *Include older-dated*
to add the rest). Nothing is filled, interpolated or substituted between D and W.

**Keys.** ↑/↓ j/k PgUp/PgDn Home/End as in SCAN; ←/→ choose the D or W side (W when a weekly
bucket is selected); Enter opens the archive chart at the exact **daily** bar that
triggered the read (a weekly read opens the last session of that completed week, never the
Monday timestamp); clicking a D or W cell opens that side. An unavailable side leaves a
status note and never opens the other side. Click a header to sort (nulls last, ties by
ticker); click a chip to filter by bucket. w/W, N hand-off and the watchlist origin
(`SCAN VIEW PATTERN`) work as in SCAN.

Performance budgets: `scan.engine_board_500_cold` ≤120 ms, `scan.engine_board_500_warm`
(content digest of 500 tails + cache hit) ≤25 ms, `paint.scan_view.{steady,hover}`
≤2 ms/frame.
