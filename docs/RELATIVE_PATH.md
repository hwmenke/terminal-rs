# Relative path versus SPY

`RPATH` (or `AAPL RPATH`, `AAPL RPATH 20 15m`) draws this name's percent from its own 09:30 open minus SPY's percent from SPY's own 09:30 open.

The amber line is the mean of prior sessions at that same minute. The faint lines are the 25th and 75th of those spreads. Today's spread is the accent line.

| Command | What it uses |
|---|---|
| `RPATH` | Focused name. Default 20 prior sessions and 5-minute bars |
| `AAPL RPATH` | AAPL versus SPY |
| `AAPL RPATH 20 15m` | Last 20 sessions with a single open on both, 15-minute bars |
| `NEW RPATH` | Add the pane |

Alias: `RELPATH`.

## What a point means

The spread is percentage points. A name up 1.2% from its open while SPY is up 0.4% from its own open prints as +0.80. It is not a second price, and it is not SPY's percent drawn on the same chart (`ICMP` does that).

The 09:30 open is that bar's open. A later bar never stands in for a missing open. Two prints in the 09:30 bucket make the open ambiguous, and that session is dropped.

A minute votes only when this name and SPY each have exactly one usable print in that bucket. A missing SPY bar stays blank. The previous SPY print is not carried forward. Two prints in one bucket are not averaged.

A zero-volume print still counts. Premarket and after-hours prints are ignored. On an early close, a bar after 13:00 does not count, and that day does not vote after its own close.

Today is not inside the mean. The window keeps the most recent sessions that had a single 09:30 open on both names (default 20, allowed 5 through 60). The mean and the band stay blank below five voters at that minute.

The curve past the last minute both printed is the history of prior sessions. It is not a forecast.

SPY itself is refused: it is the benchmark. Equity, ETF, and index only. Sample and missing bars are refused, on this name and on SPY. A bare integer is a session count. A bar size must include a letter (`5m`, `15m`, `1h`).
