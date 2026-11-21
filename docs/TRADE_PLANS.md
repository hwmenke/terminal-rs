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
then target, then stop management and the once-only +1R / breakeven milestones.
These are level checks, so gaps are caught at the observed price. Only one event can occur per plan per tick:
a quote crossing entry beyond target fires Entry; a later fresh quote can
resolve Target. Observed R is `(quote - entry) / (entry - initial stop)`, for both
longs and shorts. Open R uses the last accepted quote and its displayed UTC
time; target R in an entry message describes the planned target level.
Every displayed or messaged R is covered by **at observed quote, not a fill;
no order was changed**.
Closed plans never reopen automatically.

## Managing the stop

Arm rules with `NVDA PLAN 100 STOP 90 TARGET 130 RISK 300 BE 1 TRAIL 1R`.
`BE <r>` (or `BREAKEVEN <r>`, with an optional `R` suffix) moves the stop to
entry once observed R reaches the threshold. `TRAIL 2R` follows the best
observed quote by two times the initial entry-to-stop distance; `TRAIL 3%`
uses 3% of that high-water quote; `TRAIL 4.20` uses a fixed price distance.
For shorts the best quote is the lowest observed price and stops move down.
Rules also work on unsized plans. Breakeven thresholds and R trails are in
(0, 20]; percent trails are below 50%; a long's price distance must be below entry.

From DESK, **BE at +1R** and **Trail: Off / 1R / 2R / ATR×** sit beside
**Arm plan**. ATR freezes the pane's multiple × the verified row's ATR(14)
as a price distance and records its bar date and source. ATR is disabled
when ATR(14) is unavailable. These controls are pane-local and are not saved
in layouts.

Manage the named active plan with `NVDA TRAIL 1.5R`, `NVDA TRAIL OFF`,
`NVDA BREAKEVEN 1`, or `NVDA BREAKEVEN OFF`. Rules can be set while Armed
or Triggered. Attaching or replacing a trail on a triggered plan starts its
high-water measurement at the last accepted quote. Removing a rule leaves
the current stop in place. `BE` alone still loads the Bloom Energy ticker.

`NVDA MOVESTOP 105` tightens a triggered plan's stop by hand. It requires a
fresh verified observed quote, a strictly tighter stop, and a stop strictly
on the adverse side of that quote. Sample mode cannot move a stop. Cancel
and re-arm to change an Armed plan's stop. **Stop→BE** appears on a triggered
row when the last quote is beyond entry and the stop remains on the loss
side. Manual moves do not fire a breakeven event.

Stops only tighten. The entry tick initializes the high-water quote without
managing the stop. Later ticks check the previous stop, then target, before
ratcheting; a tick cannot stop itself out at a stop it just raised. Trail
ratchets update the row silently. Reaching breakeven fires once; if +1R fires
on the same tick, that event includes the breakeven move. Target and stop
closures take priority over management milestones.

The row shows the current stop label, initial stop, locked R and active
rules. All R calculations retain the initial stop as the risk basis, including
a gap through a moved stop. With verified ticks 99 → 100 → 110 → 118 → 107,
the example triggers at 100, reaches breakeven at 110, trails to 108 at 118,
and closes at +0.70R observed at 107: **not a fill; no order was changed**.
Locked R describes the stop level, not a guaranteed exit price.

The trail is measured from observed quotes; moves while the app was closed
are not backfilled. Stale, sample, unverified, missing, duplicate and older
quotes cannot ratchet a stop or fire an event. Existing version-1 files load
with no management rules and retain their original risk basis when a rule
is later added.

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
partial exits, chart level drawings, or automatic journal outcomes. **Journal** opens the existing draft; the user still records the
outcome. Plans are excluded from backup, restore and export payloads.
Corrupt or missing files load empty without writing; a successful later save
replaces the file.

See [PM Desk](PM_DESK.md), [price alerts](PRICE_ALERTS.md), and
[trade journal](TRADE_JOURNAL.md).
