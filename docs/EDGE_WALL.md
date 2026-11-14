# Edge on the Wall

`TWALL` and `TWALL D` include Edge by default, including in older saved layouts.
`EWALL [spec]` opens the same wall sorted by Edge score. Up to 50 loaded names
form the statistical family; overflow names are listed and excluded.

```
                          EDGE 5b · 37/50 computed
SYM   STACK | ribbons |   Δ5b       q        S    n       FOLDS | FLIP CUSH VWAP RVOL SRC
              ...     | ▲ [+3.1bp]  q .03~   +37  n 412   ▮▮▯▮  | ...
```

The glyph identifies the conditioning regime: up, flat or down. A hollow glyph
uses the historical window's last regime when a completed live ribbon is unavailable.
The tile is conditional excess return, in basis points; it fills only when the
wall pass rule holds. Non-passing tiles have an outline and dimmed fold ticks.
`S` is the edge score (below). The four ticks show the signs of the contiguous,
purged stability-fold effects; unknown folds are hollow. `LOW N` warns of fewer
than 30 effective outcomes.

## Inputs and timing

| | Intraday | Archive (`TWALL D`) |
| --- | --- | --- |
| Bars | Verified native regular-session 5m | Verified daily local research archive |
| Regimes | Adaptive Trend defaults, base medium state, finite confidence only | Same on daily bars |
| Horizon | 5 bars (25 minutes) | 5 bars (5 sessions) |
| Window | Last at most 5,000 closed bars through the last completed cash session | Last at most 5,000 closed daily bars |
| Displayed regime | Latest completed live 5m state, falling back to as-of close | Latest completed daily state, falling back to as-of close |

A name needs at least 256 closed bars after admission and gap trimming. Warm-up
bars do not enter outcomes. Each report holds all three regimes, so a completed
live regime flip switches the displayed cell and reruns wall BH without a fetch.
The summary's `CLOSED THRU` is the oldest displayed report end, shown as the end
of the last closed bar (a 15:55 5m bar shows as 16:00); hover gives each name's
first input bar open and its `CLOSED THRU`.
During the session, today's intraday bars never enter Edge's historical window.
The session identity rolls at cash close, using the existing US calendar and
half-day/DST rules. Existing daily closed-bar rules retain the provider's post-close
grace period. Prior results remain visible with their own dates during rollover.

## Statistics

Edge reuses `edge::regime_edge` at horizon 5 and the shared null statistics.
For a warmed bar t whose outcome has completed:

- `r_t = C[t+5] / C[t] − 1`.
- `Δ_r = mean(r_t | regime_t = r) − mean(r_t | all warmed t)`.
- Raw p is a two-sided circular-rotation test of the regime block against fixed
  outcomes, using up to 999 deterministic offsets. Below 99 null samples p is absent.
- Four contiguous, time-ordered folds use equal counts of completed outcomes.
  They are stability folds, not train/test splits: nothing is fitted, the regime
  uses fixed default parameters, and states are causal. An outcome whose 5-bar
  window reaches the next fold's first entry bar is purged, so no fold's labels use
  another fold's bars. The last fold also drops its final 5 outcomes (inherited,
  conservative). Fold Δ subtracts that fold's baseline. A fold is unknown (hollow)
  below eight effective outcomes, so short histories show hollow ticks rather than
  zeros. `n_eff = n/5` describes overlap.

Δ, n, hit rate and folds equal the chart's TSTACK Edge 5b cell on the same window.
Raw p can differ slightly: the rotation offsets depend on the set of horizons
tested together, and the wall tests only horizon 5 (as TSTACK Tune does), while
the chart tests 1, 5 and 20 at once.

The wall is the FDR family because the user scans names and chooses among them.
Each loaded name contributes one test, chosen by its causally known displayed
regime. The report's per-name q is shown in hover as **not used**. Wall q uses the
shared Benjamini–Hochberg step-up procedure across the loaded names.

