# Price and setup alerts

`ALERTS` (aliases `ALRT`, `ALERT`) opens the existing alerts pane. It now lists
**price levels** and **SCAN setup changes** above congressional filings. A fired
row is timestamped; click the name or message to open that chart in one step.

`AAPL ALERT 150` watches a cross above 150. `AAPL ALERT BELOW 140` watches a
cross below. `NVDA ALERT SETUP NEAR_HIGH` watches that name entering the setup
(`EXIT` / `EITHER` change the transition). `UNALERT AAPL` drops that name's
rules. The chart context menu can set the same rules from last close.

Rules persist in `price_alerts.json` under the terminal data directory. The same
(symbol, kind) is stored once. A crossing or setup transition fires once per
tick; the previous price/match is seeded on first sight so opening a pane does
not invent a hit.

## Freshness and failure

Evaluation reuses the Step 8 news pattern:

| State | Meaning |
|---|---|
| Stale | Quote older than 15 minutes, daily setup bar older than the last expected session, or a refresh failed and leftovers remain |
| Partial | Some names failed; successful rules still evaluate |
| Sample | `TR_PROVIDER=sample` or unverified bars — **no events are invented** |

Stale quotes and stale setup bars do not fire. Sample / unverified history is
refused the same way SCAN and PM Desk refuse it.

## Scope

Price rules use the live quote feed. Setup rules use the **research archive**
daily series (Yahoo / local history), not the display cache and not sample
bars. The archive universe is not polled — only names with an enabled rule
are read.

Setup names are the SCAN list: `DARVAS_BREAKOUT`, `DARVAS_BOX`, `DARVAS_FAIL`,
`NEAR_HIGH`, `VOL_SURGE`, `EP`, `BREAKOUT_QUEUE`, `RSI_OB`, `RSI_OS`.

Step 10 journal and Step 11 export are out of scope.
