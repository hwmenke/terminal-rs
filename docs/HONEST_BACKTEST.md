# Honest Backtest: BT and IBT

`AAPL BT ADTREND` and `AAPL IBT OR 15m` open **Honest** by default. Every
specified variant and its automatic benchmark is reported; there is no optimizer,
winning strategy selection, or portfolio assembled from fold winners. The older
chronological **Holdout tables** remain available. **RUN** snapshots controls;
editing controls marks them dirty while accepted evidence keeps its own settings.

## Reading the evidence

- **Equity** is net capital from ordered test observations only, starting at 1.
  Cyan is the selected variant; the thin dashed line is the benchmark. Breaks
  mark fold boundaries and missing sessions. The dashed benchmark is shown when
  fold schedules match; longer-warm-up variants can require a separate benchmark
  inspection, which is labelled. Full OOS count/dates are separate
  from the common-family count/dates used in the headline statistics.
- **Fold ribbon** shares the equity time axis: cyan wash is candidate training,
  amber hatching is removed training, signed fill is test, and neutral hatching
  is embargo. Hover for exact candidate, retained, removed and test dates.
  Zero purging is reported as zero. The final nominal embargo can extend past
  available history; its index remains in the export.
- **DSR gauge** is a probability, coloured cyan. Marks are .50, .95 and .99,
  without an approval verdict. Fewer than 30 common sessions is labelled
  **LOW N** and hatched. Unavailable confidence has a reason and no gauge fill.
  **DSR is the probability the true Sharpe beats SR\***, the best Sharpe you'd
  expect from `N_eff` lucky zero-skill variants; it is *not* the probability
  that the true Sharpe exceeds zero. **PSR** is that undeflated probability
  (true Sharpe > 0). DSR belongs to the selected variant, never an implicit
  benchmark or a winner chosen using OOS results. `N_raw` is the durable count
  of distinct experiments; `N_eff` is the common-date correlation cluster count.
  Missing estimates are grey em dashes with a reason. PSR/DSR hover cards
  substitute the selected trial's actual session Sharpe, benchmark, n, skew,
  Pearson kurtosis and probability variance into the formula; the DSR card also
  shows SR\* with its inputs (`N_eff`, cross-trial Sharpe variance `V[SR_k]`, γ).
- **Sharpe bar** puts annualised raw SR and annualised SR\* on one signed axis.
  Their difference is **SR − SR\***; DSR itself is not a reduced Sharpe estimate.
- **Scatter** uses annualised IS and OOS SR on the same numeric scale with zero
  axes and an equality diagonal. Missing statistics appear in an unavailable
  row, never as a point at zero. Click a point, ribbon row or fold row to select
  the same fold and outline its equity segment. Hover for the math and inputs.

In a focused BT/IBT pane, **PageUp / PageDown / Home / End** select the previous,
next, first or last fold. Text entry, command input, palette, hand-off menus,
binding capture and customized key bindings take priority. The same dispatcher
is used in popped-out panes. Controls and selection persist with serde defaults
for older saved layouts. Reports and trial arrays are not layout settings.

## Return and estimator conventions

Returns are **net simple capital returns**, `r_t = E_t / E_(t-1) - 1`, including
entry, reversal and liquidation costs. A separately funded fold starts at
`E_(t-1)=1`, retaining its first entry cost. Flat sessions remain actual zeros.
BT has one daily cash-session observation; IBT aggregates all intraday fills into
one complete-session return. Trade returns and individual intraday bars are not
independent Sharpe observations. Risk-free return is explicitly **0%**: there is
no verified risk-free series in this slice. Both engines display `SR_ann=√252 SR`.
Probabilities, clustering and trial dispersion always use session units.

For n finite returns, the estimators are:

```
mean = Σr / n
m_k = Σ(r - mean)^k / n
s² = Σ(r - mean)² / (n - 1)
SR = mean / s
skew = m_3 / m_2^(3/2)
kurtosis = m_4 / m_2²                    # Pearson, normal value 3
V = 1 - skew × SR + (kurtosis - 1) × SR² / 4
PSR(S*) = Φ((SR - S*) × √(n - 1) / √V)
```

