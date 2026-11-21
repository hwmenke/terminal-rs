# Seasonality Studio

`SEASON` (alias `SSN`) opens a pane for the focused security. Use `AAPL SEASON`
for daily research, or `AAPL SEASON 5M`, `15M`, `30M`, or `1H` for intraday.
`NEW SEASON` creates another pane. The palette and hotkey reference list these
commands. Ticker and timeframe persist with the workspace; Reload recomputes.

`SEASON` and `SSN` are also listed tickers. Only the bare word or the word plus a
valid SEASON interval (`SSN 30m`) opens the studio; `SEASON US Equity`,
`SSN Equity`, and any other arguments (`SEASON 4H`) load the ticker instead.

This pane supports US cash-session equities, ETFs and indices. It does not use
sample or display prices. Sample mode shows an empty state. Empty history,
unverified/sample-tainted history, invalid prices, duplicate timestamps and
misaligned intraday timestamps produce a clear refusal. No values are generated
to replace missing data. Both the data calculation and response acceptance check
verified provenance; older response generations cannot overwrite newer requests.

## Data and clocks

Daily bars come from the same local Yahoo research archive used by STATS, with
verified Whats-News daily history when the local archive is empty. Intraday
requests use `analysis_bars`: native `history_sourced` fetch, vault admission and
stitching, and verified vault history during an outage. Computation runs in
`spawn_blocking`, away from the GUI thread. No new dependencies are required.

Daily timestamps denote UTC session dates. Intraday buckets use the existing
New York exchange calendar and DST rules, starting at 09:30. Only regular-session
bars whose actual interval has closed are admitted, including the shorter final
hour on full and half days. The last source observation closes bars; reopening
a stale vault never substitutes wall time for that observation. The pane shows
source, provenance, observation time, date coverage and bar count. Calendar
conditions use the current New York month and weekday even for stale history.

All returns below are simple fractional returns, displayed as percentages.
There is no annualisation or missing-session fill.

## Daily views

- **Year × month heatmap:** prior-month final close to current-month final close.
  A month requires every expected session and its prior closing anchor. Partial
  first/current months and months with a hole remain empty. Each cell reports
  observed valid daily-return n and the expected count on hover.
- **Month of year:** mean, median, positive-return hit rate, sample t-stat and n
  across *complete monthly returns*. Here n counts months/years, not days.
- **Weekday:** close-to-close daily returns assigned to the ending session.
- **Turn of month:** the standard −1…+3 window (Lakonishok–Smidt): the last
  exchange session of a month through the third session of the next, versus
  other sessions. Missing data cannot change which day belongs to a group.
- Every daily table ends with an **All** row (every return in the sample; every
  complete month for the month table) so bucket hit rates and means are read
  against the actual unconditional baseline, not against 50% / 0. The monthly
  hit-rate chart draws that baseline as a cyan line.
- **Week of month:** calendar date bands 1–7, 8–14, 15–21, 22–28 and 29–31.

Daily returns require adjacent exchange sessions. Monthly returns never scale
an average daily return into an estimated monthly result.

## Intraday views

Mean open-to-close bar return, mean absolute bar return (the volatility smile),
and mean share of session volume are plotted by New York opening clock bucket.
A weekday × clock heatmap adds a second calendar dimension. Bucket returns use
the actual bar open and close, so overnight changes are excluded from these
profiles. Empty expected clock buckets remain visible with n=0.

Volume shares are calculated per session before averaging. Only sessions with
every expected timestamp and positive finite total volume contribute; partial
sessions cannot bias the volume curve. Zero-volume sessions produce no volume
share, rather than division by zero. Half days contribute only their actual
buckets, so bucket counts may differ and the averaged curve need not sum to
100% when half and full sessions are combined.

## Conditional forward distributions

For the current NY month and weekday, the pane compares completed 5/10/20-bar
close-to-close forward returns with all completed outcomes. These distributions
include overnight changes. A missing expected bar/session invalidates every
forward window crossing it; the last H bars have no completed H-bar outcome.
Histograms use shared edges and each group's own probability mass, with mean,
median, hit rate, raw n and effective n alongside them.

