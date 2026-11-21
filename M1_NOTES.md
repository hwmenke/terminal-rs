# M1 first slice: Archive → Scan

The price-history backlog item supersedes the five-session freshness threshold and daily-only
chart limitation described below. See [Price history](docs/PRICE_HISTORY.md) for current behavior.

Implemented in terminal-rs only. No commits or pushes; no changes to `~/wn-fix`.
Context: `~/tr-plan.log` (archive → scanners → chart discovery loop).

## Behavior

- `UNIV` browses all registered Whats-News tickers, including archive-only and desk symbols. Shows name, group/index tag, latest **daily** bar date and `OK`, `STALE`, or `MISSING`. Filter searches ticker/name/group. Empty archives show an explicit empty state.
- `STALE` means more than five US equity trading sessions after the last daily bar, using the computer's local calendar date. Weekends and regular NYSE holidays (including Good Friday and observed holidays) are excluded. Exactly five sessions is still `OK`. Ad hoc exchange closures are not modeled. `MISSING` means no daily bar/date (weekly-only coverage also counts as missing).
- `SCAN DARVAS_BREAKOUT` and other named scans display matches, scanned count, data dates/freshness, close and available box levels. Click a universe ticker or scan result to open its archived daily chart. Both panes support filtering and manual refresh. Commands are also listed in Help and the command palette.
- Research panes and archive-chart source selection survive saved layouts; old layouts default to regular market charts. `NEW UNIV`, `NEW SCAN DARVAS_BREAKOUT` and `SPLIT SCAN NEAR_HIGH` also work through panel specifications.
- Research chart requests bypass the market-data provider, market bar cache, live feeds and comparison provider. Archive charts are visibly labeled and daily-only. Range buttons change the visible span without changing the interval. Missing/offline history shows an error rather than substituting bars. Create a regular chart with `NEW CHART` to return to other market intervals/providers.
- Missing/disabled bridges show `Whats-News unavailable` inside the research pane/chart; work happens on the background worker, leaving the rest of the terminal usable. No research fallback uses sample/synthetic data. Artificial fixtures are confined to unit tests.

## Bridge and configuration

`crates/data/src/whats_news.rs` implements the bridge; `app/src/app/research.rs` renders results, with worker requests/responses in `app/src/worker.rs`.

Settings › **Whats-News** now persists the archive path, optional HTTP URL, source preference, and enable toggle in `prefs.json`. Changes apply immediately, and **Test connection** starts a fresh background check. A blank archive path auto-detects `~/wn-fix/finance.db`, `~/Whats-News/finance.db`, then `~/whats-news/finance.db`; an explicit missing path is never replaced. `~/` is expanded. The configured archive is checked for existence, readable Whats-News schema, daily coverage and explicit unverified provenance even when HTTP is active.

The clickable status strip opens that settings section and reports **Connected (HTTP)**, **Connected (archive file)**, **Disconnected**, or **Not configured**, with the selected path/URL, registered symbol count and latest daily data date. Failed preferred sources produce an actionable warning when the alternate source is used.

An independent worker task checks health every 30 seconds. Failures retry after 5, 10, 20, 40, 80, then at most 120 seconds; recovery resets that schedule. Configuration changes cancel obsolete probes/backoff. Settings generations discard obsolete responses, and open research panes/archive charts clear on disconnect and reload on recovery. Research errors use the same plain-English connection message. All work remains read-only; there is no sample fallback.

Whats-News has read endpoints in `~/wn-fix/app.py`. The bridge uses **GET-only** local HTTP requests:

- `/api/symbols`, plus `/api/ohlcv/<ticker>?freq=daily&limit=1` for coverage (up to 12 concurrent requests; 30-second universe timeout).
- `/api/setups/scan?setup=<NAME>&universe=1&limit=0`, then latest dates for matched symbols.
- `/api/ohlcv/<ticker>?freq=daily&limit=1000000` for archived chart history.

