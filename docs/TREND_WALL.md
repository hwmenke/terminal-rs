# Trend Wall (`TWALL`)

Trend Wall shows completed native Trend Stack regimes across a watchlist. Each row
has 5m, 15m, 1H and 1D lanes on a fixed 09:30–16:00 Eastern clock. Missing five-minute
prints leave empty columns in every lane. Higher-lane states become observable only
when their next native bar opens, exactly as in `TSTACK`. Outlined half-height caps
show forming observations separately. Hover for the clock, completed native bar
timestamp, confidence and provenance; the crosshair spans the wall.

| Command | Result |
| --- | --- |
| `TWALL` | Active watchlist, up to 24 unique symbols |
| `TWALL WL Growth` | Named watchlist |
| `TWALL AAPL MSFT NVDA` | Explicit symbols |
| `TWALL D` / `TWALL D WL Growth` | D/W/M research archive view, last 120 daily bars |
| `NEW TWALL` / `SPLIT DOWN TWALL` | New pane or split |

Aliases: `TGRID`, `STACKS`, `MTFGRID`. An explicit yellow key still names a security:
`TGRID US Equity` opens that symbol. Symbols beyond the cap are listed as “not loaded”.
Intraday mode uses US cash-session securities; unsupported sessions are flagged.

With the pane focused, an empty command line and no modifiers:

| Key | Action |
| --- | --- |
| ↑/↓ or J/K | Select row |
| Enter | Open selected symbol at 5m (daily in archive mode), with TSTACK enabled |
| S | Cycle stack alignment, flip age, cushion, original list order |
| D | Toggle intraday/archive |
| R | Reload |
| / | Focus command line |

Click a lane to open its interval. Linked charts follow the selected symbol, interval
and mode, while focus stays on the wall. Comma, period and backslash remain chart keys.
Settings (list, mode and sort) persist with layouts; older layouts remain compatible.

`STACK` counts lanes aligned with the completed base regime; a flat base has no arrow.
`FLIP` is native base bars since the completed medium-regime flip. `CUSH` is
(close − completed MRT) / 14-period session ATR and is red below 0.5. `VWAP` is
(close − session VWAP) / that ATR. `RVOL` uses `intraday::relative_volume` against
prior observed sessions. Missing metric inputs display a dash, never zero.
Intraday metrics stay blank in archive mode.

Breadth shows the share of Ready verified rows whose completed lane is long. Each
bar prints its numerator and denominator, and stays hidden below five votes.
Refused, warming, unavailable and stale rows have blank STACK tiles, no metric
scores, no breadth vote, and sort after scored rows in every sort mode.

The sample provider produces one refusal banner and zero history fetches. Live
lanes use verified native history and share TSTACK's 160-entry cache and interval
TTLs. The chart's verified base history also seeds the persistent native cache.
No intraday prices are resampled, interpolated or carried forward. Refresh uses the
existing UI periodic tick and 5m TTL during the regular session. Freshness is checked
again while waiting for responses. A failed refresh keeps a normal ribbon with a
`RETRY` chip only while its observations remain within their freshness windows.
Once stale, all ribbon cells and forming caps are dropped; a hatched
`STALE · last HH:MM ET` replaces them, with the failure available on hover.
Different native keys fetch independently. Four wall fetch permits bound wall
acquisition, while chart TSTACK requests bypass that semaphore.

Archive mode reads only the local research archive, including while markets are
closed or offline. Missing archive symbols stay unavailable; no bridge or market
provider is consulted. Weekly/monthly lanes aggregate verified archive daily bars.
The archive chip and hover dates identify these historical observations. Daily lanes
older than the last due cash session are flagged stale and do not score; weekends
and holidays do not demand nonexistent daily prints.

The ribbon cells and forming outlines are built into one cached mesh per pane.
Its key includes content revision, rectangle, sort and theme colours; moving the
hover crosshair does not rebuild the mesh. The 24-symbol cap bounds painting and
worker acquisition. Worker rows stream after each symbol's native fetches, with
computation on `spawn_blocking`; pane generations discard superseded responses.
