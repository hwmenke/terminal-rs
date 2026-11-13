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