Pending names and admitted short-history/low-outcome names enter at p = 1.
Unverified, unavailable, non-US-cash and quarantined names are excluded, as are
names whose computed Δ or p is non-finite (`INVALID`): a defect is not evidence.
Equal p get equal q, q never decreases as p increases, and q is capped at 1.
For example, `[.01, .01, .03, .04]` with two pending names gives m = 6 and
q = `[.03, .03, .06, .06]`; those four alone give `[.02, .02, .04, .04]`.
Pending q values are conservative upper bounds for a fixed family and inputs:
resolving a pending name cannot retract another name's pass. The `~` suffix
marks a pass still filling. A regime change or a change of family can change q.

The pass rule clones the existing EdgeCell, substitutes wall q, and calls
`EdgeCell::passes()`: at least 30 effective outcomes, wall q ≤ 0.10, nonzero Δ,
and at least three of four fold Δ signs agreeing with the overall Δ.

The score separates statistical evidence and stability from effect size:

```
a = number of folds with fold_Δ × Δ > 0
S = 100 × sign(Δ) × min(−log10(q_wall), 3)/3 × a/4
```

Missing q, low effective n, zero/non-finite Δ yield no score. q ≤ .001 is capped.
For positive Δ, q = .033 and three agreeing folds: S ≈ +37. A score magnitude
of 25 alone does not imply a pass. Effect size remains in the Δ column because
basis points are not comparable across names with different volatility.

## Controls and reading the wall

| Control | Action |
| --- | --- |
| E | Show Edge and sort by score, then cycle score → Δ → q |
| Shift+E / Edge button | Show/hide Edge; hiding cancels unfinished work |
| X / Scatter button | Toggle Δ versus −log10 q scatter |
| Δ5b / q / S header | Select that Edge sort (Δ, q, score) |
| S | Four existing stack sorts; from an Edge sort returns to Stack |
| Cell/dot click | Select the underlying row; scatter selection scrolls to it |
| N / Shift+N / Backspace | Existing hand-off and return, retaining Edge results |

Keys require pane focus and an empty command line. `TWALL EDGE` remains a wall
of ticker EDGE. `EWALL US Equity` loads that security. `NEW EWALL` and
`SPLIT DOWN EWALL` support pane creation.

Score sort uses |S| descending, q ascending, |Δ| descending, then list order.
Δ sort uses signed Δ descending; q sort uses q ascending and |Δ| descending.
Missing/non-finite values sort last, with scored rows first within the missing
set, then list order. Historical Edge may remain valid on a stale ribbon row.
While rows are still pending, an Edge sort is partial: the header turns amber
(`EDGE 5b · k/50 computed`) and the toolbar shows
`EDGE SORT PARTIAL · k/50 computed · pending last`.

The summary shows tests m, passing names split by sign, median/IQR Δ, a symmetric
Δ strip plot, a heat key, dates and fill progress. Below ten trials it shows
`SMALL WALL`; a lone name requires p ≤ .10/m. The scatter clamps −log10 q at 3,
marks Δ = 0 and q = .10, fills passing dots, labels their tickers, and rings the
selected row in amber. Names without q are counted separately. Hover a cell or
dot for the formula, hit rate and Wilson interval, raw p/null band, wall/per-name
q, fold dates/counts, score arithmetic, provenance and source details.

## Honest blanks

| Code | Cause | Counts in m |
| --- | --- | --- |
| UNVERIFIED | Sample, missing/unverified provenance, or unwitnessed mixed history | No |
| NO DATA | Missing archive name or feed outage without usable vault history | No |
| NOT US CASH | Intraday symbol/session unsupported | No |
| QUARANTINED | Vault withheld fresh data | No |
| SHORT HISTORY | Fewer than 256 closed bars after session cut/gap trim | Yes, p = 1 |
| LOW N | Too few outcomes or null samples for the displayed regime | Yes, p = 1 |
| INVALID | Non-finite Δ or p (a defect, never painted as a number) | No |
| ··· | Work pending | Yes, p = 1 |

