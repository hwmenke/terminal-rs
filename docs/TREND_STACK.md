# Adaptive Trend Stack (`TSTACK`)

Trend Stack adds multi-timeframe regime lanes and a confidence cloud to the price
pane. It works independently of `ADTREND`; enabling both keeps the classic lines
and flip dots above the cloud. It is a visualization of observed bars, with no
forecasts or trading actions.

| Command / input | Effect |
| --- | --- |
| `TSTACK`, `STACK`, `MTF`, `REGIME` | Toggle the focused chart layer |
| `TSTACK ON`, `TSTACK OFF` | Explicit enable / disable |
| `AAPL TSTACK` | Load AAPL and enable the layer |
| `NEW TSTACK NVDA`, `SPLIT DOWN TSTACK NVDA` | Open a 5m stacked chart (1D from an archive chart) |
| `,` / `.` | Shorter / longer interval |
| `\` | Last intraday interval (default 5m) ⇄ 1D |
| Click a lane label or cell | Switch to that interval, keeping the layer on |
| Chart menu → Review overlays → Trend Stack | Same toggle |

Interval keys require chart focus, an empty command line and no focused text
field. Archive charts step only through D/W/M; `\` reports “Archive charts are
daily and higher.” The existing typed intervals and header controls still work.
Only the `show_stack` flag is saved; calculations and lane prices stay in memory.
Older layouts load with the layer off.

## Reading the layer

The medium KAMA regime supplies a faint price-pane wash. The filled cloud is
MB ± σ, with dotted MB ± 2σ outer lines and axis tags. σ is the square root of the
Wilder average of `(close − MB)²` over twice the ATR length. Missing/warm-up
values break the cloud; the mesh never joins across a missing value. Cloud
geometry uses the same linear or log price scale as candles and does not change
autoscaling.

Confidence is zero when the warmed medium state is flat, otherwise:

`0.40 × ER(hlc3, mb_er) + 0.35 × clamp(|SB − MB| / (2 × ATR), 0, 1) + 0.25 × agreement`

Agreement is the fraction of short/long states matching the medium state.
Warm-up confidence is NaN. Intraday calculations use session ATR: the first
observed bar of a New York date uses high − low, excluding the overnight gap
from true range. Daily calculations and backtests retain their original ATR.
The pane's ADTREND parameters are used when present; otherwise defaults apply.

| Chart | Lanes |
| --- | --- |
| 1m | 1m · 5m · 15m · 1H |
| 5m | 5m · 15m · 1H · 1D |
| 15m | 15m · 1H · 1D · 1W |
| 30m | 30m · 1H · 1D · 1W |
| 1H | 1H · 4H · 1D · 1W |
| 4H | 4H · 1D · 1W · 1M |
| 1D | 1D · 1W · 1M |
| 1W | 1W · 1M |
| 1M | 1M |

Each lane is 7 px high with a 1 px gap; the strip takes `lanes × 8 + 4` px
before oscillator layout. A 30 px label gutter identifies each timeframe. The
strip is hidden below 220 px plot width or 160 px height; the HUD stays visible.
Long/short/flat cells use up/down/grid colours and confidence controls opacity.
Hover shows the native bar, regime age, confidence, MRT, provenance and last bar
in ET. Higher lanes on extended-hours charts remain regular-session.

`STACK n/m` counts ready, verified, current lanes agreeing with the completed
base state. Flat bases have no direction arrow. A single ready lane shows
`1/1` with “higher frames unavailable” and no score arrow. Provenance combines
only scored lanes. The intraday countdown tracks the existing refresh TTL; it
stops on stale or closed sessions. A newly completed regime flip can pulse once
for 1.2 seconds on a current real series; archive, cached-only and stale series
never pulse.

## Data guarantees and failure states

The base lane uses exactly the bars displayed by the pane. Other market lanes
request their native interval through `history_for_analysis`, with no display
cache, sample fallback, interpolation, synthetic print or base-bar resampling.
For a base bar opening at `t`, a lane bar `j` is available only when successor
`j+1` exists and its open is at or before `t`. Cells before the first completed
native bar stay empty. The last native bar appears separately as a half-height
outlined cap (`forming`, or `last` after session close); it never enters the
score. Timestamp duplicates are sorted and deduplicated keeping the last.

The archive path uses only `desk_daily` research bars and the existing W/M
resampler. It never calls the market provider. Archive chips say `RESEARCH
ARCHIVE` and use only daily and higher lanes.

- Sample/unverified base: red hatched refusal block and refusal HUD, no cloud,
  lanes or score. `TR_PROVIDER=sample` makes zero lane fetches.
- Sample/unverified higher lane: that lane is refused and excluded.
- Fewer than 60 native bars: hatched `WARMING have/60`, excluded.
- Missing/error lane: hatched `UNAVAILABLE`, with the error on hover, excluded.
- Stale base: desaturated layer and `STACK HELD · STALE …`, with no score,
  arrow, pulse or countdown. The last verified display remains after a history
  refresh fails; failure never substitutes another price source.
- Stale intraday lane: aged by its own asset clock, desaturated and excluded.
- Closed session: normal colours with `SESSION CLOSED`; no countdown or pulse.

The worker caches at most 64 native-lane entries with at most 400 bars each.
Intraday TTLs use `refresh_ttl_secs`; daily/higher TTL is 900 seconds. Only verified
market fetches write through to `BarCache` and `note_fetch`. No derived values
are persisted. Existing history refreshes and the 1.5-second live-tick throttle
also request the stack; it has no independent timer. Paint geometry is cached
by data/viewport/scale and uses a decimated cloud mesh plus one mesh per lane.

Journal, trade plans, alerts, orders, brokerage, scanners, stack backtesting,
RRG, GPU recolouring/shaders and editable lane ladders are outside this layer.

## Try it

1. Use a real provider, then enter `AAPL`, `5`, and `TSTACK`.
2. Press `.`, `.`, `,`, and `\`; hover and click the 1H/1D lanes.
3. Enable `ADTREND` to see its existing guides above the cloud and the intraday
   “session ATR” HUD note.
4. `NEW TSTACK NVDA`, then save/reload the layout.
5. With `TR_PROVIDER=sample`, repeat: only the red refusal is shown.
6. In `REVIEW`, enable `TSTACK` for archive D/W/M lanes.
