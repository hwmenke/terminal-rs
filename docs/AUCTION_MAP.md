# Auction Map

`AUCTION` (`AMAP`, **Alt+P**) adds verified, closed-bar auction profiles to a live US cash chart at 1m, 5m, 15m, 30m or 1h. It works independently in each Cockpit pane. While enabled it replaces the legacy 24-bin profile, value-area lines and VP/VA legend readouts.

## Reading the map

Each loaded session has a profile growing right from its first chart bar. Amber marks POC; accent edges bound the value area. A five-step sequential volume ramp shows intensity relative to that profile's POC. Cyan hatches identify single prints (`T` marks a tail), purple notches identify LVNs, and muted brackets show initial balance with 1×/2× extensions.

The right dock sums the last N sessions or the visible whole sessions. Its header identifies the number of sessions, partial contributions and known gaps. Prior complete-session POC/VAH/VAL extend to a touch or a known vault gap. At most twelve on-scale level tags are shown, prioritising those nearest the last closed close. Tags are vertically separated with leader lines.

Annotated screenshot description (no GUI or screenshot was captured for this implementation): on a multi-session 5m chart, a left-aligned strip sits at each session's first candle; amber POC crosses the strip, accent VAH/VAL bound a faint wash, and `PARTIAL 77/78` identifies a missing source bucket. A wider dock grows left from the right edge under `COMP 10s`. A tag such as `nPOC … · 3s · 2 tests` ends an amber level. The bottom two lines identify the bin, value-area target, RTH cut, intensity key and source assumption.

Strips become VAL–VAH spines with POC dots below 28 pixels of session width, or below 200 pixels of pane height. Adjacent rows shorter than two pixels merge for display only. The composite hides below 420 pixels of pane width and below 200 pixels of height. None of these changes alters the calculation or autoscale.

## Controls

| Command | Effect |
| --- | --- |
| `AUCTION`, `AMAP` | Toggle focused chart |
| `AAPL AUCTION` | Load AAPL and enable the map |
| `AUCTION ON`, `AUCTION OFF` | Set visibility |
| `AUCTION 10` | Enable; composite last N sessions, N = 1–20 |
| `AUCTION VIS` | Enable; composite whole sessions overlapping visible chart bars |
| `AUCTION VA 68` | Value-area target, 50–95 percent |
| `AUCTION NAKED [ON\|OFF]` | Toggle/set prior levels |
| `AUCTION NAKED 15` | Maximum level age, 1–20 sessions with data |
| `AUCTION FILLED [ON\|OFF]` | Show/hide touched levels faintly |
| `AUCTION STRIPS [ON\|OFF]` | Show/hide session strips |
| `AUCTION IB [ON\|OFF]` | Show/hide initial balance and extensions |

Alt+P affects the focused chart only. The chart context menu's **Auction Map** section offers the layers and 5/10/20/Visible composite choices. Right-clicking a level tag also offers centre-origin and anchor-VWAP actions. Hover rows, tags or the footer for details; hover is disabled while the pane is being dragged.

Click a level tag to centre its origin session; origins before loaded history use the existing paging/jump path. Alt+click anchors AVWAP at the origin open. An exact loaded open is required; otherwise the existing `ANCHOR BEFORE LOADED HISTORY` notice appears. The existing four-anchor limit applies. Time Link Shift+click and the armed AVWAP tool have priority over Auction tags, which have priority over drawings.

`AUCTION US Equity`, `AMAP Equity` and `amap us equity <GO>` still load those tickers. Invalid function arguments are rejected. No new command aliases shadow PROFILE or POL.

## Data and status

Only verified real/cached bars are admitted. The worker refuses explicit sample/offline display mode even if the request happens to contain real bars. There is no generated-data fallback. The observation clock is the chart's last refresh, and the existing `closed_len` rule supplies its closed prefix. Fine bars also need `open + 60 ≤ chart cut`; a forming chart candle cannot update volume or touch a level.

For charts above 1m, a pane-independent worker slot shares one ordinary native 1m fetch per symbol per 45 seconds, including failed attempts. It requests the existing 5,000-bar provider depth, then stitches at most 9,000 rows from Bar Vault. On fetch failure it tries the vault alone. The helper never records a pane's chart source or changes its feed/clock. A 1m chart uses its own already-stitched bars.

Each session prefers 1m when those bars start in the first chart bucket, reach the cut and miss no more than 5% of expected minute buckets. Otherwise it uses that session's closed chart bars. Missing buckets stay missing; even tolerated fine holes mark the session partial. Sample/unverified or quarantined fine data is discarded. The footer identifies coarse fallback, and row cards give source interval, bars, expected buckets, holes and missing volume. The footer's math card names chart/fine source witnesses, session dates and ignored ETH bars.

