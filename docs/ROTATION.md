# Rotation (`RRG`)

`RRG` plots relative strength and its momentum for a list against a benchmark. This
is an open formula, not JdK RS-Ratio®. It is a dated research visualization.

```
RRG
RRG WL
RRG WL Growth WEEKLY
NVDA RRG AMD AVGO VS SMH TAIL 20
NEW RRG
SPLIT DOWN RRG
```

Aliases: `ROT`, `ROTATION`, `ROTATE`. With no list, the eleven sector ETFs are
compared with SPY. `WL` uses the selected watchlist group; `WL <group>` uses that
named group. Explicit tickers are deduplicated and the benchmark is removed.
Only equities, ETFs and indices qualify. Up to 40 names are read; exclusions and
names outside the archive appear in the skipped footer. `DAILY`/`1D` and
`WEEKLY`/`1W` select cadence. `W` and `D` are ticker symbols. `TAIL <n>` snaps to
5, 10, 15, 20 or 30 observations.

## Formula and dates

On the dates common to each name and the benchmark:

1. `rs = close / benchmark_close`
2. `rs_s = EMA(rs, S)` (seeded by the first S observations' SMA)
3. `ratio = 100 × rs_s / SMA(rs_s, L)`
4. `momentum = 100 × ratio / SMA(ratio, M)`

Daily S/L/M is 5/50/10. Weekly is 2/10/4, using the last archived close per ISO
week. A partial week has a `WTD` chip. Weekly observations with different last
trading dates are left unpaired. At least S+L+M aligned bars are required. Invalid
or zero denominators produce missing points. Dates are aligned to the benchmark's
last 260 observations; missing dates stay empty, with breaks in the tail.

The 100/100 axes divide Leading (up), Weakening (amber), Lagging (down) and
Improving (cyan). A tail's colour changes at each axis crossing. Table direction
is the last real adjacent step's compass octant, speed is its Euclidean length,
and IN-Q counts consecutive observations in the current quadrant. A gap ends
that count. Missing steps have no heading. Hollow heads and amber row dates mark
stale or gappy observations; they are never advanced using invented prices.

## Archive and provenance

The worker reads only `history.db(...daily_window...)`, with 420 daily bars or
2,000 bars for weekly aggregation. Computation runs off the UI thread. All series
pass `require_research_bars`, including the verified-bar gate. The report combines
the contributing bars' provenance, and the pane verifies provenance again when
accepting a response. Older request generations are ignored.

There are no provider, sample, display-cache or Whats-News reads. Even under
`TR_PROVIDER=sample`, an empty archive displays an empty quadrant stage and a
refusal. Missing, short and unverified names are listed with reasons and never
plotted. A missing/unverified benchmark or fewer than two usable names produces
an error over the empty stage.

`VERIFIED` includes the benchmark as-of date and source. Benchmark freshness is
measured against the last expected completed US session, including archive
calendar holidays. An older benchmark displays `STALE n sessions`, hollow heads
and `HIST REFRESH`. Reload runs on open, `R`, and completion of a HIST operation.
`A` explicitly runs `HIST ADD` only for names marked “not in archive”. Opening or
reloading RRG never downloads history.

## Controls

| Key | Action |
| --- | --- |
| Space | Play/pause at 6 frames per second; restart at the end |
| Left/Right | Step one frame (Shift: five) |
| Home/End | Oldest/latest frame |
| T / Shift+T | Next/previous tail length |
| 1 / 2 / 3 / 4 | Toggle Leading / Weakening / Lagging / Improving |
| 0 | Show all quadrants |
| L | Show/hide table |
| R | Reload the archive |
| A | Add missing archive names through HIST ADD |
| j/k or Down/Up | Select a visible name |
| Enter | Open the selected name in the active archive chart; keep RRG focus |
| N | Intraday TWALL of this list, selecting the same name |
| Shift+N | COCKPIT, CLOCK, or archive chart + TSTACK |
| Backspace | Return through a live hand-off stack |
| Esc | Clear selection |
| / | Focus command bar |

Hover a head, tail or dot to inspect its date, values and quadrant entry date.
Click selects; double-click a track or table row charts it. Click column headers
to sort the table. The wheel zooms about the pointer, dragging pans, and a
double-click on empty stage restores autoscale. Drag the timeline to scrub.
Historical cursors display an amber date and `REPLAY`; End returns to latest.
Reduced motion removes the entrance and steps playback by whole frames.

The pane docks, splits, pops out, and persists its spec, tail and table visibility
in layouts. It has no default global hotkey and no trading, alert or export hooks.

See [Next ▸ hand-off](NEXT_HANDOFF.md) for the crumb, command-bar isolation and temporary layout snapshots.
