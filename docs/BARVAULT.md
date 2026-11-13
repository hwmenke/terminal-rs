# Bar Vault

## Purpose

The vault accumulates closed native intraday provider bars in `bars.sqlite3`, separately from the existing mutable display cache. It supplies older history to worker studies and initial chart loads. It is not a tick store, sample source, daily archive, or network service. Nothing interpolates prices or fills holes.

## Admission

Only US cash equities, ETFs and indices that pass the existing US exchange-session guard are eligible. Intervals are 1m, 5m, 15m, 30m and Yahoo 1H. Alpaca 1H is excluded: the repository's provider implementation does not establish that its hours use the required 09:30 grid. Crypto, FX, futures, foreign exchanges, 4H and daily-or-higher bars are excluded.

Admission requires:

1. Live provider mode and `Real` provenance. A batch containing Sample, Missing or Cached tags is rejected as a whole.
2. Valid OHLC and a positive close.
3. An exact regular-session grid open (`regular_bucket(tf, ts) == Some(ts)`). Premarket and extended hours never qualify.
4. The bar is closed according to `closed_len`, using the response-arrival clock, with a further 20-second settling delay. Closure is checked per bar, so a successor cannot accidentally admit a future bar.
5. The nominal interval end is no later than arrival plus 120 seconds. This additional conservative check can delay admission of a shortened final 1H bar until close to its nominal end.
6. A unique timestamp. Identical duplicates deduplicate; conflicting duplicates reject the batch rather than making provider ordering decide a price.

Only native history responses reach ingestion. Chart live ticks and merged display-cache series do not. Extended-hours requests bypass the vault. Explicit sample mode never opens, reads or writes the vault, including maintenance and snapshots. Test and benchmark fixtures are confined to test/bench code.

## Sources

Identity is `(Security::key(), timeframe, actual source)`. Markets reports `yahoo` or `alpaca:<feed>` for the provider that actually returned the response, including Yahoo fallback after Alpaca failure or an empty Alpaca response. Guarded forwards this identity through its verified-data gate. Other providers default to their provider name.

Adding an Alpaca key starts a separate series; historical Yahoo and Alpaca rows are never merged in storage. A refresh that changes sources replaces chart history. Paging refuses a known source change. IEX coverage holes are informational: thin trading need not produce a print in every minute, and IEX volumes differ from consolidated volumes.

## Merge rules

Every ingestion is one immediate SQLite transaction. Admission sorts by timestamp. Reconciliation uses timestamp-ordered overlaps and a median ratio, without hash-map iteration.

- **Agree:** every OHLC difference is within `max(abs(stored price) × 0.0001, 0.0051)`. Keep the stored price and increment confirmations. For example, 100.000 versus 100.004 keeps 100.000.
- **Volume only:** the comparison tolerance is `max(2%, 100 shares)`; within-tolerance observations keep stored volume; outside tolerance, stored volume takes the larger available value, so a later observation cannot reduce it. For example, 1,000 versus 1,200 stores 1,200. Missing volume is stored as -1 and restored as missing.
- **Correction:** isolated price disagreement takes the fresh OHLCV and logs the old and new bar. For example, one revised 100.50 close among 78 previously 100.00 closes is corrected.
- **Rebase:** at least 20 overlapping bars, median fresh/stored close ratio more than 1% from one, and every OHLC ratio within 0.2% of that ratio. Rescale all existing prices once and volumes inversely, replace fresh overlap, increment the adjustment epoch and delete that symbol/timeframe's reserved disk-report rows. A uniform 0.25 ratio changes an older 100 price to 25 and 1,000 shares to 4,000. A reverse split uses the same rule with a ratio greater than one.
- **Quarantine:** more than 25% of at least 20 overlapping bars disagree without a consistent rebase. For example, unrelated 5–15% price revisions quarantine the batch. No fresh prices enter storage or the stitched analysis. Stored bars remain available, with a visible quarantine note and no fresh observation clock.

The log records rejections, corrections, rebases, quarantines, failures, migration, verification, pruning and drops. It retains the newest 5,000 events. The pane shows the latest 50. A successful subsequent overlap clears a prior note; there is no automatic three-quarantine DISPUTED flag yet.

## Splits, dividends and joins

Alpaca requests `adjustment=split`; Yahoo intraday prices are treated as split-adjusted rather than dividend-adjusted, consistent with the existing provider path. The vault adds no dividend adjustments. Uniform overlap rebases handle splits and reverse splits.

A batch with **no overlap** cannot prove adjustment continuity. It is withheld from the existing stored series and marked `UNVERIFIED JOIN`; the fresh-only result is served, excluding the older vault prefix. The proposed daily-archive join check is deferred: the daily archive uses adjusted prices, so a tolerance alone does not prove the intraday adjustment basis. This conservative restriction also applies to non-overlapping older provider pages.

