# Price and setup alerts

`ALERTS` (aliases `ALRT`, `ALERT`) opens the existing alerts pane. It now lists
**price levels** and **SCAN setup changes** above congressional filings. A fired
row is timestamped; click the name or message to open that chart in one step.

`AAPL ALERT 150` watches a cross above 150. `AAPL ALERT BELOW 140` watches a
cross below. `AAPL ALERT VWAP` watches a cross above that name's session VWAP;
`AAPL ALERT VWAP BELOW` watches a cross below. `AAPL ALERT RVOL 2` watches
session relative volume crossing up through 2× (the number is the multiple you
set; there is no default). `NVDA ALERT SETUP NEAR_HIGH`
watches that name entering the setup (`EXIT` / `EITHER` change the transition).
`UNALERT AAPL` drops that name's rules. The chart context menu can set the
same price rules from last close, and a session-VWAP or 2× session-RVOL rule
on an equity, ETF, or index chart. A VWAP alert is not drawn as a fixed price
line. An RVOL alert is not drawn as one either.

Rules persist in `price_alerts.json` under the terminal data directory. The same
(symbol, kind) is stored once. A crossing or setup transition fires once per
tick; the previous price/match is seeded on first sight so opening a pane does
not invent a hit.

## Freshness and failure

Evaluation reuses the Step 8 news pattern:

| State | Meaning |
|---|---|
| Stale | Quote older than 15 minutes, last 5-minute bar not current for a VWAP or RVOL rule, daily setup bar older than the last expected session, or a refresh failed and leftovers remain |
| Partial | Some names failed; successful rules still evaluate |
| Sample | `TR_PROVIDER=sample` or unverified bars — **no events are invented** |

Stale quotes and stale setup bars do not fire. A stale or session-closed last
5-minute bar does not fire a VWAP or RVOL rule and does not move the seeded
side. Sample / unverified history is refused the same way SCAN and PM Desk
refuse it.

## Scope

Price rules use the live quote feed. VWAP rules read verified 5-minute history
for that name only (120 bars, one regular session plus a cushion). The level
is the session VWAP of those bars: `hlc3 * volume` on the latest regular
session. The first sight only records which side of VWAP the close is on.
There is no VWAP when the session has no positive volume, and a pre-market
print does not stand in for the regular-session close. RVOL rules read the
same verified 5-minute history with the scan's longer window (400 bars) so a
prior regular session can sit at the same offset. The multiple is the one in
the rule. The first sight only records whether relative volume is already at
that multiple. There is no RVOL until a prior session has volume at that
offset, and a missing prior is not stored as zero. FX, crypto, and other names
without a US cash session are refused. Setup rules use the
**research archive** daily series (Yahoo / local history), not the display
cache and not sample bars. The archive universe is not polled — only names
with an enabled rule are read.

Setup names are the SCAN list: `DARVAS_BREAKOUT`, `DARVAS_BOX`, `DARVAS_FAIL`,
`NEAR_HIGH`, `VOL_SURGE`, `EP`, `BREAKOUT_QUEUE`, `RSI_OB`, `RSI_OS`.

Step 10 journal lives in [`TRADE_JOURNAL.md`](TRADE_JOURNAL.md). Step 11 export is out of scope.
