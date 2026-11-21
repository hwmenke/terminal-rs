# Typical session path

`SPATH` (or `AAPL SPATH`, `AAPL SPATH 20 15m`) draws today's regular session
against the typical path of earlier sessions. Each point is that session's
close divided by **that session's own 09:30 open**, in percent.

`CMP` stays a price overlay rebased to the first visible bar. `ICMP` compares
several names on one session. `SPATH` asks whether today's path is unusual for
this minute.

## Commands

| Command | Action |
|---|---|
| `SPATH` | Focused name. Default 20 prior sessions and 5-minute bars |
| `AAPL SPATH` | AAPL |
| `AAPL SPATH 20 15m` | Last 20 eligible sessions, 15-minute bars |
| `NEW SPATH` | Add the pane |

A bare number is how many prior sessions to keep (5 to 60, default 20). A bar
size needs a unit (`5m`, `15m`, `1h`). `60` is sixty sessions. `1h` is the bar
size. A bar size in the command wins over the Bars field.

Aliases: `SESSIONPATH`, `TPATH`.

## What a point means

- The anchor is the **open of the 09:30 bucket**. A later bar never stands in for a missing open.
- The 09:30 point is that bar's close versus its open, so it is 0 only when the bar is unchanged.
- Today is never included in the mean.
- The mean, 25th, and 75th stay blank until five prior sessions printed at that minute.
- A hole does not vote and is not filled. Two prints in one bucket are left blank for that session.
- A prior session with no single 09:30 print, or a non-positive open, is skipped entirely.
- Premarket and after-hours prints are ignored. On an early close, a later minute gets no vote from that day.
- **vs typical** is today's last print minus the typical print at that same minute, in percentage points.
- The typical line may continue past today's last print because earlier sessions had later bars. That extension is not a forecast.
- 1m, 5m, 15m, 30m, and 1h are the bar sizes. Daily bars are refused.

Sample and unverified bars are refused. The pane does not fall back to sample prices.
