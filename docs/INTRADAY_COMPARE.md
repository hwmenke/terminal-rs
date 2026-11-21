# Intraday session compare

`ICMP` (or `AAPL ICMP`, `AAPL ICMP MSFT NVDA`, `AAPL ICMP 1m`) opens a chart of
several cash names on one clock. Each line is that bar's close divided by
**that name's own 09:30 open**, in percent. The zero line is the open.

`CMP MSFT` stays the single overlay on a price chart, rebased to the first
visible bar. `ICMP` is the session-open view.

## Commands

| Command | Action |
|---|---|
| `ICMP` | Focused name and SPY. Default 5-minute bars |
| `AAPL ICMP` | AAPL and SPY |
| `AAPL ICMP MSFT NVDA` | AAPL, MSFT, NVDA, and SPY |
| `AAPL ICMP 1m` | 1-minute bars. A bar size in the command wins over the Bars field |
| `NEW ICMP` | Add the pane |

SPY is included once. At most 8 names, including SPY.

## What a point means

- The anchor price is the **open of the 09:30 bucket**, not the first close and not a later bar.
- The 09:30 point is that bar's close versus its open, so it is 0 only when the bar is unchanged.
- A missing bucket is a gap. Nothing is filled in, and two prints in one bucket are left blank.
- Premarket and after-hours prints are ignored.
- The session is the anchor's latest regular session. A peer with no 09:30 print that day is listed and not drawn.
- **vs SPY** is percentage points at the anchor's last print. It is blank when SPY has no print on that same bucket.
- 1m, 5m, 15m, 30m, and 1h are the bar sizes. Daily bars are refused.

Sample, missing, and unverified bars are refused. `TR_PROVIDER=sample` does not feed this pane.
