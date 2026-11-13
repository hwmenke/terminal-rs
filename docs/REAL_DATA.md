# Real-data guarantee

Live mode serves provider data or reports unavailability. It never substitutes sample bars,
quotes, chains, books, curves, or ticks. `Guarded<P>` has no sample generator: its admission
checks reject synthetic flags and unverified provenance, including streamed bars. Yield
curves carry provenance through aggregation. Equity order books are Unsupported.

`TR_PROVIDER=sample` is the only application entry point to `SampleProvider`. It retains
synthetic history, chains, books, curves and ticks and shows **SAMPLE · SYNTHETIC**. A failed
live-provider initialization instead constructs `OfflineProvider`, which returns typed
Startup errors, has no stream, and shows **OFFLINE · NO DATA SOURCE**.

## Failures and recovery

Verified SQLite bars can paint immediately. A failed refresh retains those real bars and
marks the chart **STALE**, using the cache fetch time for its age. Sample and unverified
cache rows cannot paint in live mode. Other panes retain their last real value with a
STALE chip. Never-loaded prices remain a dash or show **NO REAL DATA**; an unsupported
book paints no ladder. Missing quotes in a partial response remain missing, never filled.

`HEALTH` (alias `SRC`; use `SRC US Equity` for a security) opens per-feed observations:
source, state, last success, error, retry countdown, counts, and a 60-request outcome rail.
The status strip opens the same pane. Startup Alpaca degradation is recorded even when
Yahoo remains usable. Diagnostics stay in memory and contain no credentials.

`R` in a failed/stale pane retries that pane. In HEALTH it retries the selected feed's
current requests. Clicking an unavailable card also retries. Intraday failures retry after
15 seconds and reuse the existing polling lifecycle; successful feeds resume normal TTLs
or their real stream. Superseded loads and closed panes cannot be retried. Unsupported
feeds require manual retry. Provider failure backoff is 5, 10, 20, 40, 80, then 120 seconds;
a success resets the streak. Countdown repaint runs at one Hz (the app's pre-existing
frame scheduling remains in place).

## Scope and implementation notes

Feed availability is observational, not a network-connectivity probe. A feed-wide success
can coexist with a missing symbol; symbol/pane states preserve that distinction. Bar feed
messages include the load generation so an old response cannot mark a new chart stale.
The board uses one mutex per update. Tests inject failures and payloads without network
access. The Yahoo cooldown test was updated for the renamed wrapper and still verifies
that rate-limit causes survive typed unavailability.

The optional prior-day high/low MARKS extension is deferred (the first phase-one cut).

## TSTACK Edge and ZBAND

`TSTACK EDGE [ON|OFF]` enables a cached regime-outcome card (and TSTACK). The rows are
UP/FLAT/DN; columns are 1/5/20 bars. Outcomes use the state known at entry close and only
completed forward returns. Warm-up states are excluded. The shared `closed_len` rule uses
the feed's last refresh clock for live bars and the current clock for archive bars. A bar
with a successor is closed; the final bar must have ended. Equity daily bars retain the
15-minute post-close grace, and weekly/monthly bars wait until the next session belongs
to a later bucket. Crypto uses UTC bucket ends. `CLOSED THRU` identifies the final included
bar; `FORMING EXCLUDED` marks a dropped tail. Turning TSTACK off also hides Edge; the Edge
layout flag defaults off for older layouts.

The verified research gate runs before any calculation. Edge also explicitly refuses mixed
real/cache inputs, because the existing provenance combiner treats that mix as Cached.
Uniformly verified cached history is permitted. Refusals show the shared NO REAL DATA card
with no numeric study output; hover exposes the precise refusal.

Each cell reports mean bps, a shared-scale 95% CI, positive-return hit-rate and count.
The uncertainty estimate uses `n_eff = n/h`, sample standard deviation, and a normal 1.96
multiplier. Below eight effective observations return summaries are suppressed; below thirty,
LOW N and hollow uncertainty bars remain. Hover includes median, standard deviation,
raw/effective counts, CI, Wilson hit interval, Welch-style baseline comparison and dates.
These are descriptive historical estimates; the CI still uses an overlap heuristic.

