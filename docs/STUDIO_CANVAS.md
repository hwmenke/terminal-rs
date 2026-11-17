# Quick chart and time canvas

`AAPL QUICK QQQ 5m mode=index100` opens the Studio native time canvas at full
height. Studio’s rail switches between **Quick canvas** and the original **Native
chart**. STUDIO defaults to Native chart, preserving indicators, candle/log
controls and existing study layers; QUICK defaults to scalar lines. Up to four
comparison tickers may follow QUICK. `QUICK mode=level`,
`index100`, `change`, `z`, or `vol` changes the focused Studio canvas without
recreating it or moving its pinned base. Bare QUICK opens the focused security
at daily frequency. Explicit security qualifiers still resolve the QUICK ticker.
`DSTRIP` toggles the 44-point right distribution strip on either a native chart
or Studio. The strip has its own space and never consumes the price-axis labels.

The toolbar exposes all five modes, **Base UTC**, **Distribution**, **Events**,
**Brush** and **Shared y**. The initial base is the first supplied timestamp,
then stays pinned through pan, zoom and paging. Enter an exact UTC second and
click Pin base to change it. Missing/forming base observations are unavailable;
there is no nearest-date snapping. Changing security or interval resets the base.
The explicit pinned flag distinguishes an uninitialised view from a deliberately
entered timestamp of zero. Studio/Quick view inputs persist with serde defaults;
regular-chart DSTRIP is session-only. Copy command includes tickers, interval and
mode; the exact base is preserved in the layout, not in the copied command.

## Arithmetic and receipts

For positive admitted closes P and an exact base b:

| Mode | Definition | Units |
| --- | --- | --- |
| level | P[t] | native units |
| index100 | 100 P[t]/P[b] | base=100 index |
| change | P[t]−P[b] | native units, never substituted percent change |
| z | (P[t]−mean(P[t−w..t])) / sample_sd(P[t−w..t]) | prior sample σ |
| vol | 10,000 sample_sd(log(P[j]/P[j−1]), j=t−w..t−1) | bp/bar |

The default w is 63. Both rolling references exclude the scored point. Vol needs
w consecutive prior one-bar returns and their w+1 prices. Sample variance uses
n−1; stable Welford remove/add updates refresh from a two-pass calculation every
256 points and reset at each gap. Zero reference variance makes Z unavailable;
flat-return volatility is correctly zero. No annualisation, inference, hypothesis
selection, forecast probability, bootstrap or random sampling is introduced.
LOW N (<30 reference observations) keeps descriptive values with a caution tick.

The strip counts visible **closed transformed values**, using 32 equal-width
bins. Its now percentile is the empirical **midrank**
`(# values < now + 0.5 # values = now)/n`; now is the latest closed transformed
point, even when it is outside the visible sample. A missing latest transformed
point has no now pin/percentile. Shape's existing right-continuous percentile
convention and all existing DIST/CORR/SEASON estimators remain unchanged.
Histogram columns are neutral slate; categorical line identities carry no sign
or inference meaning. Signed values use explicit +/− and sign ink. Z and vol
print unit-specific keys. Last flags pack deterministically with at least 16
points of separation and leader lines; independent axes use each line's scale.

Every new statistic has a MathCard with a stable `canvas.*` ID, formula, actual
inputs, estimator/reference n, span, source, observation clock, CLOSED THRU and
unavailable reason. IDs are `canvas.level`, `canvas.index100`, `canvas.change`,
`canvas.z`, `canvas.vol`, `canvas.base.change`, `canvas.strip.midrank` and
`canvas.brush.distribution`. CLOSED THRU is the actual final admitted interval
end (cash daily uses its exchange close, including early closes).

Native bars arrive as immutable Arcs with their original source/stitch receipts.
The worker validates the **whole supplied input before trim, digest or cache
lookup**. Mixed verified tags require the existing vault witness. Sample,
unverified, invalid OHLCV, nonpositive prices, unordered/duplicate timestamps,
empty sources and oversized input are refused. Signed futures price admission is
not implemented. Each source uses `closed_len` and its own observation clock;
unknown clocks withhold the final print. If a stream advances beyond the last
fetch receipt, that receipt is preserved but the newer observation clock is
explicitly unknown; wall time is never substituted to close the last print.

