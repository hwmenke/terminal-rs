# Session extreme

`XTRM` (or `AAPL XTRM`, `AAPL XTRM 20 15m`) shows when the high and the low of the cash session print.

The minute is the **first** regular print of that extreme. A later bar that only ties the high or the low does not move it. The chart is the share of earlier complete sessions whose high had already printed by that minute, and the same share for the low.

Today's marker is the running high and low of the bars seen so far. It can still move until the session is complete.

## Commands

| Command | Action |
|---|---|
| `XTRM` | Focused name. Default 20 prior sessions and 5-minute bars |
| `AAPL XTRM` | AAPL |
| `AAPL XTRM 20 15m` | Last 20 complete sessions, 15-minute bars |
| `NEW XTRM` | Add the pane |

A bare number is how many prior sessions to keep (5 to 60, default 20). A bar size needs a unit (`5m`, `15m`, `1h`). `60` is sixty sessions. `1h` is the bar size. A bar size in the command wins over the Bars field.

Alias: `EXTREME`.

## What a point means

- Only a **complete** session votes. Every regular bucket from 09:30 through that day's cash close has to be one finite print. A missing print or a second print drops the whole session. The two prints are not combined, and a later bar does not stand in. The missing bar might hold the high or the low.
- A zero-volume print is a real print.
- Premarket and after-hours prints are ignored.
- An early close is complete at 13:00. It votes in the morning and does not vote after its close.
- Today is never inside the rate. If the latest day has no single 09:30 print, the pane stops. It does not reuse an earlier day.
- The rate stays blank below five complete sessions whose own grid includes that minute.
- The median minute is the first-print minute of those complete sessions. It stays blank below five.
- The rate after today's last print is what completed sessions had done by then. It is not a forecast, and it is not a claim that today's high is in.

Sample and unverified bars are refused. The pane does not fall back to sample prices.
