# Export and recovery

`EXPORT`, `BACKUP` and `RESTORE` write and read portable JSON. Every file carries **source**,
**as-of date** and **provenance**. Sample-tagged rows produce a filename containing `SAMPLE` and a
banner. SnapTrade keys and other credentials never enter a portable file.

Files default to `~/Downloads` (`terminal-rs-<kind>[-SAMPLE]-<YYYYMMDD>.json`). If Downloads is
missing they go in `<data dir>/exports`. Settings › Backup uses the same path field as layout
import/export.

## What you can export

| Command | Contents |
|---|---|
| `EXPORT SCAN` | Current SCAN or UNIV snapshot: setup name, source string, last-bar dates, metrics, skipped rows, active filters |
| `EXPORT JOURNAL` | Setups, outcomes, date notes, checklist template |
| `EXPORT HIST` | Local Yahoo research archive: per-symbol source (`yahoo`), session dates, provenance and daily OHLCV |
| `EXPORT STATS` | Statistics / conditional-return snapshot: source, as-of, provenance, baselines, completed N |
| `EXPORT BACKTEST` | Paper backtest: source, as-of, costs, fill rule, holdout dates, completed-trade metrics |
| `EXPORT` | The focused scan / history / journal / stats / backtest pane, or Settings › Backup |
| `BACKUP` | Personal workspace (see below) |

SCAN / UNIV / HIST / JOURNAL panes also have an **Export** button.

Scan JSON is a snapshot for records or sharing. It is not loaded back into SCAN.

## Workspace backup and restore

`BACKUP` (or Settings › Backup › **Back up workspace**) writes:

- `prefs.json` (appearance, hotkeys, panel defaults, Whats-News connection *paths*)
- `settings.json` (watchlists, sidebar, active layout, recent symbols)
- saved layouts and custom themes
- trade journal
- price/setup alerts
- command-palette recents

**Never included:** `secrets/` (SnapTrade), `snaptrade.json`, API keys, tokens, passwords,
`demo-account.json`, `trading-access.json`, `bars.sqlite3`, `history.sqlite3` (use `EXPORT HIST`),
or the Congress database.

`RESTORE <path>` inspects `kind`:

| Kind | Result |
|---|---|
| `workspace` | Replace prefs, settings, layouts, custom themes, journal, alerts and palette recents. Extra local layouts are left in place. `secrets/` is never written. |
| `journal` | Merge setups/notes by id (no overwrite of existing ids) |
| `history` | Import **verified Real** Yahoo bars into the research archive. SAMPLE / unverified files are refused. |
| `scan` | Refused — open the JSON yourself |
| `stats` | Refused — open the JSON yourself |
| `backtest` | Refused — open the JSON yourself |

## Sample-data isolation

A sample chart date on a journal setup, or a sample scan snapshot, labels the whole file:

`WARNING: SAMPLE DATA — labelled sample export; isolate from live research`

Those files are named `terminal-rs-<kind>-SAMPLE-<date>.json`. History restore still refuses
sample bars so they cannot enter the research archive. Statistics and backtest exports
are snapshots and are not loaded back into `STATS` / `BACKTEST`.

## Commands

| Command | Action |
|---|---|
| `EXPORT` | Focused pane, or open Settings › Backup |
| `EXPORT SCAN [path]` | Scan / universe JSON |
| `EXPORT JOURNAL [path]` | Journal JSON |
| `EXPORT HIST [path]` | Archive daily history JSON |
| `EXPORT STATS [path]` | Statistics / conditional-return JSON |
| `EXPORT BACKTEST [path]` | Paper backtest JSON (costs, holdout, execution) |
| `BACKUP [path]` | Workspace backup |
| `RESTORE <path>` | Restore workspace, journal, or history |
