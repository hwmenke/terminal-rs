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
or (numerically) zero variance. There is no multiple-comparison correction:
with many buckets, |t| ≈ 2 somewhere is expected by chance.

Every heatmap cell and profile shows n; buckets with fewer than **10**
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
