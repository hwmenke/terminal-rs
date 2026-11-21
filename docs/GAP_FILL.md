# Gap fill

`GFILL` (or `AAPL GFILL`, `AAPL GFILL 40 15m`) draws today's regular session against the prior cash close, and the share of earlier gaps that had traded back through that close by the same minute.

The gap is the 09:30 **open** versus the previous session's cash close. A positive number is a gap up. The line on the chart is each bar's **close** versus that same prior close, so the first point can already differ from the headline gap.

## Commands

| Command | Action |
|---|---|
| `GFILL` | Focused name. Default 40 prior sessions and 5-minute bars |
| `AAPL GFILL` | AAPL |
| `AAPL GFILL 40 15m` | Last 40 eligible sessions, 15-minute bars |
| `NEW GFILL` | Add the pane |

A bare number is how many prior sessions to keep (5 to 60, default 40). A bar size needs a unit (`5m`, `15m`, `1h`). `60` is sixty sessions. `1h` is the bar size. A bar size in the command wins over the Bars field.

Alias: `GAPFILL`.

## What a point means

- The prior close is the close of the previous session's **last regular bar**. Premarket and after-hours prints are ignored. On an early close, that bar is the 13:00 close, not a later print.
- A session with no single 09:30 print, or no single prior cash close, is skipped. A later bar does not stand in for the open, and an older close does not stand in for a missing one.
- Today is never inside the fill rate. If the latest day has no clean 09:30 print, the pane stops. It does not reuse an earlier day as today.
- A **fill** is a trade through the prior close: the bar's low on a gap up, or the bar's high on a gap down. Touching the close counts. The fill stays filled after that.
- A missing print or a second print **ends** the known path. Later minutes do not keep the last close, and the two prints are not combined into a range. A fill that already happened stays filled through the hole.
- A zero-volume print is a real print.
- The rate is the share of **same-direction** gaps in the window with a known outcome at that minute. It stays blank below five. It mixes gap sizes. The median absolute gap of those sessions is shown with the rate. A flat open is not a gap, and it has no rate.
- An early-close day that has not filled does not vote after its close. One that filled in the morning still counts as filled later.
- The typical line is those sessions' closes versus **their own** prior closes. It can run past today's last print. The fill rate after the last print is what earlier gaps had done by then. Neither line is a forecast.

Sample and unverified bars are refused. The pane does not fall back to sample prices.
