# Source receipts, account mode and optional-service status (UX cleanup slice 1)

## Structured source receipts

`daily_research::resolve` still tries Local research archive → Whats-News → native
provider and keeps every failed earlier attempt. The accepted source is now a
structured `tr_data::source_receipt::SourceReceipt`:

| field | meaning |
|---|---|
| `provider_id` | stable id used for identity: `archive`, `whats-news`, or the native provider id (`yahoo`) |
| `provider` | short display name (`Yahoo`) |
| `closed_thru`, `bars`, `observed_at` | the series' own last closed bar, bar count and observation clock |
| `attempts` | failed earlier sources — diagnostics only |
| `detail` | the full legacy text (`… · fallback: …`), unchanged, for MathCards, logs and exports |

Receipts travel with the data (`StitchInfo.receipt` → `NamedBars.receipt` →
`LabReport.source_receipts`, `CorrReport.receipts`, `RelateReport.receipts`,
`PreparedWhen.source_receipts`, `dist::Report.source_receipt`, `SeasonReport.source_receipt`,
`StatsReport.source_receipt`, `BacktestReport.source_receipt`, `RotationReport.source_receipts`,
`ResidualReport.source_receipts`, `AttributionReport.source_receipts`). They are
display/identity metadata and are not part of any report cache key (the legacy `source`
string, which contains the same facts, still is). Older saved reports deserialize with
empty receipts (`#[serde(default)]`). The worker attaches the receipt to a copy of a cached
report, so a cache hit still names the provider that served *this* run.

**Labels never drift from receipts.** Multi-series receipts are positional
(`CorrReport.receipts`/`RelateReport.receipts` are `[Option<_>; 2]`) or `(symbol, receipt)`
pairs (Lab, Rotation, Residual, Attribution; failed rows are filtered as pairs). The widget
takes borrowed `(label, receipt)` parts (`widgets::chip_parts*`), so a missing receipt drops
its own label and nothing is relabelled onto another ticker.

## `widgets::source_chip`

One muted line per pane: `Yahoo · thru Nov 20 · 24,841 bars` (+ ` · +N series` for
multi-series analyses; the count is the **target series'** own bars, not the aligned
regression `n`). The year appears only when the closed-thru year differs from the
observation year. Hover shows every series' full chain including failed attempts; the
Studio Inspector has a "Source chain" section. Without a structured receipt (older
panes) the chip shows a one-line truncation of the legacy text with the full text on
hover (built lazily, only while hovered). Without a structured receipt the legacy text is
built only then. Sites (every former text-wall site):

| site | receipt |
|---|---|
| Studio header (all lenses; Inspector "Source chain") | per lens: Relate, When, Dist, Season, Backtest |
| When / Dist / Season lens body | hosted in the Studio: no second chip (window line stays); standalone Dist/Season pane: chip |
| Regression Lab | `(symbol, receipt)` pairs, failed factors excluded |
| Relate panel, Corr header hover | positional `[asset, benchmark]` |
| Statistics (`stats.rs`) | `StatsReport.source_receipt` |
| Rotation (`rotation.rs`) | benchmark + loaded names |
| Backtest (`backtest.rs`, daily) | `BacktestReport.source_receipt` (intraday: legacy truncation) |
| Dist · Regime Fingerprints | `dist::Report.source_receipt` |
| Attribution, Residual tickers | `source_receipts` of the scored report |
| CHECK Trend Wall edge hover | unchanged: hover-only MathCard text, fed by `analysis_bars` (no daily receipt exists) |

Intraday panes have no daily receipt; they keep the one-line truncation with full text on hover.

Provider identity comparisons use `provider_id` only (`providers_differ`). The Relate
cross-source notice appears only when both sides have a receipt and the ids differ;
different bar counts or dates from one provider are coverage, not a source change.

## Account mode

The research toolbar no longer shows DEMO/LIVE: paper trading is an execution setting, not
a data mode. Portfolio and order-entry panes show **Paper trading** or **LIVE**; real-money
mode keeps a thin red rule along the toolbar. Routing, live-account confirmation,
`set_account_mode` and the `AccountMode::Demo` default are unchanged.

## Whats-News footer

The daily resolver returns a typed `DailyUnavailable { needs_whats_news }`; the flag is true
only when the Whats-News stage failed on reachability (`WhatsNewsUnreachable`), never for "no
history for this ticker" (unknown ticker stays grey). The flag reaches panes inside the worker's
error string as `WHATS_NEWS_OFFLINE_NOTE`, tested by `error_needs_whats_news` (one definition,
not free-text matching of "Whats-News"). The footer checks Studio lenses (When, Relate, Dist,
Season, Backtest), Lab, Stats and Rotation. Not-connected states read `Whats-News offline`
(Disconnected) or `Whats-News: Not configured`, in grey. Offline Whats-News is an optional
service: the footer reads a muted grey `Whats-News offline` (endpoint and start-service guidance on hover; click still opens Settings ›
Whats-News). It turns amber only when the focused pane's own result failed because it
needed Whats-News.
