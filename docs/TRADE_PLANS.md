# Trade plans

`PLANS` opens **ALERTS → TRADE PLANS**, above the independent alert rules.
In `DESK`, select a name and set entry, stop, optional target and dollar risk,
then click **Arm plan** beside **Journal**. The button requires a valid risk
plan and a verified row without a skip reason. It copies the whole shares,
risk budget and stop source. Plans watch quotes only and place no orders.

`NVDA PLAN 142.5 STOP 138.2 TARGET 151.1 RISK 300` arms the same bracket from
the command line. `TGT` aliases `TARGET`; keywords are case-insensitive.
Target and risk are optional. With risk, shares are rounded down from
`risk / abs(entry - stop)`; without risk, shares and risk are zero and the row
says **size not set**. Stops below entry create long plans; stops above entry
create shorts. Targets must be on the profit side of entry. `UNPLAN NVDA`
cancels the name's active plan. Cancel before replacing an active plan.

Plans and their events persist in `trade_plans.json` under the terminal data
directory, separately from `price_alerts.json`. Up to 32 plans may be active;
200 plans and 200 events are retained, trimming the oldest closed plans first.
The pane lists active plans followed by the ten most recently closed ones.
Active rows have **Cancel**; closed rows have **Journal** and **Remove**.
Click a ticker or event to open its chart. Events create a toast and an unread
count; **MARK READ** marks plan events along with the other alerts.

## Transitions

The first fresh verified quote seeds the entry direction and never fires an
entry event. A seed below entry watches a cross up; a seed above watches a
cross down. A seed exactly at entry watches up for a long and down for a
short, using a strict previous side before an inclusive touch. The row says
**awaiting cross** after seeding. A stop traded before entry invalidates the
plan, including a gap through entry and stop in one tick.

After the entry crosses, the plan is **TRIGGERED**. Fresh quotes check stop,
then target, then the once-only +1R milestone. These are level checks, so gaps
are caught at the observed price. Only one event can occur per plan per tick:
a quote crossing entry beyond target fires Entry; a later fresh quote can
resolve Target. Observed R is `(quote - entry) / (entry - stop)`, for both
longs and shorts. Open R uses the last accepted quote and its displayed UTC
time; target R in an entry message describes the planned target level.
Every displayed or messaged R is covered by **at observed quote, not a fill**.
Closed plans never reopen automatically.

## Freshness and failure

Plans evaluate on the verified-quote loop (20 seconds by default), including
when no price or setup rule is enabled. Arming evaluates immediately to seed.
No quote request is made when there are no active plans.

| State | Meaning |
|---|---|
| Sample | Sample / offline provider: plans are not evaluated and never fire |
| Stale | Quote older than 15 minutes, or quote fetch failed: plans hold their last verified state |
| Partial | Failed names and reasons are listed; other fresh verified names may evaluate |

Unverified, missing, non-finite and non-positive quotes cannot seed, change
plan state or fire events. Stale quotes cannot move the seed or last price.
Duplicate or older timestamps are ignored. Crossings missed while the app
was closed are not backfilled from bars.

## Scope

Equities, FX and crypto use the quote feed. There are no bracket/OCO orders,
trailing stops, partial exits, chart level drawings, or automatic journal
outcomes. **Journal** opens the existing draft; the user still records the
outcome. Plans are excluded from backup, restore and export payloads.
Corrupt or missing files load empty without writing; a successful later save
replaces the file.

See [PM Desk](PM_DESK.md), [price alerts](PRICE_ALERTS.md), and
[trade journal](TRADE_JOURNAL.md).
