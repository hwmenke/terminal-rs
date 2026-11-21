# Studio shell and named lenses

`AAPL STUDIO 5m lens=dist` opens the original native chart above Distribution.
The rail’s Quick canvas control switches to the five-mode scalar canvas; Native
chart retains all original indicator, candle/log and study controls. `STUDIO`
uses the focused security; daily and Distribution are defaults. Supported lens
names are `dist`, `relate`, `when`, `season`, `backtest`:

```
STUDIO lens=relate benchmark=QQQ
STUDIO lens=season
STUDIO lens=when
STUDIO lens=backtest
AAPL STUDIO 5m lens=when rule="RSI(14)<30"
AAPL WHEN 5m RSI(14)<30
```

Legacy DIST/RDIST (including Shape), CORR/RBETA, SEASON/SSN, STATS/COND and
BACKTEST/IBT commands keep their standalone panes and numerical conventions.
QUICK opens the full [time canvas](STUDIO_CANVAS.md), with five comparison modes,
an exact base, strip, dated lanes and a release-committed brush. `STUDIO LAB`,
`STUDIO RESID` and `STUDIO ATTR` remain named unavailable routes (the bare words are
listed tickers and load them). Build explains that derived operations are
unavailable; About describes the composition and snapshot limits.

Alt+Left / Alt+Right cycles the supported lenses in the focused Studio; the rail
keeps all five routes visible with explicit reasons for unsupported intervals. These local
bindings take precedence over native chart watchlist navigation. Escape cancels
an active brush first, then closes the inspector. Ctrl+Z restores prior Studio inputs (20 entries); focused text
editors own their own undo. The actions live in the preferences registry and
appear in generated help. Existing user bindings retain precedence when defaults
are migrated. Ordinary chart navigation and drawing commands remain available
through the embedded native chart.

At desktop widths the receipt header reserves 44 points and the rail 32. The
native canvas starts at 40% of remaining content; drag the divider to resize.
The optional inspector is 320 points. Below 900 points it becomes an overlaid
scrollable drawer; below 600 points canvas and analysis use a vertical scroll
stack. Controls and unavailable reasons wrap at 320 points.

The inspector changes security, interval and benchmark. The existing lens
controls change their original parameters. Saved layouts store view inputs only,
with serde defaults; report arrays, pending work and request tokens are never
serialized into layouts. Split position is session-only. Copy command includes
security, interval, lens and benchmark; it cannot reproduce an old provider
revision or the full parameter history.

Canvas selections use half-open UTC `[start,end)` semantics: `[20,40)` contains
20 and 30 in `[10,20,30,40]`. Enter UTC bounds in Inspector. Switching lenses
preserves that selection and accepted report inputs. The dedicated Brush commits
the same span on release. Compare in Distribution shows a descriptive comparison
of selected versus visible transformed canvas values; it never silently filters
an existing research lens. As-of filtering remains unavailable. Each child retains its
own full accepted receipt. Unknown observation clocks are printed explicitly;
only native feed state can claim genuinely stale data.

## Data and worker lifecycle

Research lenses retain their original loader/estimator. The canvas adds the
worker arithmetic and sourced comparisons described in STUDIO_CANVAS.md. The shell retains pane states and
uses shared renderer/action adapters. DIST remains an owned immutable report;
CORR, SEASON and backtest reports retain their existing Arcs. Typed worker
requests, source admission, full-source validation, closure clocks, caches and
second-stage report keys remain the original implementations. See
[Distribution](STUDIO_DIST.md), [Relate](STUDIO_CORR.md) and
[Season](STUDIO_SEASON.md) for exact methods and data limitations.

Relate (slice 4) adds strict fixed-bar transforms, expanding β, a common-panel matrix and a
frozen-training fair-value overlay with residual ±σ bands on the native chart (`RELATE`,
`STUDIO lens=relate transform=logreturn h=5`; never in Quick-canvas mode). Bare `LAB`,
`RESID` and `ATTR` load their tickers; `STUDIO resid|attr` are unavailable routes.
See [Relate](STUDIO_CORR.md).

The Regression Lab (slice 5) is a standalone pane reached with `STUDIO lab …` (it is not a sixth
lens; `StudioLens::ALL` stays five): up to seven factors with per-factor transform and lag on the
target's strict grid, a pivoted-QR fit with rank and κ guards, four covariances, a purged frozen
holdout and two diagnostic plots. Bare `LAB` is still the LAB ticker. See [Regression Lab](STUDIO_LAB.md).

Slice-5 corrections to slice-4 numbers: the CORR/Relate chronological holdout now purges the last
h−1 training pairs at h > 1 (OOS R² at h = 5 and 20 moves; h = 1 unchanged), fair-value embargo rows
are a third state instead of "in-sample", `Transform::parse` errors no longer say "Relate" for every
caller, and the CORR report cache key no longer includes the wall-clock observation time.
Restored Studio layouts normalise the Relate transform horizon and `corr_horizon` to {1, 5, 20}.

When (slice 3) is a dedicated lens: rule mask shading on the native chart,
fixed-family within-clock HAC/BY evidence at h=1,3,6,12, shared-bin
distributions, session-block probability tiles and prior-only buckets, on
verified daily or native 1m–1h US cash bars. See [When](STUDIO_WHEN.md); the
standalone STATS/COND pane is unchanged and offers Open in When. A committed
selection adds a selected-versus-rest comparison above Distribution. SEASON supports daily/5m/15m/30m/1h US cash
sessions; unsupported intervals retain its refusal. Backtest chooses the existing
daily archive or intraday session-research path from the Studio interval. No
substituted timeframe, provider, price or peer fills missing inputs.

Accepted inactive lenses stay in memory (at most five child states). Switching
back does not request another accepted analysis. Leaving a lens invalidates its
pending responses while retaining accepted reports. Process-wide monotonically
allocated tokens distinguish recreated children and old contexts. Context and
history changes clear dependent child states immediately; second-stage seed/key
gates remain active. The native history, compare, indicators, errors, feed/vault
receipts and news responses route to embedded charts through the same adapters
as ordinary charts.

Sample mode clears every retained child and the native chart's analytical
state before draining queued replies, including invisible lenses. The shell
sends no history or statistical requests while in sample mode. Returning to real
mode loads new accepted inputs; old replies cannot restore cleared reports.

## Scope and validation

Existing lens estimators, return conventions, inference families and thresholds
are unchanged. The canvas adds descriptive level/index/change/prior-Z/volatility
arithmetic and a separate midrank histogram convention. Existing log returns,
HAC/BY, bootstrap and backtest definitions are inherited. Existing heat surfaces keep their keys and MathCards. The Studio canvas hides
the native trade buttons and alert/journal menu sections; standalone charts keep
their existing controls. Overview
cards now also name their stable `dist.overview.*` IDs, observation clock and
CLOSED THRU.

Cuts: animated lens transitions and persisted split position; undo is a bounded
input history, not a separate history per lens. No derived/model/answer features
are implemented. See [PERF.md](PERF.md) for measured gates and final validation.

The existing workspace persistence implementation differs from the plan's broad
worker-only persistence contract: `workspace::save_layout` and `layout_dirty`
still serialize/save the containing workspace synchronously. This slice follows
that existing flow and adds only bounded view inputs; it does not add analytical
serialization, a model store or a fallback I/O path. Moving the whole workspace
save/dirty protocol off-thread remains an inherited concern. The research loads,
validation and statistical stages used by Studio remain worker jobs.