HTTP defaults to `http://127.0.0.1:8050` (the main Python app's default); the optional `data_service/app.py` defaults to port **8051**. Both provide `/api/symbols` and `/api/ohlcv/<ticker>`. The data service lacks `/api/setups/scan`, so a 404 there uses HTTP archive history for the seven local scan calculations, preserving the HTTP source and provenance. Redirects are disabled and URLs must use local/loopback HTTP. Requests have a 1-second connect timeout and 4-second total timeout; universe reads have a 15-second overall HTTP budget and scans a 20-second budget. No fetch, sync, promote, refresh-ingestion, or other mutation endpoints are called.

If HTTP fails, use SQLite with `SQLITE_OPEN_READ_ONLY` and connection-local `PRAGMA query_only=ON`; missing files are never created. Scans use one read transaction. Invalid OHLCV causes a visible error, not fabricated bars. Nothing initializes or migrates the upstream database or runs Python code there.

Environment variables seed connection preferences when that section is absent from `prefs.json`; saved settings take precedence and can be changed without restarting:

| Variable | Behavior |
| --- | --- |
| `WHATS_NEWS_DB` | Archive path. If absent/blank, auto-detect the locations above. Absolute paths and `~/` paths are supported. |
| `WHATS_NEWS_URL` | Local HTTP service URL. Set to `off` (or empty) to use SQLite directly. An explicit DB path still follows HTTP unless HTTP is disabled. |
| `WHATS_NEWS_OFF=1` | Seed the connection enable toggle as off, including SQLite. |

Example (before a connection configuration has been saved): `WHATS_NEWS_URL=off WHATS_NEWS_DB="$HOME/wn-fix/finance.db" cargo run`, then enter `UNIV` or `SCAN DARVAS_BREAKOUT`.

HTTP supports the existing service's nine setups: `DARVAS_BREAKOUT`, `DARVAS_BOX`, `DARVAS_FAIL`, `NEAR_HIGH`, `VOL_SURGE`, `EP`, `BREAKOUT_QUEUE`, `RSI_OB`, `RSI_OS`. Invalid names (including bare `SCAN`) display the choices.

Step 5 extends SQLite and local Yahoo archives to all nine setups, using shared
Rust calculations over full daily history. It adds strength/RSI, volume and range
filters and sorting, completed weekly/monthly conditions, default stale exclusion,
per-symbol skip explanations, batched SQLite reads and calculation caching.
Original HTTP service fields remain separately labelled. See
[Scanner calculations and controls](docs/SCANNERS.md) for the current contract.


## Dependable-link validation

- `cargo build --offline`: passed.
- Application tests: **110 passed**, including preferences persistence, live configuration changes, obsolete response rejection across settings changes and reconnects, status-strip navigation, and research/chart disconnect/recovery.
- Whats-News tests excluding socket fixtures: **13 passed**, covering archive disconnect/recovery, auto-detect, schema/path validation, missing file/bad URL/empty archive messages, preference, backoff, provenance, and read-only access.
- Localhost tests are written for both main-app and data-service routes, HTTP disconnect/recovery, timeout advice and archive fallback. The sandbox rejects listener creation with `Operation not permitted`, so these cannot run here. Existing brokerage socket tests block the full workspace test run for the same reason.
- No changes or ingestion in `~/wn-fix`; no commits.

## Original M1 validation (historical)

Added tests for command parsing, layout/source persistence, archive-only chart request routing, freshness/weekends/holidays, Darvas states/confirmation, malformed OHLCV, SQLite coverage and scans, read-only enforcement, absent/disabled bridges, HTTP preference/404 missing data, and HTTP failure → SQLite fallback.

Completed:

- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.
- Isolated `rustc --test` execution of the actual Darvas function and its state/confirmation test (extracted unchanged, with a minimal Bar type): **1 passed**.
- Executed the exact archive SQL queries with Python's SQLite against an isolated fixture: passed, including weekly-only → missing daily coverage.
- Read-only inspection confirms the local `~/wn-fix/finance.db` currently has **0 symbols and 0 daily bars**. No data was imported or generated into it.

Attempted, but **blocked before compilation**:

- `cargo build`
- `cargo test`
- `cargo test --workspace` (needed to include the new `tr-data` tests)

All stop while resolving the existing `chrono` dependency because `index.crates.io` cannot resolve in this environment. `cargo build --offline` also fails: this machine has no cached crates. The full crate tests and GUI have therefore **not been validated**. The standalone Darvas/SQL checks are not substitutes for the complete build/test suite.

## Left

1. With dependencies available, rerun `cargo build` and `cargo test --workspace`, then smoke-test `UNIV` → `SCAN DARVAS_BREAKOUT` → select ticker with a populated archive and with `WHATS_NEWS_OFF=1`.
2. Load/refresh real data through the separately managed Whats-News app; this bridge intentionally performs no ingestion.
3. Optional later slices: SQLite RSI/composite-score parity, additional archive chart intervals/paging, an aggregate HTTP coverage endpoint (avoids per-ticker reads), and research-specific watchlist promotion. Broader stats/backtests/KNN/PM Desk features remain outside this slice.
