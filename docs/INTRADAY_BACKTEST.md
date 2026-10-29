# Intraday setup backtest

`IBT` (or `AAPL IBT`, `AAPL IBT VWAP`, `AAPL IBT OR 15m`) opens a research pane
that compares **session VWAP** and the **opening range** to **holding the cash
session** on the same unseen sessions.

This is separate from the daily archive comparison in [BACKTEST.md](BACKTEST.md).
It reads verified regular-session bars (1m, 5m, 15m, 30m, or 1h). It is paper /
research only and never places a brokerage order.

## Commands

| Command | Action |
|---|---|
| `IBT` | Pane for the focused name. Default 5-minute bars, all setups |
| `AAPL IBT` | AAPL, all setups |
| `AAPL IBT VWAP` | Session hold plus the VWAP cross |
| `AAPL IBT OR 15m` | Session hold plus the opening range on 15-minute bars |
| `NEW IBT` | Add the pane |
| `EXPORT BACKTEST` | JSON when this pane is focused (kind `intraday-backtest`) |

The benchmark is always listed. Typing one setup does not drop it.

## What is compared

| Setup | Rule |
|---|---|
| Session hold | Long from the first regular open. Flat at that session's last regular close. Declared before the open. |
| VWAP cross | After a close above session VWAP (`hlc3 × volume`), target long. After a close below, target flat (or short if shorts are on). |
| Opening range | The first 30 minutes set the high and low. That range is known only after the window's last bar closes. A later close above the high targets long. A later close below the low targets flat (or short). Inside the range, the prior target stays. |

## Execution

- The signal is the bar that just closed.
- The fill is the **next regular open of the same session**. There is no same-bar fill.
- An open trade is flattened at that session's **last regular close**. Nothing is carried overnight.
- Premarket and after-hours prints are ignored.
- A session with a missing bucket, two prints in one bucket, or no opening print is skipped. A later complete day does not stand in.
- A session whose first 30 minutes are incomplete stays flat for the opening-range setup. 1h bars do not divide 30 minutes, so that setup does not run on them.
- Commission and slippage, in bps per side, are charged on every fill including the session-close flatten.

## Holdout

Sessions are split **in time**, not at random and not by bar count. The last fraction (default 20%, at least 4 sessions) is the unseen evaluate window. Earlier sessions are train and are labelled in-sample. Setups reset every session, so train is not an indicator warm-up.

At least 10 complete regular sessions are required. Do not pick a winner on the evaluate sessions and present that choice as a new out-of-sample result.

## Metrics

| Figure | Definition |
|---|---|
| Total | Last marked equity / 1 − 1, costs included |
| CAGR | Same equity, treating each cash session as one day in a 252-session year |
| Max DD | Worst peak-to-trough on marked equity |
| Exp% | Share of regular bars with a position |
| Closed | Completed round-trips (entry and session flatten, or an earlier exit) |
| Win% | Winning **closed** round-trips / closed round-trips |
| vs Hold | Evaluate total return minus session-hold total return. Not a selected winner. |

The share of up bars while in a position is listed separately and is not a win rate.

Sample, missing, and unverified bars are refused. `TR_PROVIDER=sample` does not feed this pane.