The primary PSR benchmark is `S*=0`. Centered, translated and scaled accumulation
avoids raw-power cancellation. Sharpe needs at least two observations and positive
variance. PSR/DSR need at least four observations and finite positive V. Invalid
values are never filtered into a shorter apparently valid series, and no epsilon
variance or dispersion floor is invented. Ruin stops the affected simulation;
subsequent capital is not restarted.

The PSR and expected-maximum construction follow
[Bailey and López de Prado (2014)](https://davidhbailey.com/dhbpapers/deflated-sharpe.pdf).
They are asymptotic moment estimates, not a guarantee of independent returns or
future profitability. Session aggregation and purging do not correct every kind
of serial dependence. Reusing historical tests does not make them unseen again.

## Durable experiments and common comparison dates

The existing bars SQLite database gains additive `backtest_trial_families` and
`backtest_trials` tables. An experiment includes the exact strategy/setup,
parameters, costs, shorting rule and evaluation protocol. Both the presented
walk-forward experiment and legacy holdout experiment are recorded, including
automatic benchmarks. Full canonical descriptors are keys, so hash collisions
cannot merge experiments. Execution/formula method is `honest-bt-v1`.

A family is scoped to canonical security, BT/IBT mode, native timeframe, stable
source/price-basis identity and simulation-method version. It is not scoped by
pane, dates, current strategy text, bar digest, epoch or vault/live counts.
Local Yahoo archive and distinct bridge identities remain separate because the
archive interface does not supply proof that different upstream bases match.
Corrections and new history recompute experiments without erasing search history.

Upsert and snapshot are atomic. A new descriptor increments the family revision;
identical reruns do not. First-seen time, original input digest and dates, and
attempt/computation/failure status survive restarts and report-cache eviction.
There is no clear-trials control. A failure remains registered and is replayed
on current admitted history. Missing/disabled/read-only or memory-only register
means **DSR unavailable — trial history unavailable**, never a fictitious N=1.
The report also includes current replay statuses, independently of the initial
register snapshot. No SQLite lock is held during simulation or clustering.

All tracked experiments replay on current admitted history. DSR, its associated
SR/PSR and trial dispersion use the **intersection of actual evaluated session
dates** across all experiments. Missing dates are never filled with zero. Fewer
than four common observations makes DSR unavailable while full selected OOS
results can remain. The UI shows tracking start, revision, declared trials,
eligible stochastic trials, excluded all-zero series and cluster sizes. Earlier
experiments and external research are unknown; this is not lifetime search history.

## Effective trials and SR* (expected best lucky Sharpe)

Canonical-ID order makes clustering deterministic. Starting with singleton
clusters, merge the pair with the largest minimum cross-cluster Pearson
correlation, only if that minimum is at least **+0.90**. Ties follow sorted IDs.
Stop when no pair qualifies. `N_eff` is the remaining cluster count.
Complete linkage prevents correlation chains; negative correlation does not
collapse opposite opportunities. This fixed, versioned rule is an approximation
to effective search breadth, not an identified independent-hypothesis count.

All-zero series stay registered but are excluded from stochastic clustering.
Nonzero constant series, invalid numbers or failed replay make the family
incomplete and suppress DSR. For M eligible trial Sharpes, use the **sample**
standard deviation `s_SR` across the common panel. The null has zero mean;
the observed mean trial Sharpe is not added to it.

```
a_1 = 0
a_2 = 1 / √π
for N >= 3:
  a_N = (1 - γ) Φ⁻¹(1 - 1/N) + γ Φ⁻¹(1 - 1/(N e))
  γ = 0.5772156649015329
V[SR_k] = s_SR²
SR* = √V[SR_k] × a_N        # expected best Sharpe of N_eff zero-skill trials
DSR = PSR(SR*)               # P(true SR > SR*), not P(true SR > 0)
SR - SR*                     # shown on the Sharpe bar
```

SR\* is the Sharpe the luckiest of `N_eff` independent zero-skill variants
would be expected to show, given the observed cross-trial Sharpe variance. DSR
asks whether the selected variant's Sharpe clears that bar; PSR (`SR*=0`) is
the undeflated probability that the true Sharpe exceeds zero.

The N=2 branch is the exact maximum expectation of two independent standard
normals. Larger families use the cited approximation. With one effective
opportunity SR\* is zero and DSR equals PSR. Equal observed Sharpes
can produce zero empirical dispersion; this is disclosed rather than repaired.

Replay is capped at **64 distinct experiments** and **60,000,000 bar-pass work
units**, conservatively estimated as `native_bars × tracked_trials × 14` (up to
six training/test prefix pairs plus holdout work). Current selected WF work has
its own `native_bars × current_experiments × (folds+1)` check. Exceeding a family
limit discloses the actual count and suppresses DSR without truncating history.
This budget measures replay passes, not CPU instructions or a latency guarantee.

## Expanding folds, holding labels and embargo

Defaults are three folds and one post-test embargo session. Supported folds are
2–6; embargo is 0–252 sessions. The count is never silently reduced.
BT starts after `L=max(60, warmup+30)` sessions with at least 20 test sessions
per fold. Explicit scoreable-fill warm-ups are: hold 1, SMA slow length, RSI
period+1, Adaptive medium `max(21, ATR length)` (short/medium causal ER, slope
and ATR; its medium signal does not depend on the long regime).
IBT starts after 20 sessions and requires five test sessions per fold.

For U units, K folds and E embargo sessions, require
`R=U-L-(K-1)E >= K×minimum_test`. Divide R as evenly as possible, giving the
first `R mod K` folds one extra session. Half-open spans are
`a_0=L`, `b_f=a_f+q_f`, `a_(f+1)=b_f+E`. Candidate training is
`[warmup,a_f)`, test `[a_f,b_f)`, embargo `[b_f,b_f+E)` clipped to loaded history.

Training is simulated on its causal prefix without forced liquidation. Every
holding episode records decision, entry and exit timestamps. Its information
interval `[decision,exit]` overlaps a test or embargo `[A,B]` when
`decision<=B && exit>=A`. An unresolved exit is right-censored at the cutoff;
no future test exit is inspected. All dependent accounting returns are removed.
A reversal-day return containing costs from two episodes is removed in full
when either is excluded. Persistent daily holdings can leave no usable IS;
valid OOS equity remains, with an explicit unresolved-label reason for IS.
Purge counts include only labels touching the scored training candidate, not
unscored warm-up labels. Insufficient IS caused by known overlapping exits is
reported as too few returns; the unresolved-label reason requires an unknown exit.

IBT labels complete within their sessions. The first session-boundary fold
normally has zero purge. Later folds still exclude labels in prior embargo
sessions. These are genuine embargo removals, not invented label overlap at
the next test boundary.

There are no future training observations in a strictly forward fold. The
post-test embargo is omitted from stitched OOS and excluded from later training
scoring, along with any holding label touching it. Those past prices may still
supply causal indicator warm-up. Earlier tests may become later training history.
The configured embargo is a post-test gap, not a cap on label duration. IBT
holdings resolve within a session; daily holdings can have an unbounded horizon.
Purging uses each full holding interval regardless of the gap setting, and
removes unresolved intervals conservatively instead of assigning a shorter horizon.
See the [purged-validation label-overlap principle](https://www.quantresearch.org/Innovations.htm).

## Boundary execution, stitching and degradation

Each fixed variant starts each test flat with unit capital. BT may use the prior
closed signal to fill at the first test open; any remaining position is liquidated
at the scheduled final test close with exit costs. No overnight P&L or position
crosses a test boundary. IBT retains its next-regular-open and session-close
flatten rules. The older daily holdout table retains its mark-open-at-end rule,
which differs from WF liquidation and is included as its own experiment.

`G_f,t = G_(previous fold end) × e_f,t`: equivalently, compound each ordered test
return once. Missing/embargo periods have no observations. The worker supplies
an equity min/max envelope and plot bounds; paint does only coordinate transforms,
hit testing and formatting. No hashing, simulation, moments, clustering or DB work
runs in paint. Identical admitted input/settings/family keys reuse an immutable
`Arc` report and concurrent identical work is deduplicated. Current freshness and
vault-only receipts travel outside that science cache. Family revisions notify
matching panes and reject superseded confidence without an upsert refresh loop.

For positive, non-negligible IS SR, retention is `Q=SR_OOS/SR_IS` and degradation
is `1-Q`; negative OOS can yield negative retention. The aggregate uses the same
paired folds weighted by test counts: `Q=Σ(n_test SR_OOS)/Σ(n_test SR_IS)`.
An invalid/nonpositive paired denominator suppresses the aggregate. Repeated
expanding IS observations are not pooled as independent samples. This comparison
is distinct from Sharpe of the stitched OOS tape.

## Admission, export and implementation limits

Both engines reject explicit sample mode before scoring; a single sample or
unverified bar taints a request even if it lies in the pending tail. Mixed verified
cache/live provenance requires the vault stitch witness before normalization.
Daily archives use the captured request clock and the archive's close-plus-15-minute
grace. Intraday uses only the successful observation clock; a failed fetch does
not advance it. Vault-only history conservatively keeps its final bar pending.

IBT requires every expected regular bucket, including a closed final bucket,
and reports partial, holed, ambiguous and invalid-price exclusions separately.
DST, holidays and early closes use existing New York/calendar helpers. No fixed
390-bar assumption is made. Missing VWAP volume refuses that replay.
BT uses a deliberately stricter missing-session policy than the original plan:
**the entire request is refused** for missing, duplicate, invalid or non-session
daily observations. This preserves causality without reconstructing an exit on
a bar preceding an unexpected hole. Holiday/weekend gaps are allowed by the
exchange calendar. No holding return crosses unobserved daily history.

`EXPORT BACKTEST` includes the optional `honesty` report: estimator inputs,
availability reasons, comparison dates, folds, retained/removed spans, embargo
bounds, replay statuses, trial descriptors/scope/revision and full OOS session
returns plus the compact equity envelope. Older exports deserialize without this
field. The export remains paper research and does not submit an order.

Extra zoom controls, pinned math cards, cluster expansion and dense fold columns
are omitted; hover math and primary evidence remain. CSCV/PBO and the ATR stop
cloud (roadmap row 12) are deferred. No trade journal, alert or trade-plan behavior
is added. No new dependency is introduced.

Verified-history cold/cache/paint latency has **not been measured** in this
workspace: the standard local data directory has no daily archive or Bar Vault
database. Headless synthetic fixtures validate behavior and known answers only;
they are not production performance measurements. Do not infer a speed target
from them. The GUI and long-running servers were not launched.

## Changed files and validation

The slice adds `crates/data/src/backtest_honesty.rs`,
`crates/data/src/backtest_trials.rs`, `app/src/app/backtest/honesty.rs` and this
methodology document. Integration changes are in:

- `crates/data/src/{backtest,intraday_backtest,cache,lib,studio_dist}.rs`:
  simulation tapes, admission, additive register setup and shared quantile helper.
- `app/src/worker.rs`: blocking admission/replay, durable snapshots, revision
  notifications, science caching and immutable reports with current receipts.
- `app/src/app/backtest.rs`, `app/src/app.rs`, `app/src/layouts.rs` and
  `app/src/app/workspace.rs`: controls, response checks, persisted defaults,
  local/pop-out keys, and a test-only unique fixture directory for repeatable runs.
- `app/src/command.rs`, `app/src/app/{palette_view,hotkeys_view}.rs`: descriptions
  and explicit ticker/`<GO>` regression coverage.
- `README.md`, `docs/BACKTEST.md`, `docs/INTRADAY_BACKTEST.md`,
  `docs/REAL_DATA.md`: discovery and real-data guarantees.

Existing portable serde export functions carry the new optional report field
without changes to `portable.rs`; daily and intraday backward-deserialization
and round-trip tests verify that path.

Checks use `CARGO_TARGET_DIR=/workspace/tr-auction-target`:

```
cargo test --workspace --offline
cargo build --release --offline
cargo clippy --workspace --all-targets --offline
```

Touched Rust files pass `rustfmt --check` and `git diff --check` passes. Whole
workspace `cargo fmt --all -- --check` still reports pre-existing formatting in
untouched files. Clippy completes with no diagnostics in the new modules and
no warnings on changed lines; existing warnings elsewhere remain. In particular,
strict `-D warnings` is blocked by existing `needless_lifetimes` in core provenance
and `bool_comparison` in Adaptive Trend tests, before reaching this slice.
No GUI, server, external-data test, dependency addition or git commit is part of
this change.

Final workspace result: **1,447 passed, 0 failed, 8 ignored** (including doc-test
targets). Ignored tests retain their existing opt-in status.