Comparisons use existing daily research archive/verified bridge or intraday
`analysis_bars` paths. Failed peers stay named and unavailable; none is replaced.
They retain independent clocks and receipts. Closed source snapshots may be
reused for up to 60 seconds within one native generation; hover/pan does not
fetch a peer. Reload/security/interval changes invalidate that reuse. Updates
are coalesced with one pending request and a 200 ms minimum dispatch interval.
Sample mode clears reports and overlays and makes zero research provider calls.

Cash rolling references use the supported US session calendar (including missing
sessions and early closes). Crypto uses an explicitly labelled UTC grid. Other
session calendars/aggregated 4h cash rolling references explain their limitation;
level/index/change do not apply a US session to futures or crypto. No missing
interval is filled. Source transforms run before exact timestamp projection onto
the native chart. Missing matches remain absent; every LOD draw range is split
at gaps, so zooming out cannot connect across a missing interval. No scalar value
is converted into a positive-price Bar.

Shared y is enabled only for matching units. Because the Security model carries
no general currency metadata, units are conservative: US shares/ETFs use USD;
other native series keep their own instrument identity. Independent y overlays
label the primary axis/strip and use each series' own scale for its last flag.

## Dated events and brush

Two independent 20-point lanes show N news and E earnings. News retains actual
publication instants, publisher and headline from the existing scoped NewsBatch.
Earnings retains the verified QuoteStats New York date window as a bracket,
without manufacturing an intraday announcement time. Quote has no per-record
vendor field: the lane names the configured provider and explains that limit.
Collisions form deterministic count chips. The scrollable hover lists all actual
records using bounded visible rows. No matching records prints **No dated events
available**. Provider coverage may supply only a few recent headlines and a
single forward earnings window; no historical calendar is fabricated.

Brush is an explicit toolbar mode, exclusive of drawings, Time Link centring and
AVWAP. Escape cancels it before closing Inspector. Drag selects whole loaded bars
with a half-open UTC `[first open, last bar end)` span using existing Time Link
semantics (cash intraday caps at session close; daily selection uses UTC date
buckets); only release commits
and requests a report. It shows dates/count and **Compare in Distribution**.
That opens a descriptive selected-versus-visible comparison of the current
transformed values. Both samples use shared bin edges and their own normalized
probability masses. The visible reference includes any selected observations in
its viewport: it is not an independent complement. This comparison has no
inference. It does not silently filter or change an existing DIST lens report.
Crosshair keeps its time pill and displays native-unit change versus the exact
base. Drawings/AVWAP use level coordinates; transformed modes explain that these
tools require level mode. Time Link time centring remains available.

Defaults: Alt+Shift+B toggles the Studio brush, Alt+Shift+D toggles the strip;
help comes from the preferences registry. Existing user keybindings take priority.

## Worker, bounds and scope

Typed StudioCanvas requests carry pane ID, process-wide monotonic token, native
generation/bars/stitch revisions, view/event revision, visible span and committed
selection. Obsolete results release their own pending slot but cannot overwrite a
new context/spec/receipt. The statistical key includes every full source digest,
source identity/provenance/clock/gap witness, all view math inputs, event records,
visible span and selection. Pane identity is not cache identity. A four-entry
ReportCache namespace supplies single-flight; failures are not cached. Bounds are
120,000 supplied bars per source, one asset plus four peers, 2,000 events, 32 bins,
and w=2..1000. Source changes clear displayed arrays before admission; a prior
viewport snapshot may remain labelled PREVIOUS SNAPSHOT with its own receipt.

Transforms, validation, digesting, alignment, sorting, histogram masses,
receipt/last-value cards, flag candidates and native GPU pyramids run on blocking
workers. Paint projects immutable values and performs bounded collision/label
placement; crosshair cards substitute already-prepared values and receipts. Hover
creates no jobs. There is no model persistence or new dependency.

Cuts are small multiples and event-cluster animation, in the specified order.
Independent-axis overlays remain available. As-of filtering, derived operations
and later Studio model/Answers slices remain unavailable. Existing workspace
layout save/dirty serialization is still synchronous (inherited from Slice 1);
this slice adds only bounded view inputs, no report/model serialization. CPU
paint gates do not measure GPU presentation or live provider latency. See
[PERF.md](PERF.md) for workloads, measured results and final checks.
