# Studio When lens and selected-range comparison

```
AAPL STUDIO 5m lens=when rule="RSI(14)<30"
AAPL WHEN 5m RSI(14)<30
WHEN h=6 threshold=10bp
```

`WHEN` opens a Studio on the When lens (daily when no interval is given). With
no symbol, interval or rule, `WHEN h=… threshold=…` adjusts the focused Studio's
When lens instead of opening a pane. `STUDIO … lens=when` accepts the same
`rule=` (quote rules with spaces), `h=` and `threshold=` keys; they are refused
for other lenses. COND (STATS conditional tab) has an explicit **Open in When**
button that opens a daily When Studio on the same rule; COND's own descriptive
results are unchanged.

Supported data: verified daily archive bars, or native 1m/5m/15m/30m/1h US
cash-session bars from the existing research path (`analysis_bars`). Crypto,
futures and non-US sessions are refused with a reason; other intervals show an
explicit unavailable card. Sample mode makes no When/subset provider calls and
Studio clears every child, mask and overlay.

## Rule, grid and admission

Rules use the existing STATS grammar (`stats::parse_conditions`, `match_mask`,
1–8 comparisons joined by AND, ≤1,024 chars). `stats::bounded_feature` exposes
the existing causal indicator implementation (periods 1–1000, ≤120,000 bars);
indicator parsing is not duplicated. The editor prints the parsed echo; a draft
does not replace the accepted snapshot until **Run rule** (or Enter).

The full supplied source is validated (`studio_canvas::validate`, verified
provenance, OHLCV/clock checks, US-session requirement) before any trim,
indicator warm-up, digest or cache lookup. `closed_len` uses the source
observation clock; the forming bar is excluded. Only regular-session opens are
used for intraday. The expected grid has one slot per expected bar open
(`intraday::regular_opens`, half days included) or per exchange session for
daily bars. A missing expected bar stays missing (rule = unavailable) and resets
indicators; stitch joins (vault gaps) also reset. Ordinary session transitions
do not reset indicators (the same convention as the native chart). The mask is a
`ScalarSeries` with `rule (1) / complement (0)` values and `None` for warm-up or
missing slots. Rule and bucket features are causal; prefix invariance is tested.

## Outcomes and the fixed family

Forward simple return, in basis points:

```
R[t,h] = 10⁴ (C[t+h] / C[t] − 1),   h ∈ {1, 3, 6, 12} bars (fixed)
```

Every intervening expected bar must exist and no break may intervene. Intraday
outcomes never cross the session close; daily outcomes cross ordinary sessions
but not missing sessions or joins. A slot with no completed outcome is not
scored. The complement is every other admitted scored slot.

Clock adjustment is a fixed-effects contrast. Clock `c` is the bar-of-session
index (one clock for daily). With `g` the rule indicator:

```
g̃ = g − ḡ_c,  R̃ = R − R̄_c,  τ̂ = Σ g̃ R̃ / D,  D = Σ g̃²
s_t = g̃_t e_t / D,  e = R̃ − τ̂ g̃
Var(τ̂) = Σ s² + 2 Σ_{l=1..L} (1 − l/(L+1)) Σ_t s_t s_{t−l},  L = h − 1 + 5
```

Scores stay on the uncompressed expected grid (unscored slots contribute zero),
and products across sessions (intraday), missing bars and joins are excluded
(`studio_inference::scalar_score_variance`, `clock_contrast`). The estimand is
an explicit overlap-weighted within-clock contrast: clocks with no rule or no
complement variation have g̃ = 0 and contribute no identifying information. The
same calculation on `I(R > X)` gives the event (hit) contrast. Normal two-sided
p; 95% pointwise intervals τ̂ ± 1.96·SE.

The testing family is declared before any selection: **eight tests** (mean and
event × four horizons), Benjamini–Yekutieli q (`nulls::Family::by`). Unavailable
members stay in the family at p = 1; choosing a displayed horizon never changes
q. Inference requires ≥30 observations **and** ≥20 contributing sessions per
group; below that the descriptive estimate is retained and marked LOW N.

Each horizon reports raw scored observations, contiguous-rule **episodes**
(scored rule runs, broken by session/gap) and the variance-equivalent effective
n = raw n × Var(lag-0 scores)/Var(HAC). Episodes are never treated as
independent observations.

Hand vector (test `when_clock_contrast_hand_vector`): two clocks with outcomes
`[3,5,1,3]` and `[13,15,11,13]`, rule `[1,1,0,0]` in each → τ̂ = 2 bp, residuals
`−1,+1,−1,+1`, scores `[−.25,.25,.25,−.25,−.25,.25,.25,−.25]`; with L=1 and no
breaks Var = 7/16, SE = √7/4.

## Buckets, distributions and probability tiles

