# Full, trustworthy price history with safe refreshes

Open **Markets → Price history** or enter `HIST`. The panel adds named bundled lists or individual
Yahoo tickers to terminal-rs's own research archive, shows latest dates and freshness, and manages
background downloads. `NEW HIST` adds another panel. It persists in saved layouts.

| Command | Behavior |
| --- | --- |
| `HIST ADD ALL` | Register and download all 582 unique bundled symbols |
| `HIST ADD US` | Broad static US equity research catalogue |
| `HIST ADD NASDAQ` | Static Nasdaq research basket (100 names) |
| `HIST ADD ETFS` | Major ETFs and US equity indices |
| `HIST ADD AAPL,MSFT BRK.B` | Add named or custom Yahoo symbols; normalize share classes |
| `HIST ADD WHATS-NEWS` | Read the connected universe and download those symbols from Yahoo |
| `HIST REFRESH` | Incrementally refresh every registered local symbol |
| `HIST CANCEL` | Cancel queued/in-flight requests and retry waits; retain completed symbols |
| `HIST RESUME` | Resume unfinished symbols, including after an application restart |
| `HIST RETRY` | Retry only failed symbols |

Lists can be combined with symbols in one ADD command. The catalogue is bundled in TSV files with
security names, deduplicated by Yahoo symbol. It is a curated static research universe, **not live
index membership**. It includes historical constituents; unavailable names report provider failures.
Registration is metadata only: no prices appear until real Yahoo data arrives.

## Storage and refresh safety

Daily rows and queue checkpoints live in `history.sqlite3` under the terminal data directory
(`TR_DATA_DIR`, otherwise the platform's app data folder). This is separate from `bars.sqlite3`,
the display cache, and from Whats-News. No code writes to the Whats-News project or database.

The archive calls Yahoo directly through the existing Yahoo client. It never calls the display
provider's sample fallback, even with `TR_PROVIDER=sample`. Full downloads request daily history
from 1900 through the last completed US session, without clipping to a chart bar count. OHLC prices
are scaled consistently by Yahoo's adjusted-close/close factor. Missing adjustment metadata,
invalid OHLCV, duplicate/out-of-range dates, sample, cached or unverified download provenance are
rejected and the symbol is marked failed. Artificial prices exist only in isolated tests.

Defaults: at most two symbols in flight, at least one second between request starts globally,
and at most four attempts for transient network/rate-limit errors, with 2/4/8-second backoff.
Retries and full refetches also pass through the request limiter. Not-found, no-data and rejected
responses fail without retrying. The panel reports plain-English reasons and offers **Retry failed**.

A refresh starts seven calendar days before the latest stored daily date. Any overlapping OHLC
change exceeding relative tolerance 0.0001 (0.01%) triggers a full refetch, as does an incremental
response with no matching overlap. A full replacement must retain all previously archived dates.
The complete response is validated before a transaction changes any stored prices. Errors,
truncated replacements and cancellation preserve the prior series; successful price writes and
completion checkpoints commit together. Pending work remains resumable; running checkpoints are
recovered as pending after restart. One job runs at a time. A new ADD/REFRESH/RETRY replaces the
previous queue; use RESUME to continue it instead.

All network work runs on the background runtime. SQLite work runs on blocking worker tasks, and
the UI receives snapshots rather than opening databases. Progress displays **X of N done**, active
symbols (up to two), per-symbol failures, and disjoint fresh/stale/failed/missing counts. Failed
symbols retain their previous latest date and stale flag when old prices remain available.

## Freshness and research

Freshness uses the last **completed** NYSE session, with New York daylight-saving rules, weekends,
regular US exchange holidays, known historical ad hoc closures and early closes. A 15-minute buffer
after the session close allows Yahoo's daily candle to settle. During trading hours, yesterday's
completed session is expected. On holidays/weekends the expectation stays on the prior session.
Future unannounced exchange closures require a calendar update.

`UNIV` merges the local archive and connected Whats-News symbols. Local stored prices take priority
for duplicate symbols; missing local history can still use verified Whats-News daily bars. Local
history keeps working when Whats-News is disabled or disconnected. `SCAN` uses local and connected
daily histories together when local symbols exist, deduplicating symbols and preferring local bars.
Without local symbols the existing Whats-News scanner remains available. Local RSI/composite-score
parity remains outside this item; RSI-only scans still require the Whats-News service.

Research tables visibly mark and color stale rows. Clicking a result opens an archive chart, which
bypasses display samples/live feeds and supports `1D`, `1W`, and `1M`. Weekly and monthly candles
always aggregate **daily rows** (Whats-News weekly rows are never read). Scans still use every
stored daily row. Chart panes load a bounded page and page older history; see
[Chart review](CHART_REVIEW.md). Weeks begin Monday, months begin on the first, and partial current
weeks/months contain only completed daily sessions.

## WHATS-NEWS WEEKLY BUG:

**The same incremental weekly-candle bug exists upstream.** Read-only inspection found:

- `~/wn-fix/data_fetcher.py:94` reads the latest stored daily date; line 100 downloads only the overlap.
- `~/wn-fix/data_fetcher.py:113` cleans that download, and **line 117 resamples only that slice**.
- `~/wn-fix/data_fetcher.py:127` upserts the partial weekly result. `database.py:347` implements
  `ON CONFLICT ... DO UPDATE`, replacing the previous weekly OHLCV with it. An overlap starting
  midweek therefore loses the earlier days' open/high/low/volume for that weekly row. Older weeks
  outside the download survive, but intersecting weeks can be corrupted, including a partial
  current week.
- `~/wn-fix/scripts/bulk_archive.py:112` calls this incremental path during bulk refresh.

The required upstream fix is to validate/compare the overlapping daily prices first (full refetch
if adjustments changed), upsert daily rows, reread the **entire stored daily history without the
default 1,000-row limit** (`database.py:377`), and regenerate weekly/monthly rows from that complete
series. Commit daily and derived replacements atomically, remove obsolete derived rows, and retain
partial current periods consistently. `fetch_full_history` already resamples its complete download;
it does not solve the incremental path's boundary overwrite. **No upstream files were modified.**

## Offline validation

Archive tests cover incremental requests, overlapping corrections/adjustments, full replacement
failure/truncation, weekly/monthly history and partial weeks, sample rejection, error categories,
retry exhaustion/backoff, request spacing/concurrency, freshness (holidays, DST and early closes),
and cancel/resume across a reopened SQLite database. Yahoo parsing tests use the recorded provider
fixture. Worker tests prove local research works without Whats-News or the display provider, and
that corrupt Whats-News weekly rows are ignored while its database remains unchanged.

Run `cargo test --offline -p terminal-rs` and `cargo test --offline -p tr-data --lib history::`.
Existing HTTP fixture tests require loopback sockets, which this sandbox does not permit. Live
Yahoo requests were deliberately not run. Strict workspace Clippy currently encounters pre-existing
warnings in `crates/core/src/provenance.rs` and `app/src/prefs.rs`; ordinary Clippy can still review
the changed crates without treating those baseline warnings as errors.

Final checks: 113 app tests, 74 core tests and 59 data tests passed. The data command was
`cargo test --offline -p tr-data --lib -- --skip http --skip connection_data_service --skip connection_timeout`
(eight existing HTTP tests excluded). Formatting and whitespace checks passed. Clippy passed with
only the two existing lint categories allowed:
`cargo clippy --offline -p tr-data -p terminal-rs --all-targets -- -D warnings -A clippy::needless_lifetimes -A clippy::field_reassign_with_default`.