Edge Proof tests `D = mean(return | regime) − mean(return over warm completed entries)`.
Its null circularly rotates the entire warm signal block (state and both entry flags
together), with returns fixed. Offsets range from `L` through `W−L`, with
`L = max(20, 2 × largest horizon)`. It uses all admissible offsets up to 999, otherwise a
deterministic sample without replacement seeded by closed count and endpoint timestamps;
the work budget caps `n × offsets × 3` at about 15 million. Below 99 usable offsets it
shows `NULL N/A`. The two-sided rotation p is `(1 + extreme count)/(offset count + 1)`.
The grey band is the 2.5–97.5 percentile of null D, translated by the baseline onto the
raw-mean CI axis. Rotation preserves serial structure away from its seam, but introduces
one artificial junction per rotation; changes over time can also weaken the null.

Benjamini–Hochberg step-up adjustment covers every available cell/flip p in the card.
The header reports the number of tests, `BH 0.10`, and passes. q is descriptive within
this family of historical comparisons, not a guarantee of future performance. A solid
dot/whisker requires q ≤ 0.10, at least thirty effective observations, and at least three
known chronological folds agreeing with the observed D's sign. Four equal-count folds
of warm completed entries purge outcomes reaching the next fold edge; a fold is known
only with at least eight effective selected observations. The stability strip shows fold
signs, with grey for unknown; hover adds p, q, the null interval and fold counts/dates.

The footer summarizes signed five-bar flip returns and their rotation p/q, using signed
mean minus the unconditional five-bar baseline as D. Completed wins get filled ticks and
losses open ticks; unfinished flips stay grey rings. The entry flags are carried directly
from the existing causal adaptive-trend result, including its session-reset behavior.

`ZBAND [n]` (default 20) plots the trailing mean and ±1/±2 sample-sigma bands, with two-tone
fill and a hover z readout. Its rolling update uses no future bars. The worker refuses
unverified inputs. Edge reports and labels are cached by closed count, first/last closed
timestamps, parameters and horizons (scoped to chart generation/timeframe). Live ticks
reuse that report; history paging or a newly closed bar changes the key. Provenance gates
still run before reuse. No study calculation or network request runs on paint.

Timing targets (2 ms compute / 0.1 ms paint and the strip budget) have not been
benchmarked; validation covers correctness and headless painting. Edge reuses the
existing `describe` implementation, including its sorting for distribution statistics.

Phase two has no cuts. The only feature cut from phase one is the optional prior-day MARKS
extension. Extra compilation/test dependencies touched are `yahoo.rs` (Guarded rename),
`lookalike.rs` (shared refusal wording assertion), and `trend_stack.rs` (causal entry flags).

## Files changed

- `README.md`
- `app/src/app.rs`
- `app/src/app/chrome.rs`
- `app/src/app/health_view.rs`
- `app/src/app/hotkeys_view.rs`
- `app/src/app/options_view.rs`
- `app/src/app/palette_view.rs`
- `app/src/app/panels.rs`
- `app/src/app/watchlists.rs`
- `app/src/app/workspace.rs`
- `app/src/chart_view.rs`
- `app/src/command.rs`
- `app/src/layouts.rs`
- `app/src/markets_view.rs`
- `app/src/widgets.rs`
- `app/src/worker.rs`
- `crates/core/src/indicators/registry.rs`
- `crates/core/src/indicators/trend_stack.rs`
- `crates/data/src/edge.rs`
- `crates/data/src/nulls.rs`
- `crates/data/src/health.rs`
- `crates/data/src/lib.rs`
- `crates/data/src/lookalike.rs`
- `crates/data/src/markets.rs`
- `crates/data/src/sample.rs`
- `crates/data/src/stats.rs`
- `crates/data/src/yahoo.rs`
- `docs/REAL_DATA.md`

The [Bar Vault](BARVAULT.md) accumulates verified closed native US cash-session bars in separate source-specific tables. Sample mode bypasses it entirely. Legacy display-cache rows are preserved but never promoted into the vault.
