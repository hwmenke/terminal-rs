# Studio RESID — frozen residual tickers (slice 6)

A verified Lab or Relate fit is frozen into an immutable, versioned model file. Reopening it as a
*residual ticker* scores the **frozen** coefficients on live, re-admitted data. Nothing is ever refit
(a refit is a new Lab fit saved as a new version).

## Grammar

```text
STUDIO resid                                   saved-model list
STUDIO resid save [name=…] [monitor=N]         from the focused Lab / CORR(Relate) pane that has a fit
STUDIO resid open <id>                         open a saved model (<slug>-v<n>)
STUDIO resid check [<id>]                      fresh reload + rescore + checks
RESID SAVE … | RESID name=… | RESID OPEN <id> | RESID CHECK [<id>]   exact-verb aliases
NEW RESID                                      same as STUDIO resid
```

Bare `RESID` (and `RESID US Equity`, `RESID 1h`, `RESID DIST`, …) still loads the RESID ticker. A
verb with bad arguments (`RESID OPEN`, `STUDIO resid open ../x`, `save monitor=5`) is rejected, never
silently turned into the ticker. `name` matches `[A-Za-z0-9_-]{1,40}`; `monitor` is 20..=1000
(default `min(250, stored residuals)`; saving is refused below 20).

## Maths

Scoring reuses the Lab `build_design` (strict transforms, lags, gaps, complete-case rows):

ŷ_t = β̂₀ + Σ_j β̂_j x_{j,t−ℓ_j}, e_t = y_t − ŷ_t, z_t = (e_t − μ_saved)/σ_saved.

β̂, μ_saved (mean of all n_train training residuals) and σ_saved (√(SSE/(n−k))) come only from the file.
`e` is a signed `ScalarSeries` in the target-transform unit (never a `Bar`, never price-inverted);
`z` has unit "z (saved training σ)". Appending rows never changes an earlier value.

Segments: **TRAIN** (ts ≤ trained_thru), **EMBARGO** (next h_y−1 target bars, which share support with
training) and **SCORED**. Checks use SCORED rows only.

**ADF** (latest contiguous SCORED run, T ≥ 60): Δe_t = α + γe_{t−1} + Σδ_iΔe_{t−i} + u, Schwert bound
p_max = ⌊12(T/100)^¼⌋, BIC lag on a common sample, MacKinnon (2010) N=1 constant response-surface
critical values at T = n_eff. H0: unit root. Not an Engle–Granger cointegration certificate: training
residuals are never tested, and the coefficients were estimated on a disjoint earlier sample.
Overnight bars of intraday cash series sit inside the ADF segment (the grid is continuous).

**Fixed-horizon CUSUM**: S_k = Σ(e_i − μ_saved), D_k = max|S_j| over the first m = monitoring_horizon
scored observations. Reference: B = 499 stationary-bootstrap resamples of the stored training residuals
(block max(⌈n^{1/3}⌉, h_y), seed = content digest, centred with μ_saved). critical = 475th order
statistic, p = (1+#{D* ≥ D})/(B+1). A verdict is shown only when k = m; before that it is "partial k/m ·
descriptive". Pointwise envelope is context only.

**Half-life** (AR(1), bias-corrected φ̃ = (nφ̂+1)/(n−3)): −ln2/ln φ̃ when 0 < φ̃ < 1, else "no finite
half-life". The half-life bootstrap is cut.

Family of inferential outputs: "RESID checks per model" = {ADF τ, CUSUM p}. z is descriptive only.

## Real-data guarantees

Save requires the displayed verified report (receipts and span digests per input; reports from older
builds are refused). Scoring re-admits every live input; a factor error makes the residual unavailable —
no partial model. Revision: the live span digest over each input's bars in [train_start, trained_thru] is
compared with the saved one (`INPUT REVISED`); the lag+h warm-up bars before train_start are not covered.
A revision never rewrites the file. Sample/offline mode makes no Score/Check request. Keys contain no
wall clock (`observed_at`, `saved_at` are display only; daily `observed_at` is an archive load time).

## File format

`<data_dir>/studio/models/<slug>-v<n>.json`, pretty JSON of `FrozenFit` (schema 1, method `resid.v1`):
symbols, numbers, sources, receipts, ≤ 5,000 training residuals. No credentials. Files are never
overwritten or deleted by the app; saves are serialised in-process, written to a unique temp file and
`hard_link`ed to the final name, which fails if it exists (so two app instances cannot overwrite each other).
Loading validates id, size (≤ 4 MiB), schema, digest (FNV-64 over statistical content) and invariants
(embargo = h − 1, first break, stored-residual counts, bootstrap block).

The digest is an explicit field-by-field encoding (integer tags, f64 bit patterns; receipts contribute
source, provenance, `closed_thru`, `input_digest` and gaps, never `observed_at`) and includes
`source_report_key`, which holds no wall clock. A Relate save takes its timeframe from the report, not
the editable text box, and is refused if they disagree. A saved source is compared by provider name only
(the intraday vault label's bar/session/hole counts and notes are not identity). Default names use `_p{n}`
for extra factors because `+` is not allowed in names.

`serde_json`'s `float_roundtrip` feature is needed so saved f64s read back bit-exactly. Cargo unifies
features, so it is **workspace-wide**: all serde_json float parsing becomes exact, somewhat slower, and a
parsed value may differ by 1 ULP from earlier builds (bit-exact digests/cache keys of freshly parsed data
change once).

## Cut list

Cut: half-life bootstrap / non-reversion mass. Nothing else from the spec's cut order was cut.