The existing series remains on disk. To start accumulating anew after an unresolvable long absence, explicitly drop that symbol/timeframe, confirm, and refresh. Ticker reuse and renames need the same human judgment; a new ticker is a new identity.

## Coverage

Expected opens use the existing New York exchange calendar, including DST, holidays and half days. A half-day 1m session expects 210 bars rather than 390.

| State | Meaning | Shape |
|---|---|---|
| Complete | Every expected open is present | Solid green |
| Holes | Both ends covered, interior opens missing | Dark green with up to three notches |
| Partial | First or last expected open missing | Amber half-height |
| Missing | Session within the stored span with no bars | Hollow red |
| Failed | Missing/partial session, failed fetches and no success | Red with × |
| Pending | Today's session before its close | Muted outline |
| Closed | Calendar non-session | Omitted |

The application does not infer unscheduled exchange closures. Such dates can remain Missing. Holes are never filled. Fetch counters are attached to the observation's New York date; a failed initial fetch with unknown source is recorded under `unresolved`, which has no stored prices. Corrections mark the cell's top edge cyan.

## Reading and provenance

A normal stitch returns stored bars strictly older than the first fresh timestamp, then the fresh response, trimmed to the caller's limit. Stored tags are Cached; fresh tags are preserved. A quarantined fetch returns stored bars only. An unverified join returns fresh bars only.

The provenance line names the actual source, vault-only bar count, fresh count, session count, oldest date, interior holes, wholly missing sessions and any warning. Counts describe the returned input; the footer's coverage describes stored sessions.

CLOCK, SPATH, VPACE, RVAR, ACOR, GFILL, XTRM, RPATH, IBT, ICMP, chart Compare, BRD, BRIEF, cash levels, intraday scan and native TSTACK lane acquisition use the worker `analysis_bars` entry point. Provider request depth stays at the study's existing limit; the local read expands to up to 120,000 / 60,000 / 30,000 / 20,000 / 12,000 bars for 1m / 5m / 15m / 30m / 1H. A study's own session window still controls its statistics. Native lane TTLs remain in place.

A failed fetch can serve vault-only history when exactly one stored source is available. Ambiguous source selection returns the provider error. Refused/unverified input never becomes a fallback success. Vault-only CLOCK receives no observation clock: wall time cannot newly close its last print. Source lines say `VAULT ONLY · feed down`.

Chart initial loads respect the existing chart limit (`TR_HISTORY_LIMIT`). Refreshes ingest the fetched response before display merging. Vault-backed refreshes replace history from a consistent stitch, so a rebase cannot leave an older paged section at the previous scale. Chart paging currently asks the provider, not the vault; it still ingests eligible responses and refuses uncertain joins. There is no new archive or extended-hours stitch.

## Report cache

The memory LRU holds 256 typed `Arc` results. Its identity contains report kind, symbol, timeframe, source, input count and boundaries, stable OHLCV/timestamp digest, epoch, parsed parameter digest, code version and optional New York day. FNV-1a uses explicit little-endian encodings. Verified Cached and Real tags hash as the same provenance class. Every numerical input change changes the digest, including revisions to old bars.

CLOCK, SPATH, VPACE, RVAR, ACOR, GFILL, XTRM, RPATH, IBT, ICMP, BRD, final TSTACK computations, EDGE and CONE have memory caching. Partial multi-name reports with failed inputs and unsuccessful computations are not cached. Identical concurrent computations share a per-key lock; unrelated keys can compute independently. A memory hit avoids a compute task; value-based existing response APIs clone their report, while EDGE and CONE return the same Arc.

EDGE and CONE use the closed prefix. Their workers validate verified input before looking up a result. A valid vault stitch permits mixed Cached/Real inputs; mathematical modules receive uniform Cached tags with prices unchanged. Mixed history without a stitch is still refused. EDGE statistics now run exclusively on the worker, and stale chart keys are discarded. The chart shows UPDATING while a replacement is pending.

CLOCK keys its closed input plus day, observation availability and the current timeframe clock bucket, because staleness and today's extent depend on that clock. Other session reports currently key the entire input, including the forming display bar, to avoid freezing their live lines. They can therefore recompute within a bar; separating historical/live components is deferred. Parameters are parsed structs formatted consistently within this process; there is no persisted compatibility promise for those parameter encodings.

New closed bars, corrections, rebases, paging/window changes, parameter changes, source changes and keyed day changes invalidate by exact key mismatch. The current in-memory code version is 1; bump it when changing report mathematics. CONE's portable fingerprint has a pinned test vector.

Disk report caching and report caches for daily STATS, BT and LOOKALIKE are deferred. The schema reserves `vault_reports`, but this build does not populate it and reports do not survive restart.

