# Volume pace

`VPACE` (or `AAPL VPACE`, `AAPL VPACE 20 15m`) draws today's cumulative
regular-session volume against the mean and the 25th–75th band of prior
sessions.

The scan's RVOL number is one ratio at the last bar. `VPACE` is the path of
that total through the session.

## Commands

| Command | Action |
|---|---|
| `VPACE` | Focused name. Default 20 prior sessions and 5-minute bars |
| `AAPL VPACE` | AAPL |
| `AAPL VPACE 20 15m` | Last 20 eligible sessions, 15-minute bars |
| `NEW VPACE` | Add the pane |

A bare number is how many prior sessions to keep (5 to 60, default 20). A bar
size needs a unit (`5m`, `15m`, `1h`). `60` is sixty sessions. `1h` is the bar
size. A bar size in the command wins over the Bars field.

Aliases: `VOLPACE`, `CUMVOL`.

## What a point means

- The total is the sum of **one print per minute from 09:30** through that minute.
- Today is never included in the mean.
- The mean, 25th, and 75th stay blank until five prior sessions still have a complete total at that minute.
- A missing print or a second print **ends** that session's cumulative. Later minutes do not keep the last known total, and the two prints are not added together.
- A session with no single 09:30 print is skipped. A later bar does not stand in for the open.
- A zero-volume print is a real print and adds nothing. It does not break the total.
- Premarket and after-hours volume is ignored. On an early close, a later minute gets no vote from that day.
- **Pace** is today's cumulative total divided by the typical total at the same minute. It stays blank when the typical total is zero or fewer than five priors remain.
- The typical line may continue past today's last print. That extension is not a forecast.

Sample and unverified bars are refused. The pane does not fall back to sample prices.
