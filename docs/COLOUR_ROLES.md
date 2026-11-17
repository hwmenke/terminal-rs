# Colour roles

Each colour channel carries exactly one meaning. Roles live in `app/src/theme.rs`
(`ColourRole`, `Roles`, accessors); heat scales and formula cards are in
`app/src/widgets.rs` (`HeatScale`, `MathCard`, `stat_tile`).

| Channel | Roles | Accessor | Rule |
|---|---|---|---|
| Sign | SignPos / SignNeg / SignZero | `theme::sign(x)` | Theme `up`/`down`; only for signed numbers and candle bodies. Every signed number also prints `+`/`−`. NaN and 0 are muted. |
| Regime | RegimeUp / RegimeDown / RegimeFlat | `theme::regime(s)`, `theme::regime_lane(s)` | Model state only (TSTACK lanes, candle tint, Stop Cloud, Trend Cone, TWALL). Periwinkle/orange; never green/red. |
| Provenance | ProvVerified / ProvCached / ProvSynthetic / ProvUnverified | `theme::provenance(p)` (text), `theme::rail(p)` (rail) | Verified data has no hue. Exceptions are coloured and hatched. |
| Significance | SigPass / SigFail / LowN | — | No hue: pass is full strength, fail muted/outline, low n a caution corner tick. |
| Chrome | Caution, Alert, Info, Highlight, Accent, Muted | `theme::caution()`, `alert()`, `info()` | Amber is text/outline only. Alert is for errors and unavailable data, always with words. |

Categorical colours (`series`, window links, party, asset class, RRG quadrants)
identify things and never encode sign or regime. Link colours come from
`theme::link`, with light-theme variants.

## Heat fields

- Signed fields use the five-step diverging ramp (`theme::heat_div`, blue-teal ↔ red).
  The outer steps start at ±scale and the middle step spans ±scale/5.
- Unsigned magnitudes use the hue-free slate ramp (`theme::heat_seq`).
- **Every heat surface prints a key.** Use `HeatScale::{diverging, sequential}`:
  `fill`, `fill_gated`, `ink`, `key` and `key_at` share one set of thresholds, so a key
  can't disagree with its cells. `.nice()` rounds the scale up to 1, 2 or 5 × 10^k.
- Ink on a filled cell is `theme::on_fill(fill)`, which is black or white,
  whichever has more contrast.

## Math cards

A statistic's hover card is a `MathCard`. Its sections always appear in this
order: title (with value and provenance tag), formula, substituted inputs, footer
(`n=… · from → to · CLOSED THRU … · source`), notes, caveat (⚠) and unavailable
reason (—). When a card is unavailable, it shows the reason in place of the value
and hides the numeric sections. `stat_tile` draws a tile with a dotted underline
that marks the hover card.

## Theme files

Custom themes may set `colors.regime_up` and `colors.regime_down` (optional, `#rrggbb`).
If they are absent, the light/dark role defaults apply, so old theme files load
unchanged.

## Guards (tests)

- `theme::tests`:
  - Data roles are at least 0.10 apart in OKLab in every built-in theme.
  - Hue roles have at least 3:1 contrast on the card, and ink on every ramp step
    has at least 4.5:1.
  - The regime poles and diverging-ramp ends survive Machado 2009 protan and deutan
    simulation.
  - Regime never matches sign.
  - Both ramps are monotone.
  - Old theme files load.
- `widgets::colour_guards::every_heat_surface_prints_a_key`: every file that uses a
  heat ramp or `HeatScale` must also print a key.
- `widgets::colour_guards::no_new_raw_colours`: raw `Color32::from_rgb` /
  `Color32::RED` outside the theme files must stay within `RAW_COLOUR_BUDGET`.
  Lower the budget; never raise it.

Relationship Studio's covariance fan and rolling band use categorical series
colour. Coefficient/holdout/robust-slope numbers and interval endpoints use sign
ink; BY q text and method labels stay neutral. SEASON's two fixed family chips
are neutral counts, and its simultaneous ALL band shares the ALL curve identity.
Neither the family split nor the band introduces a significance hue.

DIST volatility classes use categorical `series([0,4,5][i])` on both tabs, with
Calm / Normal / Stressed printed beside swatches. Their identities do not encode
return sign or caution. Fingerprint Δμ and CI endpoints use explicit signed ink;
whiskers use identity colour, zero is muted, and q≤0.10 uses a neutral outline.
LOW N uses an amber corner tick. Ridges are probability masses, with shared axes
and scales rather than heat fills.

DIST Shape uses categorical series colours for KDE / Normal / Student-t identities.
Its river's central probability masses use `HeatScale::sequential` and print the
shared slate key. Mean/tail/quantile numbers use signed ink with explicit signs;
scale, density, counts and probabilities are neutral. Year identity and unsigned
volatility remain neutral beside signed mean ink. LOW N is a caution corner tick;
pointwise bootstrap intervals carry no significance hue or test-family claim.

Quick canvas lines/identifiers use categorical series roles. Change/Z values and
base deltas use signed ink with explicit +/−. The count strip is neutral slate;
its now pin and date lanes are neutral. Unit-specific Z/vol keys are printed; no
heat surface or significance hue is added. LOW N reference/strip samples use a
caution corner tick. See STUDIO_CANVAS.md for the separate midrank convention.