## UI and commands

`VAULT`, `VLT`, `AAPL VAULT`, `NEW VAULT`, `SPLIT VAULT` and `Alt+B` open the pane. It shows series, counts, database allocation, per-session strips, source/epoch, notes and recent events. It refreshes on opening, an explicit Refresh click and every 60 seconds while visible. Snapshot responses are generation-checked. Repainting alone sends no snapshot request.

- `VAULT VERIFY`: worker `quick_check` and counter rebuild.
- `VAULT PRUNE`: opens a confirmation in the pane. Its Prune action applies per-series retention.
- `VAULT DROP AAPL 1m`: confirm deletion across sources for that timeframe. Omitting the timeframe drops all intervals of the symbol. It also invalidates reserved reports and that symbol/timeframe's display-cache rows/fetch timestamp.

The chart has a 6px footer whose cells align with the visible stored sessions. It shares the pane's colours and shapes; hover shows date, coverage, holes, source and epoch. The pane prints the shape key and fetch counts. HEALTH's BAR card links to VAULT. The status-bar chip, sortable table, depth multiplier, month labels, cell-to-chart navigation and footer visibility preference are deferred. Pane buttons provide refresh/verify/prune; dedicated R/V/P and 1–5 pane keys are deferred.

## Storage and migration

Schema version 2 is in `PRAGMA user_version`. It adds `vault_series`, `vault_bars`, `vault_sessions`, `vault_events`, and reserved `vault_reports`. Price rows use an integer series ID and `(series_id, ts)` primary key. Series carry an adjustment epoch and optional warning note. Existing `bars`, `fetches` and exchange-zone metadata remain the display layer.

Version 0/1 migration runs table initialization, the conservative provenance-column migration, vault creation, migration event and version change in one immediate transaction. A failure rolls back all those changes and the next open retries. Pre-provenance rows remain Missing/unverified. **No legacy display rows are promoted**, including rows with fetch times: their actual source and prior live-tick mutations cannot be established. Fresh verified fetches start the vault.

A newer schema opens with a read-only SQLite connection, disables vault operations and displays `VAULT DISABLED · newer schema`. It never downgrades the file. An open/quick-check failure leaves the existing file untouched and logs the error; live-only operation continues. There is no automatic corrupt-file rename. For manual recovery, stop the application, preserve the database plus WAL/SHM companions together, and inspect or move that backup before starting a new database.

WAL and synchronous=NORMAL remain, with a 2-second busy timeout. All new database opening and operations run through worker `spawn_blocking`; paint consumes delivered Arc snapshots. One mutex serializes the connection. Simultaneous identical native requests share one fetch and one ingest; the key includes symbol, timeframe, end and provider limit. Disk write failure leaves the fetch available live-only rather than stitching an uncommitted adjustment. This uses the blocking pool rather than occupying the four async runtime threads.

Retention removes whole oldest sessions to stay at or below these per-source caps:

| Interval | Bars |
|---|---:|
| 1m | 250,000 |
| 5m | 150,000 |
| 15m | 60,000 |
| 30m | 40,000 |
| 1H | 30,000 |

Pruning runs after a 30-second startup delay, every 24 hours and on request. No full VACUUM runs. Freed pages are reusable; allocated file size may not fall. The file-size cap and `TR_VAULT_MAX_MB` are **not implemented**. Size in the pane is `page_count × page_size` for the whole database, including the display cache; it excludes WAL allocation. Roughly 100,000 price rows are several MB, with indices/session/log overhead depending on SQLite allocation.

## Limits and known gaps

No no-overlap adjustment inference, legacy adoption, file-size enforcement, automatic corrupt-file replacement, rate-limit-event suppression or persistent reports. After a long gap, an unverified join needs manual reset or future archive-confirmation support. A second instance can receive SQLITE_BUSY after two seconds; the failed transaction is skipped and the current fetch stays live-only. Provider revisions inside tolerance retain stored prices by design. Rebase notes clear on the next successful non-rebase ingest rather than at a session boundary.

No live provider was queried to establish Alpaca H1 anchoring or IEX sparsity. The conservative exclusion and informational hole labels avoid claiming verification that the repository's fixtures cannot provide. Runtime contention under a real TWALL burst and GUI rendering have not been exercised; no GUI or server was launched for this slice.

## Measured timings

Run the offline storage microbenchmark with `cargo bench -p tr-data --bench barvault --profile dev`. It seeds 100,000 cash-grid fixture bars into in-memory SQLite, then measures five agreeing ingests of 2,730 overlapping bars and five 20,000-bar reads. On this workspace (dev profile, in-memory SQLite), the final five-iteration run measured **192.0 ms per ingest** and **7.6 ms per read**. They measure local storage work, not providers, UI latency or physical disk durability.
