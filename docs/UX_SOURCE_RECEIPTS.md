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
`PreparedWhen.source_receipts`). They are display/identity metadata and are not part of
any report cache key (the legacy `source` string, which contains the same facts, still is).
Older saved reports deserialize with empty receipts (`#[serde(default)]`).

## `widgets::source_chip`

One muted line per pane: `Yahoo · thru Nov 20 · 24,841 bars` (+ ` · +N series` for
multi-series analyses; the count is the **target series'** own bars, not the aligned
regression `n`). The year appears only when the closed-thru year differs from the
observation year. Hover shows every series' full chain including failed attempts; the
Studio Inspector has a "Source chain" section. Without a structured receipt (older
panes) the chip shows a one-line truncation of the legacy text with the full text on
hover. Used by Regression Lab, Studio header, When, Relate panel, Attribution and regimes.

Provider identity comparisons use `provider_id` only (`providers_differ`). The Relate
cross-source notice appears only when both sides have a receipt and the ids differ;
different bar counts or dates from one provider are coverage, not a source change.

## Account mode

The research toolbar no longer shows DEMO/LIVE: paper trading is an execution setting, not
a data mode. Portfolio and order-entry panes show **Paper trading** or **LIVE**; real-money
mode keeps a thin red rule along the toolbar. Routing, live-account confirmation,
`set_account_mode` and the `AccountMode::Demo` default are unchanged.

## Whats-News footer

Offline Whats-News is an optional service: the footer reads a muted grey `Whats-News
offline` (endpoint and start-service guidance on hover; click still opens Settings ›
Whats-News). It turns amber only when the focused pane's own result failed because it
needed Whats-News.
