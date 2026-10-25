# Checklist and trade journal

`JOURNAL` (aliases `JRNL`, `CHECKLIST`, `TRADELOG`; `AAPL JOURNAL` filters to a name;
`AAPL JOURNAL 2026-10-24` also prefills the chart date) opens a **local trade journal**.
It is not the brokerage `PORT` blotter and not the PM Desk planner. `DESK` can hand a
name, last bar date and planned dollar risk into a journal draft. Step 11 export /
backup is not included.

Entries persist in `trade_journal.json` under the terminal data directory.

## Save a setup

A setup stores:

| Field | Meaning |
|---|---|
| Symbol | Market key (`AAPL US Equity`) |
| Chart date | The session you reviewed (`YYYY-MM-DD`) |
| Reasoning | Why the date, what has to hold, what invalidates |
| Planned $ risk | Initial risk; this is **1R** for the later outcome |
| Checklist | Copied from the default pre-trade list (or your template) |
| Chart provenance | Tagged when the date came from a sample chart |

Default checklist: daily trend, weekly trend, volume, stop defined, size from planned
$ risk, not chasing, news / catalyst reviewed.

`AAPL NOTE 2026-10-24 wait for the 20-day` attaches a free-form note to that historical
date. The chart context menu can save a setup or a note on the hovered (or last) bar.
Cyan **J** pins mark dates that already have a setup or a note.

## Record the outcome

Pick an open setup and enter **dollar P&L** or an **R-multiple**. The other is derived
from planned risk (`R = $ / planned $ risk`). Entering both only succeeds when they
agree. Outcomes are never read from quotes or bars.

## Sample-data isolation

Journal rows are user-authored. `TR_PROVIDER=sample` and unverified history **never
invent** a dollar result or an R-multiple. A setup saved while looking at sample bars
is labelled SAMPLE; you still type the outcome. PM Desk still refuses sample rows, so
**Journal** from the desk only appears for verified names.

## Commands

| Command | Action |
|---|---|
| `JOURNAL` / `JRNL` / `CHECKLIST` | Open the journal |
| `AAPL JOURNAL` | Journal filtered to AAPL, draft prefilled |
| `AAPL JOURNAL 2026-10-24` | Also set the chart date |
| `NOTE` / `AAPL NOTE 2026-10-24 text` | Attach a note to that date |
| `NEW JOURNAL` / `SPLIT JOURNAL` | Add a journal pane |

Layouts remember the filter, not the entries. F11 opens the journal.