Overlapping windows share future observations, so a naive iid t-statistic over
all H-bar windows would overstate significance by roughly √H. The forward table's
t-stat therefore uses **only non-overlapping windows**: scanning chronologically,
a window is kept when it starts at least H bars after the previously kept one.
The independent n is printed under each table. Mean, median and hit rate still
use every completed window. Profile (one-period) t-stat = mean / (sample stdev,
n−1 / sqrt(n)); those returns do not overlap. Any t-stat is undefined for n < 2
or (numerically) zero variance. These legacy descriptive tables have no multiple-comparison correction:
with many buckets, |t| ≈ 2 somewhere is expected by chance. The intraday Evidence
view below supplies a separately labelled HAC mean test and fixed-family BY adjustment.

Every legacy Calendar heatmap cell and profile shows n; buckets with fewer than **10**
observations are flagged as small samples (grey). Sparse forward distributions
have counts and grey summaries but no coloured curve. The threshold is a display
flag, not evidence of statistical significance.

**Bar Vault gaps.** When intraday history contains a vault UNVERIFIED JOIN gap
(non-overlapping vault join), the pane shows an amber warning with the gap
dates, and the gap acts as a session break: no return or forward window spans it. Chart medians use cyan markers. Hover reveals bucket details;
horizontal scrolling preserves legible labels in narrow docks.

## Validation and limits

Offline deterministic tests cover known monthly and forward returns, calendar
bucket counts, missing/partial months, missing-session windows, too few bars,
flat prices, NaN, duplicate sessions, provenance gates, DST alignment, pending
bars, early closes and zero volume. App tests cover parsing, pane settings JSON
round trips, stale-generation rejection and provenance refusal. Headless painter
checks exercise sparse and flat-price reports at 280- and 1440-point widths;
worker tests verify archive-only daily reads and sourced intraday reads using
in-memory fixtures, including zero provider fetches in sample mode.

There is no minimum-history extrapolation, forecast, corporate-action adjustment
beyond that supplied by the archive/provider, holiday-calendar replacement, or
continuous live subscription. Reload takes another verified snapshot. A month
profile needs several complete years to meet the display threshold. Intraday
history depth depends on provider and vault coverage. Futures, FX, crypto,
non-US sessions and extended hours are intentionally unsupported.

## Opening-range conditional lens (native 30m)

Open `AAPL SEASON 30m`, then select **OR Lens**. This local tab reuses the
SEASON snapshot and worker lifecycle. Tab, horizon and cell selection are ephemeral;
existing saved ticker/timeframe layouts and preferences load unchanged.

The exact 09:30–10:00 New York bar defines ORH and ORL. At each completed
origin close, `Above` means C > ORH, `Below` means C < ORL, and `Inside` includes
equality. Conditions first exist at 10:00. A missing opening print leaves the
session unclassified. Columns are **origin close times**, 10:00 through 16:00,
not bar opening times. The 10:00 close is necessarily Inside; its empty complement
is honest. The last columns also have unavailable outcomes at longer horizons.

Horizon chips select completed **30/60/120 minute** close-to-close outcomes,
`R = 10,000 × (C[t+h]/C[t] − 1)` in basis points. Every intervening expected
bucket must exist and be closed under the source observation clock. Endpoints
stay in the same session; half days never borrow afternoon or overnight bars.
**TODAY is excluded** from this historical lens, even after the cash close.
Vault UNVERIFIED JOIN gaps break both forward windows and any OR-to-origin
condition crossing the join. Missing data is never filled or shortened.

Click a heatmap cell to compare its state against **all other states at that
same origin clock and horizon**. Both normalized histograms use the same twenty
bin edges, and each nonempty group's mass sums to one. Raw n and descriptive
means remain visible for sparse groups. The all-state mean is context containing
the selected observations, not an independent comparator. No strategy P&L or
recommendation is calculated.

The heatmap shows the mean contrast Δ in bp with the printed shared key.
Signed numbers use sign roles; state/distribution colours identify categories.
An outline marks asymptotic BY q ≤ 0.10, without a significance hue. A corner
tick marks **LOW N** (fewer than 30 observations in either group). Hover cards
explain formulas, substitutions, both counts, expected session span, bandwidth,
source, dates, observation time, CLOSED THRU and unavailable reasons.

