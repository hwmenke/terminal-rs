# PM Desk and risk planning

`DESK` (aliases `PMDESK`, `RISK`; `AAPL DESK` preselects a name) opens a **watchlist
planning pane**. It is not the brokerage `PORT` blotter. `PORT` / `ACCT` / `POS` /
`ORD` / `ACTV` are unchanged.

The desk loads **only the selected watchlist group**. It never ranks the registered
archive universe and never substitutes sample bars.

## Leaders and group performance

21-bar close return uses the same definition as SCAN strength:
`(latest close / close 22 bars back − 1) × 100`. Rank is **watchlist-relative**:
only members of this group that share the same last bar date compete. Ties share
competition rank. Different dates never compete. The table labels ranks as
`watchlist rank r/n as of DATE (this group only; not archive-universe SCAN rank,
not SPY-relative)`.

Group figures (mean / median 21-bar return, up / down / flat counts) use the same
ranked set. Mixed last-bar dates are called out. Skipped names keep a reason
(short history, missing bars, invalid closes) instead of a padded return.

Box levels use the SCAN 20-bar / 3-bar confirmation rule on daily history. ATR(14)
is Wilder, from true range, and needs 15 bars.

## Position size

Pick a name, then set **entry**, **stop**, **target** (optional) and **dollar risk**.

Shares = dollar risk / |entry − stop|. Fractional shares are shown; whole shares
are the floor. R-multiple is |target − entry| / |entry − stop|. A stop above entry
is a short.

Stop source, in order:

1. **User** — typed stop, or a horizontal line taken from an open chart of that name
2. **Box** — box bottom for a long, box top for a short
3. **ATR fallback** — 2 × ATR(14) from entry (the multiple is editable)

Sample or unverified history is refused. The research cache and live display feed
are not used. `NEW DESK` / `SPLIT DESK` and layouts persist the group and selected
name. Horizontal drawings remain ordinary chart tools; the desk only reads their
price as a stop candidate.
