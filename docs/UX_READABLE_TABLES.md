# Readable research tables and charts (UX cleanup slice 2)

## Measured numeric tables — `widgets::numeric_table`

`numeric_table(ui, id, &[NumCol], &[NumRow], pane_w)` replaces fixed width fractions. Column
widths come from the actual text (monospace digits, so numbers align on the decimal point):

* `NumColKind::Value` — widest header/cell text, never below `min_w`, **right-aligned**.
* `NumColKind::Label` — flexible, elided by measurement with `…` (full name in the row's MathCard).
* `NumColKind::Graphic` — flexible; the cell paints its own mark (the 95% CI whisker).
* Heat chips (VIF) keep their colour role; the chip has its own padded slot.

`numeric_layout()` is pure and unit-tested. Panes **≤ 480 pt** are compact: columns marked
`.detail()` (SE, z, p, q, VIF in the Lab) leave the grid and appear as measured `title value`
pairs in a wrapped detail line when the row is clicked. Wider panes shed detail columns
right-to-left before anything is squeezed. Whenever any detail column is hidden (compact or not) the
table prints "click a row for …" built from the hidden column titles. If even the primary columns do
not fit the table scrolls horizontally. Value columns are never elided, and a chip's 6 pt padding is
part of the measured width. Every laid-out rect is returned (`NumTableOut`) and tested for
non-intersection at 320 / 480 / 520 / 640 / 900 pt, pixels-per-point 1 and 2, with long factor names
and large z / VIF values; a real click on a compact row is tested.

The Lab report body gives the table 600 pt (side by side with the plots from 972 pt), otherwise the
table stacks full-width above the plots, so 640–1440 pt panes show the full grid.

The Regression Lab coefficient table (`studio_lab::table`) is the first user. Cut from this slice:
the other fixed-fraction tables (Attribution, Season, Dist, Rotation) still use their own layout.

## Typed number formatting — `fmt_num`

`count` (thousands separators), `signed` (real minus; a non-zero value that would round to zero prints
`+<0.0001`), `prob` (`<0.001`, never `0.000`), `ret_bp` (`+0.12 bp/obs`, test-only until a site prints a bare return), `level` (σ, κ without a forced
exponent), `exact` (full precision for hover) and `CoefScale` — β, SE and CI ends of a set share one
scale (exponent from the largest value; **decimals from the smallest standard error only**, capped at 4,
so a CI end near zero or a tiny β never forces false precision); scaling is by multiples of three with the unit tag (`×10⁻⁶`) appended to the column unit, so a
scaled value never reads as a different unit. The intercept has its own scale and says so in its cells.
Missing / non-finite values print `—`. Exact β / SE / p / q values are in the row's MathCard.

## One legend owner — `chart_view/legend.rs`

`LegendRows` is held on `ChartState` (`RefCell`): `paint_legend()` resets it each frame (rows are stamped with the egui pass number, so a frame where
`paint_legend` was skipped never leaks rows into the caption) and places the
OHLC header (wrapping on narrow panes), session lines, indicator rows (SMA20, SMA50 …), HUDs and
research lines through it (measured galley heights, elided width); legacy helpers that advance a raw
`y` are recorded with `reserve_to`. `studio_fair_value()` appends its caption below the last row
through the same allocator. The caption is now **"Fair value · frozen fit"**; thru-date, band
definition and source moved to its hover, as did the long in-sample / embargo note.
Tests assert the recorded rects never intersect (widths 200–1440, pixels-per-point 1 and 2).

## Studio header vs chart

`studio_hub::show()` puts the header, lens rail and About/Build notes in a height-capped scroll area
(`chrome_cap`) so the chart is allocated first. The chart minimum is 240 pt (native) / 280 pt (Quick
canvas); the old 120 pt floor is gone. When the pane cannot hold the minimum chart plus 140 pt of
analysis, the body stacks and scrolls (`split_layout`).

## Data

No synthetic market series were introduced. Tests build fixtures in code with the existing
`tr_perf::fixtures` (the same ones the paint gate already uses); the app never uses them.
