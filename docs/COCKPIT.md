# Intraday Cockpit (`COCKPIT`)

`COCKPIT` uses the focused ticker; `AAPL COCKPIT` chooses a name and `CKPT` is an alias.
The default ladder is `1m 5m 15m`. `COCKPIT 5m 15m 1h` picks another ladder: exactly three
strictly ascending intraday intervals. A descending or duplicate ladder, a daily token, or
the wrong number of tokens is refused with a status message, and the workspace is not
changed.

The command saves the current layout under its own name (as `IDESK` does), then replaces the
workspace with a layout named `Cockpit` in the `MainRight2` preset: the middle interval large
on the left, the slow interval top-right, the fast interval bottom-right. All three charts
are in the Blue link group with TSTACK and RAIL on. The main chart shows a
`COCKPIT 1m · 5m · 15m` chip.

Each chart loads through the ordinary history request at its **own native interval**. No
lane is resampled from another. A pane whose feed fails shows its existing NO REAL DATA or
STALE card; the other two keep working. Nothing is substituted.

Time Link, Lens and anchored VWAP work on any linked charts, not only in the Cockpit layout.
Panes with a different symbol, a different link group, or no link are ignored.

## Time Link

Hovering a chart publishes the hovered bar's span `[s, s + d)` (its open and interval, in
UTC seconds). Every other chart in the same link group showing the same security maps that
span onto its own bars (`tr_data::time_link::bars_in_span`):

- `lo` = first bar `j` with `t_j + e > s`
- `hi` = first bar `j` with `t_j ≥ s + d`

where `t_j` is the target bar's open and `e` its interval. The ghost cursor covers bars
`lo..hi`: a band in the link colour at about 12% alpha with a 1 px edge, a time pill with the
source chart's label, and a dashed line at the source cursor's price when it is inside this
chart's scale. While the local pointer is elsewhere, the legend shows bar `hi − 1`.

If `lo ≥ hi` there is no bar for that time (a missing bucket, before the first bar or after
the last). Nothing is drawn except a small tick on the time axis. Nothing is interpolated or
forward-filled.

For US equity, ETF and index intraday bars, each bar's end is capped at that date's cash
close (`intraday::close_minute`, the same rule `closed_len` uses), so the 12:45 15-minute bar
of a 13:00 half day maps to the 1-minute bars 12:45–12:59 only. All mapping is in UTC
seconds, so sessions either side of a daylight-saving change map identically.

The ghost costs two binary searches per chart per frame. No ghost is drawn while a pane is
being dragged.

## Lens

Each chart publishes its visible window: the first visible bar's open to the last visible
bar's end. A chart draws a bracket (top and bottom ticks, a 6% fill in the link colour,
labelled with the finer interval, for example `1m`) over the window of the **next-finer**
linked chart of the same name, mapped with the same function.

`Shift+click` on a bar, or the context-menu item **Center linked charts here**, scrolls the
other linked charts of the same name so that time is centred. A chart still loading keeps
the request and applies it when its bars arrive.

Click precedence: on a time-linked chart, `Shift+click` only centres the linked charts; an
active drawing tool or the armed AVWAP tool ignores that click. On an unlinked chart Shift
does nothing special. Otherwise an armed AVWAP tool takes the click (anchor only, no drawing
point; an armed drawing tool stays armed for the next click); only then do drawing tools get it.

## Anchored VWAP (`AVWAP`)

- `Alt+A` or `AVWAP` arms a one-click tool; click a bar to anchor at that bar's open.
  The context menu also has **Anchor VWAP here**.
- `AVWAP OPEN` anchors at today's cash open (09:30 New York on the latest loaded session,
  through `intraday::regular_opens`). `AVWAP HIGH` / `AVWAP LOW` anchor at the latest
  session's highest / lowest bar.
