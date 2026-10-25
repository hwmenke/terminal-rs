# Adaptive Trend overlays

`ADTREND` (aliases `ATREND`, `ADAPTIVE`) draws Whats-News Adaptive Trend on the
**price pane**: short / medium / long adaptive baselines, destination bands,
regime-change dots, and ratcheting trailing-stop guides.

The study is a native Rust port of the Whats-News Adaptive Trend rules
(`adaptive_trend.py` conceptually). Python is not shipped in the runtime.

## How to enable

| Action | Result |
|---|---|
| `ADTREND` | Toggle the overlay on the focused chart |
| `ATREND` / `ADAPTIVE` | Same study |
| `ADTREND 14 0.3 1` | ATR 14, confirm 0.3, ADMA smoothing (1). Defaults: ATR 20, confirm 0.25, KAMA (0) |
| `IND` / `IND TREND` | Picker; search “adaptive” |
| Chart menu → Indicators → Trend → Adaptive Trend | Same toggle |
| Chart menu → Review overlays → Adaptive Trend | Same instance; saved with the layout |

Repeating the command removes it. The spec is stored with the chart like any
other study (`"ADTREND 20 0.25 0"`), so it survives layout save/restore.

`REVIEW` does **not** turn Adaptive Trend on by itself.

## What the lines mean

| Overlay | Meaning |
|---|---|
| **SB / MB / LB** | Short, medium and long adaptive baselines on typical price (HLC3). KAMA by default: efficiency-ratio smoothing between a 2-bar and a 30/60/120-bar constant. Method `1` is ADMA (same blend, no Kaufman square). |
| **SDB / MDB / LDB** | Destination / take-profit bands. They sit on the profit side of the matching baseline (`center ± k×ATR`) and **only ratchet with the trend**. Multiples: SDB 2.0, MDB 4.5, LDB 4.5. |
| **MRT / LRT** | Trailing-stop guides. Medium ratchet trail (2.25×ATR from MB, driven by the **medium** regime) and long ratchet trail (2.25×ATR from LB, driven by the **long** regime). Long: below price, only moves up. Short: above price, only moves down. A regime flip resets the band. |
| **Long flip / Short flip** | First bar of a new **medium** regime (the trade-management state). The opening bar of the series is never a signal. |

The HUD reads `AT  S+ M+ L-  · MRT …  LRT …`. `+` long, `-` short, `0` not yet
fired. **Medium** is the master state for trade management; short and long are
context. Stops are guides, not broker orders.

Warm-up is NaN until each window fills (SB 10, MB 20, LB 40, ATR 20). A flat
regime leaves destination/stop bands blank until the first fire, then holds the
last level across a flat stretch.

## Provenance

Adaptive Trend that claims live archive analysis **refuses** sample, unverified,
mixed or empty series. There is no silent fallback onto demo bars. The chart
keeps the study toggled and paints `ADTREND refused: …` in red.

Verified real or cached bars (research archive, Whats-News bridge, live Yahoo)
compute normally. `TR_PROVIDER=sample` never draws the bands.

## Parameters

| Param | Default | Role |
|---|---|---|
| ATR length | 20 | Wilder ATR for confirm and band width |
| Confirm | 0.25 | `confirm × ATR` deadband so small wiggles do not flip a regime |
| Method | 0 | `0` KAMA (squared), `1` ADMA (linear) |

Horizon windows stay at the Whats-News defaults (SB 10/2/30, MB 20/2/60,
LB 40/2/120, min-hold 10 bars).

## Out of scope

Step 14 backtests and Step 15 lookalikes are later. This study does not place
orders or change STATS / SCAN.