Inference uses the explicit expected exchange-session grid between the first
and last admitted historical sessions. Missing sessions have zero score and
preserve lag distance. For group g and its complement, the score is
`u[s] = I[g](R−mean[g])/n[g] − I[¬g](R−mean[¬g])/n[¬g]`.
Bartlett HAC variance is `Σu² + 2Σ(l=1…L)(1−l/(L+1))Σu[s]u[s−l]`, with
`L=min(5,T−1)` for T expected sessions. There is no iid/sample-variance
substitution or small-sample correction. A mean helper uses `I(R−mean)/n`
on the same grid for later slices.

With **at least 30 observations in each group**, the whisker is the approximate
pointwise **Δ ± 1.96 SE** interval and the two-sided p-value uses a standard
normal approximation. Zero/numerically degenerate variance produces unavailable
inference, never an infinite test statistic or a manufactured p=0. The complete
fixed **3 states × 13 clocks × 3 horizons = 117** family enters BH followed by
the Benjamini–Yekutieli harmonic multiplier: `q=min(1,H₁₁₇×q_BH)`.
Unavailable cells enter at p=1. Changing horizon or selected cell cannot change
q-values or the family. Intervals are pointwise, not simultaneous.

These are **asymptotic**, bandwidth-sensitive methods. BY adjusts dependence
between tests only when their marginal p-values are valid. Repeated exploration
across symbols, dates and settings is outside the displayed family. Provider and
vault depth may leave most comparisons unavailable; thresholds are never relaxed.
Corporate-action continuity remains limited by existing source/vault evidence.
The original Calendar views retain their existing descriptive methods; the
multiple-comparison limitation described above still applies to those older views.

Acceptance tests cover hand-calculated states/returns/contrasts, direct HAC and
BY references, missing-session lag spacing, causal classification, missing opening
and intervening prints, early closes, gaps, pending endpoints, full-input admission,
shared histogram mass, selection invariance, generation/provenance refusals and
finite geometry at 320/1440 px. All generated inputs are test/dev fixtures only.

## Season evidence ribbon (intraday)

Select **Evidence** in an intraday SEASON pane. A local **Descriptive / Evidence**
control changes presentation only: both modes consume the same immutable cached
report. No selection changes means, intervals, p-values, q-values or membership.
These controls are ephemeral; existing saved layouts and preferences load unchanged.
The original Calendar daily/monthly and forward views retain their methods. CLOCK's
sign/permutation tests are unchanged and are not equivalent to this mean test.

The clock heatmap includes **ALL** plus Monday–Friday, using the same closed
open-to-close bar returns as Calendar, with NY **opening** clock labels. Every
expected full-session bucket stays present, including n=0 cells. Signed mean
numbers use sign ink. The shared diverging heat key uses exactly the cells'
thresholds. Descriptive mode fills observed means without implying evidence;
Evidence mode fills only cells with an available asymptotic **BY q ≤ 0.10**.
A neutral outline also marks passing cells, and a caution corner tick marks
**LOW N (<30)**. There is no significance hue. Absolute-return and volume-share
profiles remain descriptive magnitudes, drawn with categorical rather than sign
colour; their heights say nothing about directional evidence.

Beneath the heatmap, the large **ALL CLOCK** curve plots means about a symmetric
zero axis with approximate pointwise **95% HAC whiskers**. Descriptive means remain
visible with sparse counts; inference requires **n ≥ 30**. Unavailable intervals
are omitted. Zero or numerically degenerate variance has no interval or p-value,
rather than an infinite statistic or fabricated certainty. Hover either a heat
cell or an ALL curve bucket for the MathCard: formula, substituted inputs, n,
expected session span T, bandwidth L, SE, interval, p, q, family size, coverage,
dates, source/provenance, observation time and actual CLOSED THRU. Empty cells
explain the unavailable reason. Partial days use the actual closed buckets only;
an afternoon observation is never invented for an early close. Unlike OR Lens,
this extension preserves Calendar's inclusion of today's actually closed bars.

For each of the six rows and B full-session clocks, on the **expected exchange
session grid** between the first and last admitted dates:

```text
R[s] = C[s]/O[s] − 1
mean = Σ observed R / n
u[s] = I[s] (R[s] − mean) / n
L = min(5, T−1)
Var(mean) = Σ u[s]² + 2 Σ(l=1…L) (1−l/(L+1)) Σ u[s]u[s−l]
SE = sqrt(Var); pointwise CI = mean ± 1.96 SE
p = 2 Φ(−abs(mean/SE))
q = min(1, H_m × BH(p)), H_m = Σ(j=1…m) 1/j, m = B (ALL family), or 5B (weekday family)
```