Buckets use the first non-constant rule operand (e.g. `RSI(14)`). Each slot's
quintile edges are Type-7 q20/40/60/80 of finite feature values **strictly
before** that slot (expanding, ≥30 prior values; Fenwick order statistics), so
neither current nor future values enter an edge. All five slots are always
present; empty buckets print “—” with an unavailable card. Bucket means are
descriptive (no bucket inference or family).

The selected horizon (presentation only, no recompute) drives two normalised
shared-bin distributions (32 equal-width bins over the pooled domain; each
nonempty group sums to one; tie-complete ECDF distance in the card) and the
five-bucket strip.

Probability tiles show the event definition `R[t,h] > X bp`, conditional rate,
base rate (all scored slots, including the rule group), lift and a 95%
percentile **session-block bootstrap** interval: a second cached worker stage
resamples whole expected-session rows (stationary bootstrap, 499 draws, mean
block ceil(S^(1/3)), seed = accepted report key, joins break blocks). The same
resampled sessions serve all four horizons. Replicates in which a group (or the
base event) is absent are excluded and counted; more than 24 (5%) withholds the
interval with the reason. Inference needs ≥30/group and ≥20 sessions/group. If
the bootstrap is unavailable, the reason is printed; Wilson is never substituted.

## Selected-range comparison (Distribution)

When a UTC span `[start,end)` is committed (Brush or Inspector) and Distribution
has an accepted verified report, Studio requests `StudioSubset`. It compares
closed one-bar log returns (bp) inside the span with all other admitted returns
of the same report, on 32 shared bins, printing selected/rest counts and date
ranges. Only adjacent expected bars inside one session count (overnight intraday
moves and missing intervals are excluded). The ECDF distance is descriptive; its
95% interval uses a stationary bootstrap over whole sessions (499 draws, mean
block ceil(S^(1/3)), generated block-by-block with an exact integer ECDF sweep),
never iid KS tables. ≥30 returns and ≥20 sessions per group are required for the
interval. This replaces nothing: the canvas's selected-versus-visible Compare and
all Distribution results are unchanged.

## Canvas shading

The native chart draws a low-alpha accent wash over contiguous accepted rule
runs plus a printed `RULE · … · n slots` label. Runs are built on the worker
from the accepted report mask; paint binary-searches visible native timestamps.
Editing a draft keeps the previous accepted snapshot visible (with a caution
note) until Run; a refusal, sample mode or native history revision clears the
report, mask and shading.

## Worker, keys and cache

`Request::StudioWhen {pane, generation, symbol, tf, rule, threshold}` builds one
report (mask, all horizons, family, buckets) through `ReportCache`
(`studio.when.v1`, capacity 4). The key covers the canvas input digest (OHLCV,
provenance per bar, source, observation clock, stitch gaps/epoch), the method
schema, the complement definition, the canonical rule AST (sorted unique AND
clauses) and the threshold; a corrected old bar changes it.
`StudioWhenBootstrap` is a separately cached second stage (`studio.when.bootstrap.v1`,
capacity 8) keyed by the report key. `StudioSubset {pane, generation, input,
selection}` is cached by Distribution digest plus span (`studio.subset.v1`,
capacity 4). Every request carries a fresh process-wide token; responses must
match the pane's token, symbol, interval, applied rule/threshold, verified
provenance, report key and (subset) span/digest. Studio drops When/subset
snapshots when the native history revision changes. Reports are bounded
(≤120,000 grid slots, ≤20,000 sessions; no retained bootstrap draws).

Saved layouts store only the rule, threshold and selected horizon
(`StudioInputs::{condition, when_threshold, when_horizon}`, serde defaults).
Copy command reproduces `STUDIO … lens=when rule="…" h=… threshold=…bp`.

## Statistic IDs

`when.contrast.mean`, `when.contrast.hit`, `when.probability`,
`when.distribution`, `when.bucket.mean`, `studio.subset.ecdf`, and the corrected
`dist.shape.threshold`. Every card prints formula, substituted inputs, estimator,
n, span, source, observation clock, CLOSED THRU and any unavailable reason.

## Colour roles

Signed estimates and bucket means use `theme::sign` with explicit +/−. Rule and
complement (or selected and rest) histograms use categorical `series(0/1)` with
a printed legend. CI whiskers and zero ticks are muted; q ≤ 0.10 is a neutral
outline; LOW N is a caution corner tick. The canvas wash is chrome accent at 8%
alpha with a muted label; no heat surface is introduced.

## Cuts and limitations

Cut (per the spec's cut order): bucket whiskers, and extra probability-tile
display controls (the tile shows the selected horizon only). Kept: rule evidence,
fixed family, causal edges, masks, subset comparison, bootstrap.
`selected_span` is not part of the When report key — the When report does not
depend on the selection; subset keys include it. Daily When uses a single clock
(no day-of-week adjustment). The full-sample contrasts are descriptive and may
change on recomputation; repeated rule searches across runs are outside the
fixed family. No `prefs.rs` keyboard action was added (none was specified).
