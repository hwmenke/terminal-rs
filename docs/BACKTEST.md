# Credible strategy backtests

`BACKTEST` (or `AAPL BACKTEST`, `BT`) opens a research pane that compares
**declared** strategies to **buy-and-hold** on the same unseen evaluate window.

This is a native Rust port of the Whats-News backtester idea (`backtester.py`
conceptually). Python is not shipped in the runtime. The path is **paper /
research only** — it never places a brokerage order.

The pane uses **verified daily bars** from the local Yahoo research archive, then
the connected Whats-News bridge if the local series is empty. Sample, unverified
or display-cache prices are refused. There is no silent fallback.

## Commands

| Command | Action |
|---|---|
| `BACKTEST` / `BT` | Pane for the focused name (default strategy set) |
| `AAPL BACKTEST` | AAPL vs buy-and-hold |
| `AAPL BT ADTREND` | Buy-and-hold plus Adaptive Trend |
| `AAPL BACKTEST SMA 20 50` | Buy-and-hold plus that SMA cross |
| `NEW BACKTEST` / `SPLIT BACKTEST` | Add the pane |
| `EXPORT BACKTEST` | JSON with source, as-of, costs, holdout, execution and metrics |

Strategy chips: **ALL** (buy-and-hold, Adaptive Trend, SMA 20/50, RSI 14 30/70),
**Hold**, **ADTREND**, **SMA 20/50**, **RSI**. Buy-and-hold is always the
benchmark, even when you type a single name.

## How to run a comparison

1. Archive daily history (`HIST ADD AAPL` or a connected Whats-News name).
2. Type `AAPL BACKTEST` (or open **Backtest** in the sidebar).
3. Set **holdout** (default last 20%), **commission** and **slippage** in bps
   per side, and whether shorts are allowed.
4. Press **RUN**. The evaluate table is the comparison. Train figures sit below
   it and are labelled in-sample.

`TR_PROVIDER=sample` never feeds this pane.

## Holdout

The series is split **in time**, not at random:

- **Train** = earlier bars. Used for indicator warm-up. Metrics are **in-sample**
  and are not used to pick a champion.
- **Evaluate** = the last fraction (default 20%, at least 20 sessions). This is
  the unseen window. Every strategy is scored here with the same declared
  parameters.

Do **not** select a winner on the evaluate history and present that choice as a
new out-of-sample result. Changing parameters after seeing evaluate numbers
invalidates the holdout.

## Costs and execution

| Setting | Default | Meaning |
|---|---|---|
| Commission | 5 bps | Per side, on the fill price |
| Slippage | 5 bps | Per side, on the fill price |
| Fill | next open | Signal after bar *i* close; order fills at bar *i+1* open |
| Position | all-in | Long, flat, or short (shorts off by default) |

There is no same-bar fill and no close-to-close lookahead. Buy-and-hold enters
at the first evaluate open and is marked to the last evaluate close.

## Metrics

| Figure | Definition |
|---|---|
| CAGR | `(end / start) ^ (252 / N) − 1` on marked evaluate equity |
| Total | Last marked equity / first marked equity − 1 |
| Max DD | Worst peak-to-trough on the evaluate equity curve |
| Exp% | Share of evaluate sessions with a non-flat position |
| Closed | Number of **completed** trades (full exit) |
| Win% | Winning **closed** trades / closed trades |
| vs B&H | Evaluate CAGR minus buy-and-hold CAGR (same window). Not a selected winner. |
| Open | Position still on at the last bar; marked for CAGR, **not** a completed win |

**Completed-trade win rate is not the share of positive holding days.** That
holding-day rate is listed separately and labelled “not a win rate.”

An open trade at the end of evaluate is marked to the last close for CAGR and
excluded from the completed-trade win rate.

## Reproducible settings

The pane header (and `EXPORT BACKTEST`) always shows:

- seed (`1`, deterministic — no Monte Carlo)
- fill rule
- commission + slippage
- holdout fraction and the train / evaluate date ranges
- strategy parameters
- source, as-of date, provenance (`REAL` / `CACHED`)

## Provenance

The header shows **REAL / CACHED**, the source string, the as-of date and the
session range. A sample-tagged export is named `SAMPLE` and is not restored into
the archive or this pane.

## Out of scope

Step 15 historical lookalikes. Live order routing. SnapTrade / credentials.
Parameter search on the evaluate window.