Blank cells show an em-dash and reason, with no statistical digits. Hover explains
why and whether the name enters m. A regime with valid statistics but fewer than
30 effective outcomes remains outlined with a `LOW N` warning. An all-same regime
can show zero Δ and q = 1, and never passes.

## Real data, caching and cost

Validation precedes filtering: sample/unverified inputs cannot be hidden by a
session cut. Mixed real/cache tags require the Bar Vault's valid stitch witness.
Only the observed closed prefix enters; without an observation clock the last
session date is dropped conservatively. The window starts after the last vault
gap inside it; gaps are never bridged. Quarantined stitches are refused. Archive
Edge reads local daily history only, with no market provider or bridge fallback.
The sample provider triggers a refusal banner and zero Edge fetches.

Ribbons load first, then Edge streams rows as they finish. Rows on screen, plus six
rows each side, start first; the rest follow in display order. Scrolling or
re-sorting during a pass re-ranks the rows not yet started (`WallEdgeFocus`, no
fetch). At most six rows are in flight per pass; four wall permits bound
acquisition, and four shared compute permits bound `spawn_blocking` across panes.
No network/disk work or indicator/null computation runs on the UI thread. A separate
Edge generation cancels abandoned passes; late generations and mismatched symbols
are ignored. Ribbon refreshes keep Edge results and send no extra Edge request.

The acquisition memo deduplicates a symbol/mode for the completed session day,
including second panes and reopens. It holds at most 128 completed entries and
never retains errors. The shared 256-entry report cache keys exact admitted bar
inputs (digest, count, endpoints), timeframe, provider source identifier, vault
epoch, session date, default parameters, horizon and code version. A changed label
with identical inputs does not cause a false miss. Corrections/source/parameter
changes invalidate the report when newly acquired; same-day acquisition memo hits
do not poll the provider or archive for corrections. The lane cache holds 256 keys,
enough for 50 names × four intraday lanes.

**Vault reuse (intraday).** Before a deep fetch, Edge reads the Bar Vault's 5m
series for the name. If it has exactly one source, its last admitted fetch is at or
after the close of the last completed session (the wall's own ribbon refresh
usually is), and it already holds a full contiguous 5,000-bar closed window, that
window is used with no provider request. The input is the same window a deep fetch
would produce; the fetch could only re-confirm or, rarely, correct older bars.
Otherwise Edge makes one deep request for the name.

Cost for a cold 50-name intraday wall: the ribbon pass makes about 200 requests
(4 lanes × 50). Edge then adds up to 50 deep 5m requests, each clipped by Yahoo to
about 59 days (≈3,100 bars), and all paced at ≥200 ms apart, so ≥10 s for Edge
alone. A vault already holding 64+ contiguous sessions per name needs 0 Edge
requests. Archive Edge makes 50 SQLite reads. Warm same-day opens need no
acquisitions. Measured compute (release build, unit-level): ≈8 ms per name at
3,100 bars and ≈13 ms at 5,000, so about 0.4–0.7 s CPU for 50 names, spread over
four blocking threads. Provider latency is not measured here. `cargo bench -p tr-data --bench wall_edge` can measure real inputs
provided via `EDGE_WALL_BARS_JSON` (a verified regular-session 5m Bar JSON export);
it performs no fetch and does not fabricate bars.

## Limits

Intraday five-bar outcomes can span the overnight close: this is inherited from
Edge Proof. Small/short histories limit p resolution and FDR power. Live regime
selection is causal, but repeated viewing across dates/regimes is not an additional
across-time FDR correction. Names on a wall are correlated (shared market moves),
and two-sided tests are not guaranteed to meet BH's positive-dependence (PRDS)
condition, so the 10% FDR is nominal rather than guaranteed. Edge is descriptive
historical evidence, not a forecast.
