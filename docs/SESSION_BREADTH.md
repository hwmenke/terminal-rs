# Session breadth

`BRD` (or `BREADTH`, `SBREADTH`, `AAPL BRD MSFT NVDA`, `BRD 15m`) charts how a
list of cash names participates through one regular session.

Two lines, each from 0% to 100%:

- the share of names whose close is **at or above their own 09:30 open**
- the share whose close is **at or above their own session VWAP**

An unchanged print counts as at or above. The roster under the chart shows each
name's last print.

## Commands

| Command | Action |
|---|---|
| `BRD` | Selected watchlist. Default 5-minute bars |
| `BRD AAPL MSFT NVDA` | Those names, plus the command's own name when one is typed first |
| `AAPL BRD MSFT 15m` | AAPL, MSFT, 15-minute bars |
| `BRD 15m` | Watchlist on 15-minute bars |
| `NEW BRD` | Add the pane |

A bare number is a bar size. `5` is five-minute bars. `60` is one hour. A bar
size in the command wins over the Bars field. The list is at most 40 names.

## What a point means

- A name votes at a minute only when that bucket has **one** print.
- A hole does not vote and is not filled. Two prints are not averaged.
- The rate stays blank until **five** names vote at that minute. The counts still show.
- The session is the latest day on which at least five names have a single 09:30 print. A later day with fewer opens is not mixed in.
- A name with no single 09:30 print is out of every minute. A later bar does not stand in for the open.
- VWAP is that name's cumulative typical price for the session. A bar with no volume does not vote on VWAP until volume has printed.
- Premarket and after-hours prints are ignored. On an early close, a later minute gets no vote.
- The right edge is the last print in the read, not a forecast of the cash close.

Sample and unverified bars are refused. One sample name is left out of the rate. A board with no verified 09:30 print is refused.
