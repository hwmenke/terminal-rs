# Fast daily / weekly chart review

`REVIEW` (or `AAPL REVIEW`) opens a **linked daily and weekly archive pair**. Scan
results and watchlist stepping keep both charts on the same name. `LATEST` or
`End` jumps to the latest candle without changing zoom. `BOX`, `RS` and `VCTX`
toggle the review overlays; they are saved with the layout.

Archive charts stay on the research path: no market-display cache, live feed or
sample fallback. Weekly and monthly candles are still resampled from **daily**
rows. Whats-News stored weekly rows are never read.

## Bounded history

M1 loaded every archived daily bar into the chart. Review loads a page instead:

| Interval | Displayed candles | Daily rows fetched |
|---|---|---|
| Daily | 800 | 800 |
| Weekly | 400 | 2,010 |
| Monthly | 240 | 5,530 |

`TR_ARCHIVE_LIMIT` overrides the candle count. Scrolling left pages older daily
rows and re-resamples the combined window so a week that straddles the page
boundary is not split. Scans, `UNIV` and freshness still use the full stored
daily series.

## Overlays (honest labels)

| Overlay | What it is | What it is not |
|---|---|---|
| Box | Latest 20-bar high / 3-bar confirmation **on this interval** (same rule as SCAN) | A daily SCAN box drawn unchanged on a weekly chart |
| RS | 21-bar close return of **this series** | SPY-relative RS, a published rating, or SCAN universe rank |
| Volume | This bar / mean of the prior 20, plus 5/20 | The original Whats-News service volume field |

Insufficient history leaves the number blank and says why. Sample or unverified
bars are refused.

## VWAP

The VWAP study follows [`session_key`](../crates/core/src/indicators/levels.rs):
UTC day on intraday bars, calendar month on daily bars, calendar year on weekly
and monthly bars. The legend therefore says **Session VWAP**, **Monthly VWAP**
or **Yearly VWAP**. Daily-bar VWAP is not a session-anchored daily VWAP.
