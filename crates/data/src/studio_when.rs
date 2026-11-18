//! When: causal rules, completed simple forward returns, within-clock HAC
//! contrasts, prior-only feature quintiles and session-block bootstraps.
//!
//! No provider calls, interpolation, price substitution, iid KS tables or UI
//! work. Everything here runs on worker threads over admitted real bars.
use crate::{
    stable_hash::Fnv64,
    stats::{Cmp, bounded_feature, match_mask, parse_conditions},
    studio::{ScalarSeries, SeriesReceipt, SeriesUnit},
    studio_canvas::{CanvasInput, CanvasSpec},
    studio_inference::{HacEstimate, clock_contrast},
};
use std::collections::{BTreeMap, BTreeSet};
use tr_core::{Bar, Timeframe};

/// Fixed, predeclared horizon family (bars). Never selected by result.
pub const HORIZONS: [usize; 4] = [1, 3, 6, 12];
pub const DEFAULT_RULE: &str = "RSI(14)<30";
pub const DEFAULT_THRESHOLD_BP: f64 = 10.;
/// Session-block bootstrap replicates (When probability tiles and subset ECDF).
pub const DRAWS: usize = 499;
/// Shared equal-width bins for rule/complement and selected/rest histograms.
pub const BINS: usize = 32;
/// Minimum observations per group before any inference is shown.
pub const MIN_GROUP: usize = 30;
/// Minimum contributing sessions per group before any inference is shown.
pub const MIN_SESSIONS: usize = 20;
/// Replicates that resample no member of a group are excluded and disclosed;
/// beyond this many (5% of draws) the interval is withheld.
pub const MAX_EXCLUDED: usize = DRAWS / 20;
/// Prior finite feature values required before quintile edges exist.
pub const MIN_EDGE_HISTORY: usize = 30;
const MAX_GRID: usize = 120_000;
const MAX_SESSIONS: usize = 20_000;
const METHOD: &str = "when.v1|simple R bp|FE clock contrast|scalar-score Bartlett L=h+4|BY m=8|prior quintiles|session499";

/// Bartlett lag for horizon `h`: overlap `h−1` plus five bars of persistence.
pub fn hac_lag(h: usize) -> usize {
    h - 1 + 5
}

#[derive(Debug, Clone, PartialEq)]
pub struct WhenSpec {
    pub conditions: Vec<Cmp>,
    pub horizons: [usize; 4],
    pub threshold_bp: f64,
}
impl Default for WhenSpec {
    fn default() -> Self {
        Self::parse(DEFAULT_RULE, DEFAULT_THRESHOLD_BP).expect("default rule")
    }
}
impl WhenSpec {
    pub fn parse(rule: &str, threshold_bp: f64) -> Result<Self, String> {
        if rule.len() > 1024 {
            return Err("Rule exceeds 1024 characters".into());
        }
        let conditions = parse_conditions(rule).map_err(|e| e.to_string())?;
        if conditions.is_empty() || conditions.len() > 8 {
            return Err("When requires 1..8 comparisons joined by AND".into());
        }
        if !threshold_bp.is_finite() || threshold_bp.abs() > 1e6 {
            return Err("Threshold must be finite and within ±1,000,000 bp".into());
        }
        for c in &conditions {
            for operand in [&c.left, &c.right] {
                bounded_feature(operand, &[]).map_err(|e| e.to_string())?;
            }
        }
        Ok(Self { conditions, horizons: HORIZONS, threshold_bp })
    }
    /// Parsed echo, in input order.
    pub fn rule(&self) -> String {
        self.conditions.iter().map(Cmp::label).collect::<Vec<_>>().join(" AND ")
    }
    /// Canonical AST: AND is commutative/idempotent, so sorted unique clauses.
    pub fn canonical(&self) -> String {
        let mut ast: Vec<_> = self.conditions.iter().map(Cmp::label).collect();
        ast.sort();
        ast.dedup();
        ast.join(" AND ")
    }
    /// Feature used for the prior-only quintile buckets: the first non-constant operand.
    pub fn bucket_feature(&self) -> &str {
        self.conditions
            .iter()
            .flat_map(|c| [c.left.as_str(), c.right.as_str()])
            .find(|o| o.trim().parse::<f64>().is_err())
            .unwrap_or(self.conditions[0].left.as_str())
    }
}

/// Arguments for `WHEN` or the rule suffix on `STUDIO … lens=when`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WhenCommand {
    pub tf: Option<Timeframe>,
    pub rule: Option<String>,
    pub horizon: Option<usize>,
    pub threshold: Option<f64>,
}