| Mark | Meaning |
| --- | --- |
| `DEV` | Cut precedes cash close, with all expected closed source buckets present |
| `PARTIAL used/expected` | Missing source buckets or missing volume; only observed volume contributes |
| `½ DAY` | Complete scheduled half day |
| `GAP` | Level continuity stops at the last observed bar before a vault gap |
| `?` | A later partial session did not touch; continuity is uncertain |
| `⇅` | Consecutive bars jumped over the level without containing it |
| `◂` | Origin precedes loaded chart history |
| `…` | Previous verified report remains visible while newer inputs compute |

NaN/negative/nonfinite volume adds nothing and is counted as missing. Reported zero is valid but adds nothing. A zero-total session has no strip; if every session has zero/missing volume the map refuses. A whole missing session has no profile. Known chart and fine-vault gaps are combined conservatively; a composite that includes observations on either side advertises the gap rather than filling it.

## Math

All sessions in a report share a tick-anchored lattice. The tick is $0.01 at prices ≥ $1 and $0.0001 below $1. Let R be the median range of complete finished sessions (upper middle for an even count); if none exists, use the latest available session range. Width is `tick × nice_ceil(R / 40 / tick)`, with nice values 1, 2, 5 × powers of ten and a minimum of one tick. Row k is `[k·width, (k+1)·width)`. Finished-session ranges keep the lattice stable as the developing session extends, when at least one complete finished session is available.

For high H > low L, a bar contributes `volume × overlap(row, [L,H]) / (H−L)` to each row. Flat bars put all volume in their price row. Volume is conserved up to floating-point rounding. This assumes uniform volume across high–low; it is an OHLCV approximation, not observed tick volume at price. Coarser fallback bars spread volume across more prices and generally have greater approximation error.

POC is the maximum-volume row centre. Ties choose the row nearest the session range midpoint, then the lower row. Value area starts at POC and expands contiguously until it reaches the target share: larger adjacent volume wins; tied neighbours use the side with more remaining volume; a remaining tie chooses lower. Empty rows are steps, not skipped. VAL is the lowest included edge and VAH the upper included edge. Realised share can exceed the target.

A 30-minute bracket contributes once to every overlapping row, regardless of bar count. At least two consecutive rows with one bracket form a single-print run; a run touching a range end is a tail. Brackets and single prints are unavailable from 1h source bars. LVNs use smoothing `(¼,½,¼)`: an interior row must be a strict minimum over ±2 rows and at most 35% of the smaller maximum on either side.

Initial balance is the high/low of 09:30–10:30, available only after that hour closes and every source bucket is present and verified. Extensions add/subtract one and two IB ranges. The data report also computes open location against an adjacent complete prior session's range/value area; its display chip is deferred.

POC/VAH/VAL candidates come only from finished complete sessions with positive volume. Later closed RTH source bars touch a price iff `low ≤ price ≤ high`; the first such bar ends the level. A vault gap ends it at the last verified bar before that gap. A jump across price is flagged and does not count as a touch. Tests count later non-touching sessions whose observed high–low comes within one bin. Age counts subsequent sessions with data; missing days and holidays do not invent age steps. Candidates older than the configured limit are dropped. Touched reports remain available for the FILLED layer.

A composite sums aligned row volumes exactly, then applies the same POC/value-area algorithms. Partial sessions contribute their observed volume. N mode includes the developing session if present. VIS mode includes whole sessions whose observed session span overlaps the visible chart-bar span; it is not a cropped bar-range profile.

## Session calendar and refusals

Cash sessions use New York dates, the existing holiday calendar, 09:30 opens and scheduled 16:00/13:00 closes. Bounds go through DST-aware `ny::to_utc`; regular-session UTC offsets are constant within a session. Tests cover both 2026 DST transitions and Thanksgiving's half day. Extended-hours observations are ignored and counted.

The existing exchange-timezone/suffix gate also refuses known non-US cash symbols. Crypto/futures session support is deferred, along with FX/government; this version refuses all non-cash classes rather than choosing an uncertain session convention. Indices with no usable volume receive the volume refusal.

The refusal card is titled `AUCTION MAP · NO REAL DATA` and paints no Auction rows, levels, tags or market readouts. Common reasons are:

- `Auction Map refused: sample / offline display mode`
- `Auction Map needs a live intraday chart (1m–1h)` (also archives, H4 and daily/higher)
- `Auction Map needs a US cash session`
- `Auction Map needs closed verified bars`
- `Auction Map needs verified volume`
- The existing verified-provenance or vault mixed-history gate's reason.

Invalid parameters, unordered/invalid prices, pathological lattice spans (over 20,000 rows) and session volume overflow also refuse explicitly. An overflowing composite is omitted. A first pending request shows `AUCTION MAP · building verified session profiles…`. Its refusal/pending message is offset below a simultaneous Trend Cone message.

## Caches and performance