- `AVWAP CLEAR` removes every anchor for the name; click a flag and press Del to remove one.
- At most four anchors per symbol. A fifth is refused with a chip.
- Anchors propagate to every chart in the link group showing the same name, are stored per
  symbol (keyed like drawings), and are saved with the layout. Older layouts without the
  field load with no anchors.

### Formula

For an anchor time `A`, the start index `a` is the first bar with `t_a ≥ A`.

A bar is usable when its OHLC passes `cash_session::usable_range` and it reports a volume
`v ≥ 0`. With typical price `p_i = (H_i + L_i + C_i) / 3`, a reference `p_ref` (the first
usable typical price, for numerical stability) and `d_i = p_i − p_ref`, the running sums
over usable bars `a..=k` are

```
V_k = Σ v_i        S_k = Σ v_i d_i        Q_k = Σ v_i d_i²
AVWAP_k = p_ref + S_k / V_k
σ_k     = sqrt(max(0, Q_k / V_k − (S_k / V_k)²))
```

σ is the volume-weighted standard deviation of the typical price since the anchor. The
bands are `AVWAP ± σ` (stronger fill) and `AVWAP ± 2σ` (weaker fill). The readout is
`z_k = (C_k − AVWAP_k) / σ_k`, omitted when `σ_k = 0`. Anchored at the cash open on one
session, the line and ±1σ band equal the session VWAP bands (`indicators::vwap_bands`).

- An unusable bar outputs NaN (a break in the line) and adds nothing to the sums.
- While `V_k = 0` the output is NaN. The typical price is never substituted.
- Extended-hours bars are included when the pane shows extended hours, and the tag adds
  `EXT`. Otherwise the line carries across sessions on regular-session bars.

### The `≈` rule

On a chart coarser than the one the anchor was placed on, the anchor time can fall inside a
bar. The line then starts at the first bar that opens **at or after** the anchor, never at a
bar that opens before it, and the tag reads `≈` with the actual start time
(`AVWAP ≈10:45`). On the chart where the anchor was placed, and on finer charts, the line
starts exactly at the anchor.

### Forming-bar rule

`closed_len` decides which bars are complete, using the chart's last refresh (the feed's
observation clock), not wall time, so a stale partial bar stays forming. Archive (D/W/M)
charts use the current time. The segment to a forming last bar is dotted and has no band.
The right-edge tag (`AVWAP 10:45  412.31  z +1.4`) uses the last **closed** bar; hovering it
shows `FORMING` when a forming bar exists. Changing the forming bar never changes a closed
value, because the sums are causal.

### Refusals

The worker runs `Bar::require_verified(&bars, "Anchored VWAP")` before any arithmetic.
Sample or unverified bars, including under `TR_PROVIDER=sample`, return the shared refusal
text: the chart shows it and paints no line, band or number. Other refusals:

- `ANCHOR BEFORE LOADED HISTORY · page back` — the anchor is older than the first loaded
  bar. Nothing is drawn; the line never starts silently at the first loaded bar.
- Anchor after the last bar, or no bars: nothing is drawn and the anchor is kept.
- `AVWAP needs verified volume` — no bar from the anchor on reports volume (FX, some
  indices).

Archive charts accept AVWAP on their research-archive daily bars and never call the market
provider; AVWAP never fetches prices at all.

### Caching

The chart keys its result on

```
AvwapKey { generation, tf, anchors, n, first_ts, last_ts,
           last_close_bits, last_volume_bits, closed_len }
```

and sends a request only when the key changes: a tick on the forming bar, a closed bar,
paging, a reload or an anchor edit. A tick that only changes the forming bar's close or
volume waits for the 1.5 s indicator throttle. A response whose key does not match the
chart's current key is dropped. The computation runs on the worker in `spawn_blocking`.
Painted geometry is cached on (key, viewport, scale, rect); paint does no statistics and no
I/O.

### Math card

Hovering a flag shows the anchor time (New York), the start bar, bars used and skipped,
`ΣV`, the formula in one line, `CLOSED THRU`, and the provenance.
