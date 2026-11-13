# Seasonality Studio

`SEASON` (alias `SSN`) opens a pane for the focused security. Use `AAPL SEASON`
for daily research, or `AAPL SEASON 5M`, `15M`, `30M`, or `1H` for intraday.
`NEW SEASON` creates another pane. The palette and hotkey reference list these
commands. Ticker and timeframe persist with the workspace; Reload recomputes.

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
- **Turn of month:** the exchange calendar's last three and first three sessions
  versus other sessions. Missing data cannot change which day belongs to a group.
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

Overlapping windows share future observations. Effective n = raw n / H is a
non-overlapping equivalent, **not** a measured independent sample size or an
inferential correction. Profile t-stat = mean / (sample stdev / sqrt(n)); it is
undefined for n < 2 or zero variance. These descriptive t-statistics have no
serial-dependence or multiple-comparison correction.

Every heatmap cell and profile shows n; fewer than **3** observations are grey.
Sparse forward distributions have counts and grey summaries but no coloured
curve. Three observations are a display threshold, not evidence of statistical
significance. Chart medians use cyan markers. Hover reveals bucket details;
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
