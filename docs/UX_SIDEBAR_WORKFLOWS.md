# UX slice 3 — consistent sidebar workflows and visual language

Stacked on slice 1 (`source_chip`, receipts) and slice 2 (`fmt_num`, `numeric_table`, legend allocator).

## Sidebar

* Destinations are one enum, `app/nav.rs::NavDest` (label, icon, command hint, the `Function` a click runs, and
  `active(focused_pane, settings_tab)`). Selection is derived from the focused pane's kind, Studio lens and Quick
  flag, so each row is lit for exactly one screen: Studio (any lens but When) / Quick / When are separate rows,
  Lab / Residual / Attribution follow their panes, Backtest vs Intraday BT follow `intraday`, Price history is
  lit only for the history flavour of `Research`, Settings is not lit on the Brokerage tab (that row is).
* Sections: Watchlist · Research · Sessions · Markets · Trading (PM Desk, Journal, Portfolio…) · Congress ·
  Tracked · Layouts · More.
* Settings no longer carries the stray `PDF` mnemonic. "Cash levels" is labelled **Add cash levels to chart**
  (it is a chart action and is never "active").
* The remove-factor `✕` glyph (missing in some UI fonts, drew as a square) is a painted `Icon::Close` and leads its
  row so it cannot wrap away from the factor it removes.
* Watchlist provenance is stated once ("Real · 12 quotes"); a row repeats it only when it differs from the
  list's majority. Rows without a quote make no provenance claim.

## Shared helpers (`widgets/analysis.rs`, `theme.rs`)

| Helper | Purpose |
|---|---|
| `pane_header(title, context, source, actions)` | one header row (24 pt) + optional one-line `source_chip` |
| `header_button` | control-height action; blue only when it is the active choice |
| `analysis_state(state, body)` | `Loading · NeedsInput · Empty · Unavailable · Stale · Failed · Ready` |
| `run_state(...)` | the standard ladder for a single-result pane (loading / failed / unavailable / empty / ready) |
| `needs_ticker` / `ticker_picker` | neutral "Choose a ticker" card: text field, **Use ticker**, up to six watchlist shortcuts |
| `methodology` | collapsed "Method" disclosure; explanation only on demand |
| `theme::SP_*`, `CONTROL_H`, `STATE_CARD_H` | one spacing scale, one control height, a stable state-card height |

State looks (label + tone + border, all pairwise distinct): Loading — blue-info `LOADING`; NeedsInput — accent
`CHOOSE`; Empty — grey `NO RESULTS`; Unavailable — grey dashed `UNAVAILABLE` (refused / unverified / unsupported
errors, detected by `error_is_unavailability`); Stale — amber `PREVIOUS SNAPSHOT`; Failed — alert `FAILED`.
Red/green are never used for state, only for direction.

Geometry is stable: a full card is at least `STATE_CARD_H` (132 pt); when a previous result is still on screen
(`retaining`), the state is a fixed 20 pt banner ("Updating · showing previous snapshot", or "Previous snapshot ·
refresh failed: …") and the old numbers stay put. No panes ever draw sample data to fill a card.

## Setup states

Session compare, Volume pace, Gap fill, Session extreme and Lookalikes open without a focused ticker into the
neutral chooser. `open_*` and `request()` no longer store a command-style error for an empty ticker (empty ticker
is needs-input, not failure); sample-display mode is a grey chip, not red text. Their explanatory paragraphs are
now a collapsed **Method** line, and their loading / error / empty rendering goes through `run_state`.

Fix pass: the chooser types into a non-persisted `ticker_draft`; the pane's real `symbol` (and the request) is
only set when the chooser returns a ticker, so typing never auto-requests half a ticker. `request()` keeps the
previous result (reload shows the 20 pt "Updating · showing previous snapshot" banner and the old chart); a failed
refresh keeps it too, labelled; a refused/unverified result still clears it. A typo in bar size / sessions / date is
stored in `input_error` and drawn as a neutral "Check the inputs" card (never FAILED, no request sent). The four
intraday panes show their date · bars · provenance · source as the slice-1 header chip (legacy text, truncated, full
on hover — these results carry no structured receipt, so none is invented).

Latest Trades distinguishes `FeedEmpty::NoDisclosures` ("No disclosures yet", **Refresh**) from
`FeedEmpty::NoMatches` ("No matches for these filters", **Clear filters**).

## Studio hub

`StudioPane::show` uses `pane_header` (title, `SYMBOL · tf · lens`, source chip; actions Copy command / About /
Inspector). "Build" is now a row of direct route actions (Regression Lab, Residual tickers, Attribution —
each issues the `STUDIO …` command, hover explains) instead of paragraphs; About is one line.

## Not in this slice

`pane_header` is adopted in the panes touched here and the Studio hub only. Other panes (Session path, Breadth,
Trend Wall, Rotation, Session Clock, Relative path, Session brief, PM Desk, Backtest) keep their existing headers.