/// Parse `5m RSI(14)<30`, `rule="RSI(14)<30 AND close>SMA(50)" h=6 threshold=10bp`.
/// Quotes keep spaces; anything that is not a key or interval is rule text.
pub fn parse_command(s: &str) -> Result<WhenCommand, String> {
    let mut out = WhenCommand::default();
    let mut words = vec![];
    let mut word = String::new();
    let mut quoted = false;
    for c in s.chars() {
        if c == '"' {
            quoted = !quoted;
        } else if c.is_whitespace() && !quoted {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(c);
        }
    }
    if quoted {
        return Err("Unclosed rule quote".into());
    }
    if !word.is_empty() {
        words.push(word);
    }
    let mut rule = vec![];
    for w in words {
        let lower = w.to_ascii_lowercase();
        if let Some(x) = lower.strip_prefix("h=") {
            let h = x.parse().map_err(|_| "h must be one of 1, 3, 6, 12")?;
            if !HORIZONS.contains(&h) || out.horizon.replace(h).is_some() {
                return Err("h must be one of 1, 3, 6, 12, given once".into());
            }
        } else if let Some(x) = lower.strip_prefix("threshold=") {
            let t = x.strip_suffix("bp").unwrap_or(x).parse::<f64>().map_err(|_| "threshold is in bp, e.g. 10bp")?;
            if out.threshold.replace(t).is_some() {
                return Err("Repeated threshold".into());
            }
        } else if lower.starts_with("rule=") {
            rule.push(w[5..].to_string());
        } else if let Ok(tf) = w.parse::<Timeframe>()
            && rule.is_empty()
        {
            if out.tf.replace(tf).is_some() {
                return Err("Repeated interval".into());
            }
        } else {
            rule.push(w);
        }
    }
    let threshold = out.threshold.unwrap_or(DEFAULT_THRESHOLD_BP);
    if rule.is_empty() {
        WhenSpec::parse(DEFAULT_RULE, threshold)?;
    } else {
        let r = rule.join(" ");
        WhenSpec::parse(&r, threshold)?;
        out.rule = Some(r);
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SharedDistribution {
    pub lo: f64,
    pub hi: f64,
    pub counts: [Vec<usize>; 2],
    /// count/n per group: each nonempty group sums to one.
    pub mass: [Vec<f64>; 2],
    pub n: [usize; 2],
    pub distance: Option<crate::studio_regime::Distance>,
}

#[derive(Debug, Clone)]
pub struct WhenCell {
    pub horizon: usize,
    pub mean_contrast: HacEstimate,
    pub hit_contrast: HacEstimate,
    pub q_mean: f64,
    pub q_hit: f64,
    /// Completed scored outcomes (rule + complement).
    pub raw_n: usize,
    /// Contiguous scored-rule runs; never treated as independent observations.
    pub episodes: usize,
    /// raw n × Var(lag-0 scores) / Var(HAC): variance-equivalent n.
    pub effective_n: Option<f64>,
    /// Contributing sessions: [rule, complement].
    pub sessions: [usize; 2],
    pub probability: Option<f64>,
    pub base_rate: Option<f64>,
    pub lift: Option<f64>,
    pub distributions: SharedDistribution,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForwardBucket {
    pub horizon: usize,
    /// 0..5; all five slots are always present.
    pub slot: usize,
    pub n: usize,
    pub mean: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct WhenReport {
    pub key: u64,
    pub symbol: String,
    pub tf: Timeframe,
    pub spec: WhenSpec,
    /// Rule (1) / complement (0) / unavailable (None) on the expected grid.
    pub mask: ScalarSeries,
    pub cells: Vec<WhenCell>,
    pub buckets: Vec<ForwardBucket>,
    pub bucket_feature: String,
    /// Prior-only quintile edges in force at each grid slot.
    pub edges: Vec<Option<[f64; 4]>>,
    pub bucket_slots: Vec<Option<usize>>,
    pub sessions: usize,
    /// Per expected session and horizon: [rule n, rule hits, rest n, rest hits].
    pub session_rows: Vec<[[usize; 4]; 4]>,
    pub session_breaks: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WhenBootstrap {
    pub key: u64,
    pub draws: usize,
    pub block: usize,
    pub sessions: usize,
    pub probability: [Option<(f64, f64)>; 4],
    pub lift: [Option<(f64, f64)>; 4],
    pub unavailable: [Option<String>; 4],
    /// Excluded replicates per horizon (empty group or zero base rate).
    pub excluded: [usize; 4],
}

fn support(input: &CanvasInput) -> Result<(), String> {
    let sec = crate::security(&input.symbol);
    if !crate::cash_session::is_cash(&sec) {
        return Err("When needs a supported US cash-session calendar; futures/crypto session comparison is unavailable"
            .into());
    }
    crate::cash_session::require_us_session(&sec.ticker, None)?;
    if input.tf != Timeframe::D1 && !crate::cash_session::cash_bar_size(input.tf) {
        return Err("When supports daily and native 1m/5m/15m/30m/1h US cash bars".into());
    }
    Ok(())
}

/// Full-source validation precedes every trim, feature, digest and cache lookup.
/// Includes source digest/clock/provenance/gaps, canonical rule AST, threshold,
/// fixed horizons and the complement definition.
pub fn input_key(input: &CanvasInput, spec: &WhenSpec) -> Result<u64, String> {
    crate::studio_canvas::validate(input).map_err(|e| format!("When: {e}"))?;
    support(input)?;
    if spec.horizons != HORIZONS {
        return Err("When horizons are the fixed [1,3,6,12] family".into());
    }
    let canonical = WhenSpec::parse(&spec.rule(), spec.threshold_bp)?;
    let base = crate::studio_canvas::input_key(std::slice::from_ref(input), &CanvasSpec::default(), None, None, &[])?;
    let mut hash = Fnv64::default();
    hash.write_u64(base);
    hash.write_str(METHOD);
    hash.write_str("complement=all other admitted scored slots");
    hash.write_str(&canonical.canonical());
    hash.write_u64(spec.threshold_bp.to_bits());
    Ok(hash.finish())
}

fn date(ts: i64, tf: Timeframe) -> chrono::NaiveDate {
    if tf.is_intraday() {
        tr_core::ny::civil(ts).date()
    } else {
        chrono::DateTime::from_timestamp(ts, 0).map(|d| d.date_naive()).unwrap_or_default()
    }
}

fn percentile_interval(v: &mut [f64]) -> Option<(f64, f64)> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some((crate::studio_dist::quantile(v, 0.025), crate::studio_dist::quantile(v, 0.975)))
}

/// Shared equal-width bins over the pooled domain; mass per group sums to one.
pub fn shared_distribution(a: &[f64], b: &[f64]) -> SharedDistribution {
    let lo = a.iter().chain(b).copied().fold(f64::INFINITY, f64::min);
    let hi = a.iter().chain(b).copied().fold(f64::NEG_INFINITY, f64::max);
    let (lo, hi) = if !lo.is_finite() {
        (0., 1.)
    } else if hi > lo {
        (lo, hi)
    } else {
        (lo, lo + lo.abs().max(1.) * 1e-6)
    };
    let bin = |v: f64| (((v - lo) / (hi - lo) * BINS as f64) as usize).min(BINS - 1);
    let counts = [a, b].map(|v| {
        let mut c = vec![0; BINS];
        for &x in v {
            c[bin(x)] += 1;
        }
        c
    });
    let n = [a.len(), b.len()];
    let mass = std::array::from_fn(|i| {
        counts[i].iter().map(|c| if n[i] > 0 { *c as f64 / n[i] as f64 } else { 0. }).collect()
    });
    SharedDistribution { lo, hi, counts, mass, n, distance: crate::studio_regime::distance(a, b) }
}

/// Expected session grid: one slot per expected bar open (or session for daily).
struct Grid {
    ts: Vec<i64>,
    bar: Vec<Option<usize>>,
    session: Vec<usize>,
    clock: Vec<usize>,
    /// Outcome/HAC break: `reset` or an intraday session boundary.
    breaks: Vec<bool>,
    /// Indicator reset: first slot, missing slot or predecessor, stitch join.
    reset: Vec<bool>,
    sessions: usize,
    session_breaks: Vec<bool>,
}

fn grid(tf: Timeframe, bars: &[Bar], gaps: &[(i64, i64)]) -> Result<Grid, String> {
    let mut g = Grid {
        ts: vec![],
        bar: vec![],
        session: vec![],
        clock: vec![],
        breaks: vec![],
        reset: vec![],
        sessions: 0,
        session_breaks: vec![],
    };
    let Some((first, last)) = bars.first().zip(bars.last()) else { return Ok(g) };
    let by_key: BTreeMap<i64, usize> = if tf == Timeframe::D1 {
        bars.iter()
            .enumerate()
            .map(|(i, b)| (date(b.ts, tf).and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(), i))
            .collect()
    } else {
        bars.iter().enumerate().map(|(i, b)| (b.ts, i)).collect()
    };
    let mut day = date(first.ts, tf);
    if !crate::history::calendar::is_session(day) {
        day = crate::history::calendar::next_session(day);
    }
    let end_day = date(last.ts, tf);
    let mut previous_ts: Option<i64> = None;
    while day <= end_day {
        let opens = if tf == Timeframe::D1 {
            vec![day.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp()]
        } else {
            crate::intraday::regular_opens(day, tf)
        };
        let session = g.sessions;
        let mut joined = session == 0;
        for (clock, key) in opens.into_iter().enumerate() {
            let bar = by_key.get(&key).copied();
            let ts = bar.map_or(key, |i| bars[i].ts);
            if ts < first.ts || ts > last.ts {
                continue;
            }
            let gap = previous_ts.is_some_and(|p| crate::studio_dist::spans_gap(gaps, p, ts));
            joined |= gap && clock == 0;
            let missing_before = g.bar.last().is_some_and(|b| b.is_none());
            let new_session = g.session.last().is_none_or(|s| *s != session);
            let reset = g.ts.is_empty() || gap || missing_before || bar.is_none();
            g.breaks.push(reset || (tf.is_intraday() && new_session));
            g.reset.push(reset);
            g.ts.push(ts);
            g.bar.push(bar);
            g.session.push(session);
            g.clock.push(clock);
            previous_ts = Some(ts);
        }
        g.session_breaks.push(joined);
        g.sessions += 1;
        if g.ts.len() > MAX_GRID || g.sessions > MAX_SESSIONS {
            return Err("When expected grid exceeds 120,000 slots / 20,000 sessions".into());
        }
        day = crate::history::calendar::next_session(day);
    }
    Ok(g)
}

/// Prior-only expanding Type-7 quintile edges: slot `i` sees only finite
/// features before `i`. Fenwick tree over value ranks (ranks index only).
fn prior_quintiles(feature: &[Option<f64>]) -> Vec<Option<[f64; 4]>> {
    let mut order: Vec<usize> = (0..feature.len()).filter(|&i| feature[i].is_some()).collect();
    order.sort_by(|&a, &b| feature[a].unwrap().total_cmp(&feature[b].unwrap()).then(a.cmp(&b)));
    let sorted: Vec<f64> = order.iter().map(|&i| feature[i].unwrap()).collect();
    let mut rank = vec![0; feature.len()];
    for (k, &i) in order.iter().enumerate() {
        rank[i] = k;
    }
    let n = sorted.len();
    let mut tree = vec![0usize; n + 1];
    let kth = |tree: &[usize], mut k: usize| {
        let mut pos = 0;
        let mut step = n.next_power_of_two();
        while step > 0 {
            if pos + step <= n && tree[pos + step] <= k {
                pos += step;
                k -= tree[pos];
            }
            step /= 2;
        }
        sorted[pos]
    };
    let mut count = 0;
    let mut out = vec![None; feature.len()];
    for i in 0..feature.len() {
        if count >= MIN_EDGE_HISTORY {
            out[i] = Some([0.2, 0.4, 0.6, 0.8].map(|p| {
                let index = (count - 1) as f64 * p;
                let (lo, hi) = (kth(&tree, index.floor() as usize), kth(&tree, index.ceil() as usize));
                lo + (hi - lo) * index.fract()
            }));
        }
        if feature[i].is_some() {
            let mut j = rank[i] + 1;
            while j <= n {
                tree[j] += 1;
                j += 1 << j.trailing_zeros();
            }
            count += 1;
        }
    }
    out
}

/// One report: mask, all four horizons, fixed BY family and buckets. The
/// selected horizon is presentation-only.
pub fn compute(input: &CanvasInput, spec: &WhenSpec) -> Result<WhenReport, String> {
    let key = input_key(input, spec)?;
    let (level, _) = crate::studio_canvas::transform(input, &CanvasSpec::default())?;
    level.receipt.provenance.require_verified("When")?;
    let end = level.ts.last().copied().ok_or("When: no closed bars under the source observation clock")?;
    let admitted = crate::vault::research_bars(&input.bars, input.stitch.as_deref())?;
    let bars: Vec<Bar> = admitted
        .iter()
        .filter(|b| {
            b.ts <= end
                && (input.tf == Timeframe::D1
                    || crate::intraday::regular_opens(date(b.ts, input.tf), input.tf).binary_search(&b.ts).is_ok())
        })
        .cloned()
        .collect();
    if bars.is_empty() {
        return Err("When: no closed regular-session bars".into());
    }
    let gaps = input.stitch.as_ref().map_or(&[][..], |s| s.gaps.as_slice());
    let g = grid(input.tf, &bars, gaps)?;
    let n = g.ts.len();
    // Indicators run over contiguous observed runs; they reset at missing
    // expected bars and stitch joins but carry normal session transitions.
    let mut group = vec![None; n];
    let mut feature = vec![None; n];
    let bucket_operand = spec.bucket_feature().to_string();
    let mut start = 0;
    while start < n {
        if g.bar[start].is_none() {
            start += 1;
            continue;
        }
        let mut stop = start + 1;
        while stop < n && !g.reset[stop] {
            stop += 1;
        }
        let run: Vec<Bar> = (start..stop).map(|i| bars[g.bar[i].unwrap()].clone()).collect();
        let mask = match_mask(&run, &spec.conditions).map_err(|e| e.to_string())?;
        let mut valid = vec![true; run.len()];
        for c in &spec.conditions {
            for operand in [&c.left, &c.right] {
                for (v, x) in valid.iter_mut().zip(bounded_feature(operand, &run).map_err(|e| e.to_string())?) {
                    *v &= x.is_some();
                }
            }
        }
        let f = bounded_feature(&bucket_operand, &run).map_err(|e| e.to_string())?;
        for j in 0..run.len() {
            group[start + j] = valid[j].then_some(mask[j]);
            feature[start + j] = f[j];
        }
        start = stop;
    }
    let edges = prior_quintiles(&feature);
    let bucket_slots: Vec<Option<usize>> = (0..n)
        .map(|i| edges[i].zip(feature[i]).map(|(e, x)| e.iter().filter(|edge| x > **edge).count()))
        .collect();
    let mut rows = vec![[[0usize; 4]; 4]; g.sessions];
    let mut cells = vec![];
    let mut buckets = vec![];
    for (k, h) in HORIZONS.into_iter().enumerate() {
        let mut outcomes: Vec<Option<(usize, bool, f64)>> = vec![None; n];
        let mut values = [vec![], vec![]];
        let mut bucket_values: [Vec<f64>; 5] = Default::default();
        let mut contributing = [BTreeSet::new(), BTreeSet::new()];
        let mut episodes = 0;
        let mut previous_rule = false;
        for i in 0..n {
            let scored = (|| {
                let rule = group[i]?;
                let j = i + h;
                // Every intervening expected bar must exist inside the session/run.
                if j >= n || (i + 1..=j).any(|t| g.breaks[t] || g.bar[t].is_none()) {
                    return None;
                }
                let (c0, c1) = (bars[g.bar[i]?].close, bars[g.bar[j]?].close);
                let r = 1e4 * (c1 / c0 - 1.);
                r.is_finite().then_some((rule, r))
            })();
            let Some((rule, r)) = scored else {
                previous_rule = false;
                continue;
            };
            if rule && (!previous_rule || g.breaks[i]) {
                episodes += 1;
            }
            previous_rule = rule;
            outcomes[i] = Some((g.clock[i], rule, r));
            let slot = usize::from(!rule);
            values[slot].push(r);
            contributing[slot].insert(g.session[i]);
            let row = &mut rows[g.session[i]][k];
            row[2 * slot] += 1;
            row[2 * slot + 1] += usize::from(r > spec.threshold_bp);
            if let Some(b) = bucket_slots[i] {
                bucket_values[b].push(r);
            }
        }
        let lag = hac_lag(h);
        let mut mean = clock_contrast(&outcomes, lag, &g.breaks, MIN_GROUP);
        let hits: Vec<_> =
            outcomes.iter().map(|o| o.map(|(c, g, r)| (c, g, f64::from(r > spec.threshold_bp)))).collect();
        let mut hit = clock_contrast(&hits, lag, &g.breaks, MIN_GROUP);
        let sessions = contributing.each_ref().map(|s| s.len());
        if sessions.iter().any(|s| *s < MIN_SESSIONS) {
            for e in [&mut mean, &mut hit] {
                withhold(e, format!("LOW N: need {MIN_SESSIONS} contributing sessions in each group"));
            }
        }
        let raw_n = values[0].len() + values[1].len();
        let iid = crate::studio_inference::clock_contrast(&outcomes, 0, &g.breaks, 1).variance;
        let effective_n = iid
            .zip(mean.variance)
            .and_then(|(a, b)| (b > 0. && a > 0.).then_some(raw_n as f64 * a / b))
            .filter(|_| mean.p.is_some());
        let hit_rule = values[0].iter().filter(|r| **r > spec.threshold_bp).count();
        let hit_all = hit_rule + values[1].iter().filter(|r| **r > spec.threshold_bp).count();
        let probability = (!values[0].is_empty()).then(|| hit_rule as f64 / values[0].len() as f64);
        let base_rate = (raw_n > 0).then(|| hit_all as f64 / raw_n as f64);
        let lift = probability.zip(base_rate).and_then(|(p, b)| (b > 0.).then_some(p / b));
        cells.push(WhenCell {
            horizon: h,
            mean_contrast: mean,
            hit_contrast: hit,
            q_mean: 1.,
            q_hit: 1.,
            raw_n,
            episodes,
            effective_n,
            sessions,
            probability,
            base_rate,
            lift,
            distributions: shared_distribution(&values[0], &values[1]),
        });
        for (slot, v) in bucket_values.iter().enumerate() {
            buckets.push(ForwardBucket {
                horizon: h,
                slot,
                n: v.len(),
                mean: (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64),
            });
        }
    }
    let q = fixed_family_q(&cells);
    for (i, c) in cells.iter_mut().enumerate() {
        c.q_mean = q[2 * i];
        c.q_hit = q[2 * i + 1];
    }
    let mask = ScalarSeries {
        ts: g.ts,
        values: group.into_iter().map(|g| g.map(f64::from)).collect(),
        break_before: g.breaks,
        unit: SeriesUnit::Native("rule (1) / complement (0)".into()),
        receipt: level.receipt,
    };
    Ok(WhenReport {
        key,
        symbol: input.symbol.clone(),
        tf: input.tf,
        spec: spec.clone(),
        mask,
        cells,
        buckets,
        bucket_feature: bucket_operand,
        edges,
        bucket_slots,
        sessions: g.sessions,
        session_rows: rows,
        session_breaks: g.session_breaks,
    })
}

fn withhold(e: &mut HacEstimate, reason: String) {
    e.p = None;
    e.interval = None;
    e.se = None;
    e.variance = None;
    if e.unavailable.is_none() {
        e.unavailable = Some(reason);
    }
}

/// BY over the fixed, predeclared family of eight (4 mean + 4 hit). Unavailable
/// members stay in the family at p = 1; the family never shrinks under selection.
pub fn fixed_family_q(cells: &[WhenCell]) -> Vec<f64> {
    let mut ps: Vec<Option<f64>> = cells.iter().flat_map(|c| [c.mean_contrast.p, c.hit_contrast.p]).collect();
    ps.resize(2 * HORIZONS.len(), None);
    crate::nulls::Family::by("when.fixed8", &ps).q
}

impl WhenReport {
    /// Second-stage stationary bootstrap over whole expected-session rows. The
    /// same resampled sessions serve all four horizons. Deterministic seed.
    pub fn bootstrap(&self) -> WhenBootstrap {
        let n = self.session_rows.len();
        let mut out = WhenBootstrap {
            key: self.key,
            draws: DRAWS,
            block: (n.max(1) as f64).cbrt().ceil() as usize,
            sessions: n,
            probability: [None; 4],
            lift: [None; 4],
            excluded: [0; 4],
            unavailable: std::array::from_fn(|i| {
                let c = &self.cells[i];
                let low = c.mean_contrast.n < MIN_GROUP
                    || c.mean_contrast.complement_n < MIN_GROUP
                    || c.sessions.iter().any(|s| *s < MIN_SESSIONS);
                low.then(|| format!("LOW N: need {MIN_GROUP}/group and {MIN_SESSIONS} contributing sessions/group"))
            }),
        };
        if n == 0 || n > MAX_SESSIONS {
            out.unavailable.fill(Some("Session block input outside bounds".into()));
            return out;
        }
        let mut p: [Vec<f64>; 4] = Default::default();
        let mut lift: [Vec<f64>; 4] = Default::default();
        let mut excluded = [0usize; 4];
        crate::studio_inference::stationary_bootstrap(n, DRAWS, self.key, &self.session_breaks, |idx| {
            let mut sums = [[0usize; 4]; 4];
            for &s in idx {
                for (a, b) in sums.iter_mut().zip(self.session_rows[s]) {
                    for j in 0..4 {
                        a[j] += b[j];
                    }
                }
            }
            for i in 0..4 {
                let a = sums[i];
                if a[0] == 0 || a[2] == 0 || a[1] + a[3] == 0 {
                    excluded[i] += 1;
                    continue;
                }
                let prob = a[1] as f64 / a[0] as f64;
                let base = (a[1] + a[3]) as f64 / (a[0] + a[2]) as f64;
                p[i].push(prob);
                lift[i].push(prob / base);
            }
        });
        out.excluded = excluded;
        for i in 0..4 {
            if out.unavailable[i].is_none() && excluded[i] > MAX_EXCLUDED {
                out.unavailable[i] = Some(format!(
                    "{} of {DRAWS} session replicates lacked a group or had zero base rate (limit {MAX_EXCLUDED})",
                    excluded[i]
                ));
            }
            if out.unavailable[i].is_none() {
                out.probability[i] = percentile_interval(&mut p[i]);
                out.lift[i] = percentile_interval(&mut lift[i]);
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct SubsetReport {
    pub key: u64,
    pub selection: (i64, i64),
    pub selected_dates: Option<(i64, i64)>,
    pub rest_dates: Option<(i64, i64)>,
    pub distribution: SharedDistribution,
    pub distance_interval: Option<(f64, f64)>,
    pub unavailable: Option<String>,
    pub receipt: SeriesReceipt,
    pub unit: SeriesUnit,
    pub sessions: [usize; 2],
    pub draws: usize,
    /// Replicates whose resampled sessions contained no member of a group.
    pub excluded: usize,
}

/// Selected `[start,end)` versus the rest of the same admitted scalar series,
/// on shared bins. ECDF distance is descriptive; its uncertainty comes from a
/// whole-session stationary bootstrap (never iid KS tables).
pub fn subset(line: &ScalarSeries, tf: Timeframe, selection: (i64, i64), key: u64) -> Result<SubsetReport, String> {
    line.receipt.provenance.require_verified("Studio subset")?;
    if line.ts.len() != line.values.len() || line.ts.len() > MAX_GRID || selection.0 >= selection.1 {
        return Err("Invalid subset input or empty span".into());
    }
    let mut groups = [vec![], vec![]];
    let mut dates: [Vec<i64>; 2] = [vec![], vec![]];
    let mut session_of: BTreeMap<chrono::NaiveDate, usize> = BTreeMap::new();
    let mut session_breaks = vec![];
    let mut entries = vec![];
    let mut sessions = [BTreeSet::new(), BTreeSet::new()];
    let mut previous: Option<i64> = None;
    for (&t, v) in line.ts.iter().zip(&line.values) {
        let Some(x) = *v else { continue };
        if !x.is_finite() {
            return Err("Nonfinite subset scalar".into());
        }
        let d = date(t, tf);
        let s = match session_of.get(&d) {
            Some(s) => *s,
            None => {
                let s = session_of.len();
                session_of.insert(d, s);
                let joined = previous.is_none_or(|p| crate::studio_dist::spans_gap(&line.receipt.gaps, p, t));
                session_breaks.push(joined);
                s
            }
        };
        previous = Some(t);
        let g = usize::from(!(selection.0 <= t && t < selection.1));
        groups[g].push(x);
        dates[g].push(t);
        sessions[g].insert(s);
        entries.push((x, g, s));
    }
    let span = |d: &[i64]| d.first().zip(d.last()).map(|(a, b)| (*a, *b));
    let mut out = SubsetReport {
        key,
        selection,
        selected_dates: span(&dates[0]),
        rest_dates: span(&dates[1]),
        distribution: shared_distribution(&groups[0], &groups[1]),
        distance_interval: None,
        unavailable: None,
        receipt: line.receipt.clone(),
        unit: line.unit.clone(),
        sessions: sessions.each_ref().map(|s| s.len()),
        draws: DRAWS,
        excluded: 0,
    };
    if groups.iter().any(|g| g.len() < MIN_GROUP) || out.sessions.iter().any(|s| *s < MIN_SESSIONS) {
        out.unavailable = Some(format!(
            "LOW N: need {MIN_GROUP} observations and {MIN_SESSIONS} sessions per group for the session-block interval"
        ));
        return Ok(out);
    }
    entries.sort_by(|a, b| a.0.total_cmp(&b.0));
    let n = session_of.len();
    // Structure-of-arrays sweep: session, group and tie-group ends (ECDF
    // differences are only evaluated after a complete tie group).
    let session: Vec<u32> = entries.iter().map(|e| e.2 as u32).collect();
    let group: Vec<u8> = entries.iter().map(|e| e.1 as u8).collect();
    let tie_end: Vec<bool> =
        (0..entries.len()).map(|i| i + 1 == entries.len() || entries[i + 1].0 != entries[i].0).collect();
    // Prefix counts per group over sessions: a run [a, a+len) adds
    // cumulative[a+len] − cumulative[a] observations to each group.
    let mut cumulative = vec![[0u64; 2]; n + 1];
    for e in &entries {
        cumulative[e.2 + 1][e.1] += 1;
    }
    for s in 0..n {
        let previous = cumulative[s];
        cumulative[s + 1][0] += previous[0];
        cumulative[s + 1][1] += previous[1];
    }
    let mut samples = Vec::with_capacity(DRAWS);
    let mut diff = vec![0i32; n + 1];
    let mut weights = vec![0i64; n];
    let mut excluded = 0;
    let block = (n.max(1) as f64).cbrt().ceil() as usize;
    crate::studio_inference::stationary_bootstrap_runs(n, DRAWS, key, &session_breaks, block, |runs| {
        diff.fill(0);
        let mut total = [0u64; 2];
        for &(a, len) in runs {
            diff[a] += 1;
            diff[a + len] -= 1;
            for g in 0..2 {
                total[g] += cumulative[a + len][g] - cumulative[a][g];
            }
        }
        let mut level = 0i64;
        for (w, d) in weights.iter_mut().zip(&diff) {
            level += i64::from(*d);
            *w = level;
        }
        if total.contains(&0) {
            excluded += 1;
            return;
        }
        // Exact integer sweep: c0·T1 − c1·T0, scaled by 1/(T0·T1) at the end.
        let scale = [total[1] as i64, -(total[0] as i64)];
        let mut running = 0i64;
        let mut max = 0i64;
        let mut min = 0i64;
        for i in 0..session.len() {
            running += weights[session[i] as usize] * scale[group[i] as usize];
            if tie_end[i] {
                max = max.max(running);
                min = min.min(running);
            }
        }
        samples.push(max.max(-min) as f64 / (total[0] as f64 * total[1] as f64));
    });
    out.excluded = excluded;
    if excluded > MAX_EXCLUDED {
        out.unavailable = Some(format!(
            "{excluded} of {DRAWS} session replicates lacked a group (limit {MAX_EXCLUDED}); interval withheld"
        ));
    } else {
        out.distance_interval = percentile_interval(&mut samples);
    }
    Ok(out)
}

/// Selected/rest one-bar log returns (bp) from an accepted Distribution report.
/// Only adjacent expected bars inside a session count; joins break.
pub fn subset_from_dist(r: &crate::studio_dist::Report, span: (i64, i64)) -> Result<SubsetReport, String> {
    let sec = crate::security(&r.symbol);
    if !crate::cash_session::is_cash(&sec) {
        return Err("Selected-range session comparison needs a supported US cash calendar".into());
    }
    crate::cash_session::require_us_session(&sec.ticker, None)?;
    if r.timeframe != Timeframe::D1 && !crate::cash_session::cash_bar_size(r.timeframe) {
        return Err("Selected-range comparison: unsupported session interval".into());
    }
    let mut hash = Fnv64::default();
    hash.write_u64(r.bars_seed);
    hash.write_str("subset.v1|one-bar log return bp|selected [start,end) vs rest|32 shared bins|session499");
    hash.write_str(&format!("{span:?}|{:?}|{}", r.observed_at, r.end));
    let ts: Vec<i64> = r.underwater.iter().map(|b| b.ts).collect();
    let returns: BTreeMap<i64, f64> = r.returns.iter().map(|p| (p.ts, p.value * 1e4)).collect();
    let mut breaks = vec![true; ts.len()];
    let mut values = vec![None; ts.len()];
    for i in 1..ts.len() {
        let (a, b) = (ts[i - 1], ts[i]);
        let adjacent = if r.timeframe == Timeframe::D1 {
            crate::history::calendar::next_session(date(a, r.timeframe)) == date(b, r.timeframe)
        } else {
            b - a == r.timeframe.seconds() && date(a, r.timeframe) == date(b, r.timeframe)
        };
        breaks[i] = !adjacent || crate::studio_dist::spans_gap(&r.gaps, a, b);
        if !breaks[i] {
            values[i] = returns.get(&b).copied();
        }
    }
    let line = ScalarSeries {
        ts,
        values,
        break_before: breaks,
        unit: SeriesUnit::Native("one-bar log return (bp)".into()),
        receipt: SeriesReceipt {
            source: r.source.clone(),
            provenance: r.provenance,
            observed_at: r.observed_at,
            closed_thru: r.end,
            input_digest: r.bars_seed,
            gaps: r.gaps.clone(),
        },
    };
    subset(&line, r.timeframe, span, hash.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::studio_inference::scalar_score_variance;
    use std::sync::Arc;
    use tr_core::Provenance;

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    /// Fixture only: verified-tagged synthetic AAPL 5m sessions.
    fn sessions(days: usize, f: impl Fn(usize) -> f64) -> Vec<Bar> {
        let mut out = vec![];
        let mut day = chrono::NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
        let mut k = 0;
        for _ in 0..days {
            for ts in crate::intraday::regular_opens(day, Timeframe::M5) {
                let c = f(k);
                out.push(Bar {
                    ts,
                    open: c,
                    high: c * 1.001,
                    low: c * 0.999,
                    close: c,
                    volume: 1000.,
                    provenance: Provenance::Real,
                });
                k += 1;
            }
            day = crate::history::calendar::next_session(day);
        }
        out
    }
    fn input(bars: Vec<Bar>) -> CanvasInput {
        let observed = bars.last().unwrap().ts + 3600;
        CanvasInput {
            symbol: "AAPL".into(),
            tf: Timeframe::M5,
            bars: Arc::new(bars),
            source: "fixture".into(),
            observed_at: Some(observed),
            stitch: None,
        }
    }
    fn wave(k: usize) -> f64 {
        100. + 3. * ((k as f64) * 0.37).sin() + 2. * ((k as f64) * 0.051).cos() + (k % 7) as f64 * 0.05
    }

    #[test]
    fn when_clock_contrast_hand_vector() {
        let y = [3., 5., 1., 3., 13., 15., 11., 13.];
        let g = [true, true, false, false, true, true, false, false];
        let grid: Vec<_> = (0..8).map(|i| Some((i / 4, g[i], y[i]))).collect();
        let e = clock_contrast(&grid, 1, &[false; 8], 1);
        near(e.estimate.unwrap(), 2.);
        near(e.variance.unwrap(), 7. / 16.);
        near(e.se.unwrap(), 7f64.sqrt() / 4.);
        assert_eq!((e.n, e.complement_n, e.lag), (4, 4, 1));
    }

    #[test]
    fn when_hac_score_reference() {
        let s = [-0.25, 0.25, 0.25, -0.25, -0.25, 0.25, 0.25, -0.25];
        near(scalar_score_variance(&s, 1, &[false; 8]), 7. / 16.);
        near(scalar_score_variance(&s, 0, &[]), 0.5);
        // Brute-force Bartlett reference at L=3 with a break before index 4.
        let mut breaks = [false; 8];
        breaks[4] = true;
        let run = |i: usize| usize::from(i >= 4);
        let mut v = 0.;
        for i in 0..8usize {
            for j in 0..8usize {
                let l = i.abs_diff(j);
                if l <= 3 && run(i) == run(j) {
                    v += (1. - l as f64 / 4.) * s[i] * s[j];
                }
            }
        }
        near(scalar_score_variance(&s, 3, &breaks), v);
        // Unbroken products across index 4 change the answer.
        assert!((scalar_score_variance(&s, 3, &[false; 8]) - v).abs() > 1e-6);
    }

    #[test]
    fn when_no_future_in_rule_or_edges() {
        let bars = sessions(40, wave);
        let spec = WhenSpec::parse("RSI(14)<45 AND close>SMA(20)", 5.).unwrap();
        let full = compute(&input(bars.clone()), &spec).unwrap();
        let cut = bars.len() - 400;
        let prefix = compute(&input(bars[..cut].to_vec()), &spec).unwrap();
        let m = prefix.mask.ts.len();
        assert!(m > 2000);
        assert_eq!(&full.mask.ts[..m], &prefix.mask.ts[..]);
        assert_eq!(&full.mask.values[..m], &prefix.mask.values[..], "rule mask is prefix-invariant");
        assert_eq!(&full.edges[..m], &prefix.edges[..], "quintile edges use prior values only");
        assert_eq!(&full.bucket_slots[..m], &prefix.bucket_slots[..]);
        assert!(full.edges.iter().flatten().count() > 1000);
        // Edges at slot i equal type-7 quintiles of feature values strictly before i.
        let f = bounded_feature("RSI(14)", &bars).unwrap();
        let i = 900;
        assert_eq!(full.mask.ts[i], bars[i].ts);
        let mut prior: Vec<f64> = f[..i].iter().flatten().copied().collect();
        prior.sort_by(f64::total_cmp);
        let e = full.edges[i].unwrap();
        for (k, p) in [0.2, 0.4, 0.6, 0.8].into_iter().enumerate() {
            near(e[k], crate::studio_dist::quantile(&prior, p));
        }
        // All five bucket slots are present for every horizon.
        for h in HORIZONS {
            let slots: Vec<_> = full.buckets.iter().filter(|b| b.horizon == h).map(|b| b.slot).collect();
            assert_eq!(slots, [0, 1, 2, 3, 4]);
        }
    }

    #[test]
    fn when_family_is_eight_under_selection() {
        let bars = sessions(40, wave);
        let r = compute(&input(bars), &WhenSpec::parse("RSI(14)<40", 0.).unwrap()).unwrap();
        assert_eq!(r.cells.len(), 4);
        let ps: Vec<_> = r.cells.iter().flat_map(|c| [c.mean_contrast.p, c.hit_contrast.p]).collect();
        let q = crate::nulls::Family::by("x", &ps).q;
        assert_eq!(q.len(), 8);
        for (i, c) in r.cells.iter().enumerate() {
            near(c.q_mean, q[2 * i]);
            near(c.q_hit, q[2 * i + 1]);
        }
        // Withholding members (or showing one horizon) keeps m = 8 with p = 1.
        let mut cells = r.cells.clone();
        cells[0].mean_contrast.p = None;
        let q2 = fixed_family_q(&cells);
        assert_eq!(q2.len(), 8);
        assert_eq!(fixed_family_q(&cells[..1]).len(), 8);
        let solo = crate::nulls::Family::by("x", &[Some(0.01)]).q[0];
        let mut one = r.cells[..1].to_vec();
        one[0].mean_contrast.p = Some(0.01);
        one[0].hit_contrast.p = None;
        assert!(fixed_family_q(&one)[0] > solo, "selection cannot shrink the family");
    }

    #[test]
    fn when_sessions_and_gaps_break_outcomes() {
        let mut bars = sessions(25, wave);
        // Remove one expected bar mid-session in day 3.
        let gone = 78 * 3 + 40;
        let removed = bars.remove(gone);
        let r = compute(&input(bars.clone()), &WhenSpec::parse("close>0", 0.).unwrap()).unwrap();
        let ts = &r.mask.ts;
        let at = ts.binary_search(&removed.ts).unwrap();
        assert_eq!(r.mask.values[at], None, "missing expected bar stays missing");
        assert!(r.mask.break_before[at] && r.mask.break_before[at + 1]);
        let h1 = &r.cells[0];
        // h=1: last bar of each session and the bar before/at the hole are unscored.
        let sessions = 25;
        assert_eq!(h1.raw_n, 78 * sessions - 1 - sessions - 1);
        let h12 = &r.cells[3];
        assert_eq!(h12.raw_n, (78 - 12) * sessions - 12 - 1 + 0);
        // A stitch join inside a session also breaks outcomes.
        let mut joined = input(bars.clone());
        let (a, b) = (bars[100].ts, bars[101].ts);
        joined.stitch = Some(Arc::new(crate::vault::StitchInfo { gaps: vec![(a, b)], ..Default::default() }));
        let j = compute(&joined, &WhenSpec::parse("close>0", 0.).unwrap()).unwrap();
        assert_eq!(j.cells[0].raw_n, h1.raw_n - 1);
        assert!(j.mask.break_before[j.mask.ts.binary_search(&b).unwrap()]);
        assert_ne!(j.key, r.key, "gaps enter the key");
    }

    #[test]
    fn when_episode_count_is_not_effective_n() {
        // Long persistent rule runs: many observations, few episodes.
        let bars = sessions(30, |k| 100. + 10. * ((k as f64) * 0.02).sin() + (k % 5) as f64 * 0.03);
        let r = compute(&input(bars), &WhenSpec::parse("close>SMA(50)", 0.).unwrap()).unwrap();
        let c = &r.cells[2];
        assert!(c.episodes > 0 && c.episodes * 5 < c.mean_contrast.n, "{} vs {}", c.episodes, c.mean_contrast.n);
        assert_ne!(Some(c.episodes as f64), c.effective_n);
        assert!(c.raw_n >= c.mean_contrast.n + c.mean_contrast.complement_n);
        if let Some(e) = c.effective_n {
            assert!((e - c.episodes as f64).abs() > 1.);
        }
    }

    #[test]
    fn when_daily_admission_refuses_sample_and_crypto() {
        let mut bars = sessions(25, wave);
        bars[10].provenance = Provenance::Sample;
        assert!(compute(&input(bars), &WhenSpec::default()).is_err(), "sample bars refused before trim");
        let mut crypto = input(sessions(25, wave));
        crypto.symbol = "BTCUSD".into();
        assert!(compute(&crypto, &WhenSpec::default()).unwrap_err().contains("cash"));
    }

    #[test]
    fn when_key_tracks_corrections_rule_ast_and_threshold() {
        let bars = sessions(25, wave);
        let spec = WhenSpec::parse("RSI(14)<30 AND close>SMA(20)", 10.).unwrap();
        let k = input_key(&input(bars.clone()), &spec).unwrap();
        let swapped = WhenSpec::parse("close>SMA(20) AND RSI(14)<30", 10.).unwrap();
        assert_eq!(k, input_key(&input(bars.clone()), &swapped).unwrap(), "canonical AST");
        assert_ne!(k, input_key(&input(bars.clone()), &WhenSpec::parse("RSI(14)<30", 10.).unwrap()).unwrap());
        assert_ne!(k, input_key(&input(bars.clone()), &WhenSpec { threshold_bp: 11., ..spec.clone() }).unwrap());
        let mut corrected = bars.clone();
        corrected[5].close *= 1.0001;
        assert_ne!(k, input_key(&input(corrected), &spec).unwrap(), "old-bar correction invalidates");
    }

    #[test]
    fn when_bootstrap_is_seeded_and_session_based() {
        let bars = sessions(40, wave);
        let r = compute(&input(bars), &WhenSpec::parse("RSI(14)<45", 0.).unwrap()).unwrap();
        let a = r.bootstrap();
        assert_eq!(a, r.bootstrap(), "deterministic");
        assert_eq!((a.draws, a.sessions, a.block), (DRAWS, 40, 4));
        let (lo, hi) = a.probability[0].expect("available");
        let p = r.cells[0].probability.unwrap();
        assert!(lo <= p && p <= hi);
    }

    #[test]
    fn parse_command_forms() {
        let c = parse_command("5m RSI(14)<30").unwrap();
        assert_eq!((c.tf, c.rule.as_deref()), (Some(Timeframe::M5), Some("RSI(14)<30")));
        let c = parse_command("h=6 threshold=10bp").unwrap();
        assert_eq!((c.horizon, c.threshold, c.rule), (Some(6), Some(10.), None));
        let c = parse_command("rule=\"RSI(14)<30 AND close>SMA(50)\" h=12").unwrap();
        assert_eq!(c.rule.as_deref(), Some("RSI(14)<30 AND close>SMA(50)"));
        assert!(parse_command("h=5").is_err());
        assert!(parse_command("FOO(3)>1").is_err());
        assert!(parse_command("rule=\"RSI(14)<30").is_err());
    }

    #[test]
    fn subset_block_runs_respect_breaks_and_match_brute_force_ecdf() {
        use crate::studio_inference::{stationary_bootstrap_blocks, stationary_bootstrap_runs};
        let n = 300;
        let mut breaks = vec![false; n];
        breaks[0] = true;
        breaks[120] = true;
        let mut lengths = vec![];
        stationary_bootstrap_runs(n, 200, 9, &breaks, 6, |runs| {
            assert_eq!(runs.iter().map(|r| r.1).sum::<usize>(), n);
            for &(a, len) in runs {
                assert!(a + len <= n);
                assert!(!(a + 1..a + len).any(|i| breaks[i]), "a run never crosses a break");
                lengths.push(len);
            }
        });
        let mean = lengths.iter().sum::<usize>() as f64 / lengths.len() as f64;
        assert!((3.5..=6.5).contains(&mean), "mean block {mean}");
        // Expanded indices equal the runs, element for element.
        let mut expanded = vec![];
        stationary_bootstrap_runs(n, 3, 9, &breaks, 6, |runs| {
            expanded.push(runs.iter().flat_map(|&(a, l)| a..a + l).collect::<Vec<_>>())
        });
        let mut direct = vec![];
        stationary_bootstrap_blocks(n, 3, 9, &breaks, 6, |ix| direct.push(ix.to_vec()));
        assert_eq!(expanded, direct);
        // The integer subset sweep equals a brute-force replicate ECDF distance.
        let bars = sessions(45, wave);
        let observed = bars.last().unwrap().ts + 3600;
        let r = crate::studio_dist::compute(
            "AAPL",
            &bars,
            Timeframe::M5,
            crate::studio_dist::Window::All,
            "fixture",
            Some(observed),
        )
        .unwrap();
        let span = (bars[78 * 10].ts, bars[78 * 30].ts);
        let s = subset_from_dist(&r, span).unwrap();
        let (mut x, mut g, mut sess) = (vec![], vec![], vec![]);
        let mut days: Vec<chrono::NaiveDate> = vec![];
        // In-session one-bar returns only: the previous expected bar exists.
        let opens: BTreeSet<i64> = bars.iter().map(|b| b.ts).collect();
        let in_session = |t: i64| opens.contains(&(t - 300)) && date(t - 300, Timeframe::M5) == date(t, Timeframe::M5);
        for p in r.returns.iter().filter(|p| in_session(p.ts)) {
            let d = date(p.ts, Timeframe::M5);
            if days.last() != Some(&d) {
                days.push(d);
            }
            x.push(p.value * 1e4);
            g.push(!(span.0 <= p.ts && p.ts < span.1));
            sess.push(days.len() - 1);
        }
        let mut brute = vec![];
        let k = days.len();
        let first_only: Vec<bool> = (0..k).map(|i| i == 0).collect();
        let block = (k as f64).cbrt().ceil() as usize;
        stationary_bootstrap_blocks(k, DRAWS, s.key, &first_only, block, |ix| {
            let (mut a, mut b) = (vec![], vec![]);
            for &si in ix {
                for i in (0..x.len()).filter(|i| sess[*i] == si) {
                    if g[i] { b.push(x[i]) } else { a.push(x[i]) }
                }
            }
            brute.push(crate::studio_regime::distance(&a, &b).unwrap().d);
        });
        assert_eq!(s.excluded, 0);
        assert_eq!(brute.len(), DRAWS);
        brute.sort_by(f64::total_cmp);
        let expected = (crate::studio_dist::quantile(&brute, 0.025), crate::studio_dist::quantile(&brute, 0.975));
        let got = s.distance_interval.unwrap();
        assert!((got.0 - expected.0).abs() < 1e-12 && (got.1 - expected.1).abs() < 1e-12, "{got:?} vs {expected:?}");
    }
    #[test]
    fn subset_shared_bins_preserve_mass() {
        let a: Vec<f64> = (0..50).map(|i| i as f64 * 0.7 - 3.).collect();
        let b: Vec<f64> = (0..80).map(|i| (i as f64 * 1.3).sin() * 20.).collect();
        let d = shared_distribution(&a, &b);
        for g in 0..2 {
            near(d.mass[g].iter().sum::<f64>(), 1.);
            assert_eq!(d.counts[g].iter().sum::<usize>(), d.n[g]);
            assert_eq!(d.mass[g].len(), BINS);
        }
        assert_eq!(d.n, [50, 80]);
        assert!(d.distance.is_some());
        let empty = shared_distribution(&a, &[]);
        near(empty.mass[1].iter().sum::<f64>(), 0.);
        // Real subset over a DIST report: selected/rest disjoint and complete.
        let bars = sessions(45, wave);
        let observed = bars.last().unwrap().ts + 3600;
        let all = crate::studio_dist::Window::All;
        let r = crate::studio_dist::compute("AAPL", &bars, Timeframe::M5, all, "fixture", Some(observed)).unwrap();
        let mid = bars[78 * 20].ts;
        let span = (mid, bars[78 * 42].ts);
        let s = subset_from_dist(&r, span).unwrap();
        let n = s.distribution.n;
        assert_eq!(n[0] + n[1], 45 * 77);
        assert!(s.selected_dates.unwrap().0 >= span.0 && s.selected_dates.unwrap().1 < span.1);
        assert!(s.sessions[0] >= 20 && s.sessions[1] >= 20);
        let (lo, hi) = s.distance_interval.expect("session-block interval");
        assert!(lo <= hi && hi <= 1.);
        let again = subset_from_dist(&r, span).unwrap();
        assert_eq!(again.distance_interval, s.distance_interval);
        assert_ne!(subset_from_dist(&r, (span.0, span.1 + 300)).unwrap().key, s.key);
    }
}
