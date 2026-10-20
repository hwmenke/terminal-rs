# M1 first slice: Archive → Scan

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

Whats-News already has read endpoints in `~/wn-fix/app.py`. The bridge prefers **GET-only** local HTTP requests:

- `/api/symbols`, plus `/api/ohlcv/<ticker>?freq=daily&limit=1` for coverage (up to 12 concurrent requests; 30-second universe timeout).
- `/api/setups/scan?setup=<NAME>&universe=1&limit=0`, then latest dates for matched symbols.
- `/api/ohlcv/<ticker>?freq=daily&limit=1000000` for archived chart history.

HTTP defaults to `http://127.0.0.1:8050` (the Python app's default). Redirects are disabled and URLs must use local/loopback HTTP. Network requests have timeouts. No fetch, sync, promote, refresh-ingestion, or other mutation endpoints are called.

If HTTP fails, use SQLite with `SQLITE_OPEN_READ_ONLY` and connection-local `PRAGMA query_only=ON`; missing files are never created. Scans use one read transaction. Invalid OHLCV causes a visible error, not fabricated bars. Nothing initializes or migrates the upstream database or runs Python code there.

Environment variables, read at request time:

| Variable | Behavior |
| --- | --- |
| `WHATS_NEWS_DB` | SQLite fallback path. Defaults to `$HOME/wn-fix/finance.db` if it exists, otherwise `$HOME/Whats-News/finance.db`. Use an absolute or shell-expanded path. |
| `WHATS_NEWS_URL` | Local HTTP service URL. Set to `off` (or empty) to use SQLite directly. An explicit DB path still follows HTTP unless HTTP is disabled. |
| `WHATS_NEWS_OFF=1` | Disable the entire bridge, including SQLite. |

Example: `WHATS_NEWS_URL=off WHATS_NEWS_DB="$HOME/wn-fix/finance.db" cargo run`, then enter `UNIV` or `SCAN DARVAS_BREAKOUT`.

HTTP supports the existing service's nine setups: `DARVAS_BREAKOUT`, `DARVAS_BOX`, `DARVAS_FAIL`, `NEAR_HIGH`, `VOL_SURGE`, `EP`, `BREAKOUT_QUEUE`, `RSI_OB`, `RSI_OS`. Invalid names (including bare `SCAN`) display the choices.

SQLite supports the first seven. Darvas follows `portfolio.py`'s 20-bar high / three-bar confirmation over up to 260 stored daily bars. Basic gap, near-high and volume conditions follow the same thresholds; volume compares the latest bar with the **preceding** 20 bars. SQLite scans require at least 30 daily bars and sort by ticker. They deliberately omit the Python composite score/rank; HTTP results retain the service score. Source and ordering differences are explained in the pane. Stale symbols remain eligible and are clearly marked.

## Validation

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