The existing `studio_inference::hac_mean` supplies this five-session Bartlett
HAC estimator. Missing sessions and nonselected weekdays have zero score and
retain lag distance. Explicit vault joins reset lag products while preserving
the expected grid and bandwidth. There are now **two fixed predeclared BY
families**: the B ALL-row clocks, and the 5B weekday-row clocks. At 5m, their
sizes are 78 and 390. Unavailable/LOW N tests stay in their family at p=1.
Family chips print membership; selection or presentation cannot change q-values.
This avoids penalising the ALL row with hundreds of mostly sparse weekday tests.

The thin whiskers remain **pointwise** HAC intervals. A separately labelled shaded
**simultaneous 95% ALL band** arrives from a second worker response when Evidence
opens. Stationary bootstrap (Politis–Romano, 1994) samples whole expected-session
rows using shared geometric blocks of mean ceil(T^(1/3)), 500 draws, seeded from
the admitted bars digest and gap/timeframe identity. Missing cells remain missing;
block continuation stops at joins (including circular wrap when runs differ).
Original HAC SEs use each clock’s own join boundaries. For each draw,
`M* = max_clock |(mean*−mean)/SE_HAC|`; the upper 95th empirical critical value c
forms `mean ± c SE_HAC`. These are **fixed-original-HAC studentizers**, not a
refitted/bootstrap-t pivot or a stepdown hypothesis test. This max-statistic
construction follows the simultaneous-inference principle of Romano–Wolf (2005).

Only ALL clocks with valid original n≥30 HAC inference enter the max. Other clocks
retain no interval, and no simultaneous coverage is claimed for them. If any draw
has fewer than 30 observations in an eligible clock, the whole band is unavailable;
replicates are never silently dropped. The MathCard prints the critical value,
block length, draws, seed, eligible membership and unavailable reason. Independent
polygon strips skip unavailable clocks. This curve is indexed by aggregate clock,
not chronological bars; no polygon connects dates across a vault gap.

The band runs in a separate cached blocking job, adds no provider calls and does
not delay the mean/BY report. Generation and digest checks reject late band
responses. Reload/sample/refusal clears it. The immutable first-stage report retains
the session grid used by the bootstrap; paint performs no resampling or inference.

Both HAC and block-bootstrap coverage are approximations. BY cannot repair invalid
marginal p-values and does not make pointwise whiskers simultaneous. The separate
band does not change BY hypotheses or q-values. Provider/vault depth, bandwidth and
block-length sensitivity, source corporate-action evidence and repeated exploration
across symbols/dates/settings remain limitations. Sparse weekday rows stay sparse;
thresholds and memberships are never relaxed to fill the display.

Complete input validation precedes trimming, digesting and cache reuse. Reports
are built in the existing worker job and stored as immutable `Arc<SeasonReport>`
in bounded `ReportCache`; the key includes the full bars digest, symbol, interval,
source, provenance, observation clock, New York calendar date and vault gaps. Corrections,
clock changes and changed gaps invalidate. Sample mode, refusal and generation
rejection retain the existing clear/no-fetch rules. No statistics or history
scans occur in the new painter.

New tests cover direct mean/SE/CI references, the two fixed ALL/weekday BY families,
missing-session lag spacing, join resets, early closes, pending closure, empty/
zero/sparse cells, n=29/30, complete-input refusal, cache identity/invalidation,
mode invariance, matching heat keys/fills, complete cards, actual hover cards and
finite 320/1440-point geometry. See PERF.md for gate IDs and verification status.

The v2 inference tests also cover deterministic stationary draws, common-row max
statistics, unavailable clock retention, second-stage cache reuse without fetches,
and rejection of wrong-generation/digest results. The release season paint gate
includes the accepted simultaneous band without changing its existing budgets.

## Studio shell route

`STUDIO lens=season` embeds this same state, report Arc and second-stage band
worker. Supported intervals and US cash-session admission are unchanged. The
shell explains unsupported intervals without substituting another timeframe.
Accepted reports remain available after lens roundtrips; sample/context changes
clear all dependent children. Globally allocated request tokens and existing
band keys reject obsolete responses. See [Studio](STUDIO.md).
