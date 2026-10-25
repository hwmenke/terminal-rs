# Historical lookalikes

`LOOKALIKE` (or `AAPL LOOKALIKE`, `KNN`) opens a research pane that finds **past
sessions whose market conditions look like a query date**, then compares what
happened next.

This is a native Rust port of the Whats-News nearest-neighbor idea
(`knn_model.py` conceptually). Python is not shipped in the runtime. The path
is **paper / research only**.

The pane uses **verified daily bars** from the local Yahoo research archive, then
the connected Whats-News bridge if the local series is empty. Sample, unverified
or display-cache prices are refused. There is no silent fallback.

## Commands

| Command | Action |
|---|---|
| `LOOKALIKE` / `KNN` | Pane for the focused name |
| `AAPL LOOKALIKE` | AAPL lookalikes as-of the last archived session |
| `AAPL KNN 2024-01-15` | AAPL lookalikes as-of that date |
| `AAPL LOOKALIKE K=15 GAP=21 H=5,10,21` | Neighbor count, overlap gap, horizons |
| `GOTO 2024-01-15` | Jump the focused archive chart to that session |
| `AAPL GOTO 2024-01-15` | Open AAPL's archive chart on that date |
| `NEW LOOKALIKE` / `SPLIT LOOKALIKE` | Add the pane |
| `EXPORT LOOKALIKE` | JSON with source, as-of, counts, overlap and neighbors |

Chips in the pane set **K=10 / K=20**, **Gap 21**, and the usual horizons.
Click a neighbor row (or select it and press Enter) to jump the archive chart
to that date.

`TR_PROVIDER=sample` never feeds this pane.

## How to run a search

1. Archive daily history (`HIST ADD AAPL` or a connected Whats-News name).
2. Type `AAPL LOOKALIKE` (or open **Lookalikes** in the sidebar).
3. Optionally set **as-of** (`YYYY-MM-DD`), **K**, **gap** and **horizons**.
4. Press **RUN**. Read the **per-horizon counts** first, then the frequencies,
   then the neighbor list.
5. Click a neighbor date to jump the archive chart.

## Distance features

Every feature is **backward-looking**. The query bar and each candidate use
only bars at or before that session:

| Feature | Lookback | Definition |
|---|---|---|
| `ret_1d` / `ret_5d` / `ret_10d` / `ret_21d` | 1 / 5 / 10 / 21 | `close[t] / close[t−L] − 1` |
| `rsi_14` | 14 | Wilder RSI |
| `vol_21` | 21 | Stdev of daily close-to-close returns |
| `dist_sma20` / `dist_sma50` | 20 / 50 | `close / SMA − 1` |
| `volume_ratio` | 20 | `volume / SMA(20) volume` |
| `range_21` | 21 | `(max high − min low) / close` over 21 sessions |

Distance is **standardized Euclidean**: each feature is z-scored across the
eligible past sessions **plus the query**, then
`√ Σ (z_query − z_candidate)²`. A feature with zero variance contributes 0.
The query itself and the `gap` sessions immediately before it are excluded so
a near-duplicate of “now” is not a neighbor. Future bars are never candidates.

A name needs **50 prior sessions** before the query so SMA(50) is defined.

## Horizons and incomplete outcomes

Default horizons are **5, 10 and 21** trading days. Forward return on neighbor
*t* is `close[t+H] / close[t] − 1`.

If fewer than H sessions exist after *t*, that horizon is **incomplete**. The
pane labels it (`incomp.`) and does **not** invent a finished return. Those
neighbors stay out of the mean, median, win rate and percentiles for that H.

## Per-horizon counts and overlap

The count table is shown **before** outcome frequencies:

| Figure | Meaning |
|---|---|
| req N | Selected neighbors (after the gap) |
| completed | Neighbors with a finished H-day close |
| incomplete | Neighbors whose H-day window has not finished |
| eff. N | Non-overlapping equivalent (see below) |
| mean / win% / p25/p75 | Computed on **completed** outcomes only |

**Overlap reduction.** Neighbors closer than `gap` sessions (default 21) to an
already chosen nearer neighbor are discarded. The header shows requested K,
selected N, discarded overlap and the eligible pool.

**Effective N.** After the gap, H-day outcome windows can still share days when
`gap < H`. Then `effective N = completed × gap / H`. When `gap ≥ H`, effective
N equals completed N. This is a label, not a discarded sample — every completed
neighbor still appears in the list.

## Chart jump

Click a neighbor (or `GOTO YYYY-MM-DD`) to open the **archive** chart and pan
so that session sits near the right edge. Zoom is unchanged. If the date is
older than the loaded window, the chart pages left and retries the jump.

`LATEST` / `END` still jump to the newest candle.

## Provenance

The header shows **REAL / CACHED**, the source string, the as-of date and the
session range. `EXPORT LOOKALIKE` writes the same fields plus requested K,
selected N, discarded overlap, per-horizon completed / incomplete counts and
the neighbor table. A sample-tagged file is named `SAMPLE` and is not restored
into the archive or this pane.

## Out of scope

Live order routing. SnapTrade / credentials. Cross-name lookalikes (search is
same-symbol past sessions). Python / sklearn (optional research tooling is not
shipped; the runtime path is native Rust).