The UI only sends requests and paints cached geometry. Provider and SQLite work stay in the worker; SQLite and report hashing/computation run via `spawn_blocking`. There are three cache layers:

1. Per-symbol fine slots coalesce concurrent reads, with completion-based 45-second TTL and bounded completed-slot retention.
2. The shared report cache keys the exact closed chart and fine input digests, symbol/interval, params, class, cut, tick, sources, epochs and gaps. Failures are never inserted. A changed closed candle, correction, rebase or parameter invalidates; a forming-only tick does not.
3. Geometry keys report revision, loaded history positions, viewport, scale, rect, layer settings and palette. One mesh holds all profile rows. Composites and hover text rebuild only when geometry inputs change; unchanged frames reuse the shapes. Compatible new closed bars and prepended history retain the prior timestamp-based report while pending. Symbol/interval changes and incompatible corrections clear it. Stale responses are dropped.

Reproduce offline, with `CARGO_TARGET_DIR=/workspace/tr-auction-target`:

```sh
CARGO_NET_OFFLINE=true cargo bench -p tr-data --bench auction -- --noplot
CARGO_NET_OFFLINE=true cargo test -p terminal-rs --release auction_prepare_performance -- --nocapture
CARGO_NET_OFFLINE=true cargo test -p terminal-rs --release auction_vault_stitch_performance -- --nocapture
```

Measured in this workspace (benchmark/test fixtures only, no network):

| Operation | Measurement | Budget |
| --- | --- | --- |
| Release build, 20 × 390 1m bars | 1.693 ms estimate; 1.671–1.716 ms interval | ≤ 3 ms |
| Release composite, 20 profiles | 0.606 µs estimate; 0.558–0.665 µs interval | ≤ 50 µs |
| Release headless geometry, 20 sessions | 44.0 µs rebuild; 0.026 µs unchanged-key preparation | ≤ 300 µs rebuild |
| Release in-memory vault read + stitch, 7,800 rows, including worker scheduling | 4.05 ms | ≤ 50 ms typical |
| In-memory vault read + stitch, 7,800 rows, debug worker scheduling included | 15.6 ms (ten reads averaged) | ≤ 50 ms typical; file-backed latency unmeasured |

The initial geometry implementation took 442 µs in release. Sharing row-card metadata and formatting only the hovered row reduced it to 44.0 µs. The initial build benchmark was 4.90 ms. Removing repeated calendar conversions within each session brought it under budget without changing timestamps or filling data. No live provider-depth, file-backed SQLite latency or GUI frame-time claim is made. Native 1m availability remains provider dependent.

Verification on 2026-11-14 used offline Cargo and the shared target directory. The full app suite passed (503 passed, zero failed/ignored), including the 59-action keymap assertion and a fixed-clock native-fetch coalescing test that still requires exactly one provider call and one successful ingest. Auction-specific app tests also passed in release (31 passed, zero failed/ignored). The full `cargo test --workspace --no-fail-fast` run, using a fresh `TMPDIR` to prevent stale fixture reuse, reported 1,295 passed, 14 failed and 8 ignored. All 14 failures were existing broker/WhatsNews HTTP mock servers denied a loopback socket bind (`Operation not permitted`) by the execution sandbox; none were skipped or weakened. A fully green workspace run still requires an environment that permits those local mock servers. Clippy completed with existing warnings and no new warnings in the Auction Map changes.

## Saved layouts and limits

`PaneSettings::Chart.auction` is optional: absent/None means off. A present object uses serde defaults for omitted fields: strips=true, composite=10 (0 means VIS), value_area=70, naked=true, max_age=10, filled=false, ib=true. Restore clamps numeric values to their supported ranges. No journal, trade-plan or alert state is introduced.

Composite size and level age cap at 20. Calculation retains up to 21 observed sessions to supply a prior origin at the age boundary. Older vault-only sessions can supply levels but get no strip until chart bars for that session are loaded. There is no tick data, ETH profile, opening-type classifier, off-scale edge chip or open-location chip. Non-cash sessions are refused. Dense/small panes deliberately omit detail; every number displayed still comes from admitted closed observations.

## Verification in this environment

On 2026-11-14, the focused Auction tests passed: **62 passed, 0 failed, 0 ignored**. The full workspace run (`cargo test --workspace --no-fail-fast`, after also attempting `cargo test --workspace`) finished with **1,295 passed, 14 failed, 8 ignored**. All fourteen failures are existing loopback HTTP mock tests: the restricted sandbox rejects their `127.0.0.1:0` listeners with `Operation not permitted`. They occur in broker client/throttle/trading tests and the data `whats_news` tests. They were not skipped or changed to conceal the restriction; the workspace is not fully green here.

`cargo clippy --workspace --all-targets` completed successfully, with no warnings on the added/changed Auction code. Existing repository warnings remain. No GUI was launched and no live provider was contacted. A permitted loopback test environment is still needed to close full-suite acceptance.
