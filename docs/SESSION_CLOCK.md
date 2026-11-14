# Session Clock (`CLOCK`)

`CLOCK` uses the focused ticker. `AAPL CLOCK` chooses a name, `AAPL CLOCK 1h`
chooses cash-hour buckets, and `NEW CLOCK` adds a pane. The default is 30 minutes:
13 columns from 09:30 through 15:30 ET, with Mon–Fri, ALL and TODAY rows.
`R`, Reload, or the unavailable card retries. Visible panes refresh once a minute
through the application's update lifecycle during cash hours, including the close.
The amber outline follows the current cash bucket. Narrower buckets scroll horizontally;
the default grid fits a 600 px pane.

## Keys and selection

| Key / control | Action |
| --- | --- |
| Click historical cell | Select a weekday/ALL bucket, including holes and LOW N cells |
| Arrow keys | Start selection at Mon's first bucket; move within the grid |
| Click Session Clock heading | Select the symbol without a bucket |
| N | Open this symbol's 5m chart with Auction Map enabled |
| Shift+N | SPATH, BRIEF or COCKPIT |
| Backspace | Return through a live hand-off stack |
| R / Reload | Retry the existing worker request |

N needs a selected cell or symbol heading. A bucket is named in status; when it
has observations, the chart uses its existing timestamp jump to the most recent
contributing closed session, applying it when bars arrive. Holes and symbol-only
selections do not jump. TODAY remains descriptive. Command/text inputs retain their
keys. See [Next ▸ hand-off](NEXT_HANDOFF.md) for the crumb and layout restoration.

## Observations and admission

The worker reads only `history_for_analysis`, with the shared
`cash_session::research_fetch_limit` for the chosen intraday timeframe. Daily and higher
bars, unsupported cash grids, non-US sessions, sample data and unverified inputs are
refused. Refusals show **NO REAL DATA**, without numeric market or statistical placeholders.
There is no sample fallback. Verified cached inputs are marked **STALE** because they
lack a fresh observation clock; input behind the expected cash grid is also marked stale.

Session dates, bucket opens and single-print checks use the shared cash-session helpers.
For each session and bucket, the observation is `10,000 × (close / open − 1)`, in basis
points. Exactly one usable print is required. Missing prints, ambiguous buckets and invalid
OHLC ranges do not vote; no interpolation, forward fill, later open substitute, or merging
of duplicate prints occurs. A hole affects its own bucket, rather than discarding the other
usable buckets in that session. Premarket and after-hours prints are ignored. Early-close
days stop at their own close and contribute nothing to later columns. A final cash-hour
bucket can be shorter than an hour.

TODAY means the current New York date and never enters any statistical sample, including
after the close. Each prior session supplies at most one return to each applicable weekday
cell and to its ALL cell. These returns do not overlap within a cell. The ALL row and the
weekday rows share observations, so their tests are dependent.

The completed prefix comes from `closed_len`, using the input's observation time rather
than advancing stale data with wall time. A further cash-bucket end check handles hourly
buckets that begin at :30 and shortened final buckets. TODAY numbers appear only as buckets
close; its row stays outlined because it has no p or q. Holes remain blank. A stale final
print is excluded from prior-session inference as well. A duplicate remains ambiguous even
if only one of its prints is in the completed prefix.

## Cell tests and BH

For each cell, count positive and negative returns and drop exact zeros from the sign test.
Under equal sign probability, the exact two-sided p is twice the probability of observing
at most the smaller sign count in a fair binomial sample, capped at one. An all-zero cell
gets p = 1. The test concerns directional balance, not a t-test of the displayed mean.

Apply Benjamini–Hochberg together across all weekday and ALL cells with at least eight
observations. Sort their p values, multiply each by the number of tests divided by its
one-based rank, take the running minimum from the largest rank back, and cap at one.
These are the displayed q values in the original cell order. Low-N cells have no q and
receive a corner tick. Zero returns count toward the displayed n, mean, median and hit-rate
denominator, but not toward the binomial sign counts. The hit rate is the share strictly
positive. Hover also shows the date span and a mean/sample-standard-error t statistic;
the t statistic never controls colour. It is absent when the standard error is zero.

## Joint permutation test

The joint statistic is the population variance across the available ALL-row bucket means.
For each of 999 permutations, independently shuffle the observed returns among the observed
bucket labels within each session, recompute the ALL means, and measure the same variance.
The missingness mask stays fixed: holes and times after an early close never receive values.
The deterministic SplitMix64 seed uses the sample's first/last dates, session count and grid
size. TODAY data and wall-clock repainting cannot change the seed.

The Monte Carlo p is `(1 + number of permuted statistics >= observed) / 1000`. If fewer than
two columns have observations, the joint p is unavailable and fills are withheld. The null
assumes the observed bucket labels are exchangeable within each session; actual intraday
volatility patterns and selective missingness can violate this assumption.

## Display and interpretation

A historical cell gets a fill only when **n ≥ 8, q ≤ 0.10 and joint p ≤ 0.10**. The header
counts cells passing both gates: `k of m pass · joint p …`. If the joint test fails or is
unavailable, **NO CLOCK EFFECT DETECTED** appears and every historical fill is withheld.
Other cells retain a muted number and outline, or a dash when there is no observation.

The reusable five-step symmetric ramp has fixed thresholds, printed beside the grid:
≤ −10, −10 to −2, −2 to +2, +2 to +10, and ≥ +10 bp. It does not rescale to make small
noise look large. The key is illustrative; its swatches are not inferential cells.

All statistics are computed on data arrival in the worker/data crate. Paint formats the
cached report and reads wall time only for the current-column outline. Painting a report
does not fetch history or rerun inference.

These are descriptive comparisons of the available history, not a trading signal or
forecast. BH q is a multiple-comparison adjustment under its assumptions, not the probability
that a particular cell is false. The additional joint gate is conservative but does not
remove dependence, selection bias, changing market conditions, transaction costs, or the
exchangeability limitation of the permutation test.
