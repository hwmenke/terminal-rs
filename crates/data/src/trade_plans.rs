//! Persistent quote-only entry → stop/target brackets. Observations are never fills.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::price_alerts::{self, AlertFailure, PriceSide, PriceTick};

pub const MAX_ACTIVE_PLANS: usize = 32;
pub const MAX_PLANS: usize = 200;
pub const MAX_EVENTS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanStatus {
    Armed,
    Triggered,
    TargetHit,
    Stopped,
    Invalidated,
    Cancelled,
}
impl PlanStatus {
    pub fn active(self) -> bool {
        matches!(self, Self::Armed | Self::Triggered)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Armed => "ARMED",
            Self::Triggered => "TRIGGERED",
            Self::TargetHit => "TARGET",
            Self::Stopped => "STOPPED",
            Self::Invalidated => "INVALIDATED",
            Self::Cancelled => "CANCELLED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanEventKind {
    Entry,
    OneR,
    Breakeven,
    Target,
    Stop,
    Invalidated,
}
impl PlanEventKind {
    fn key(self) -> &'static str {
        match self {
            Self::Entry => "entry",
            Self::OneR => "one_r",
            Self::Breakeven => "breakeven",
            Self::Target => "target",
            Self::Stop => "stop",
            Self::Invalidated => "invalidated",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TrailKind {
    R,
    Percent,
    Price,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrailSpec {
    pub kind: TrailKind,
    pub value: f64,
    #[serde(default)]
    pub source: String,
}
impl TrailSpec {
    pub fn distance(&self, plan: &TradePlan, high_water: f64) -> f64 {
        match self.kind {
            TrailKind::R => self.value * (plan.entry - plan.initial_stop()).abs(),
            TrailKind::Percent => self.value / 100.0 * high_water,
            TrailKind::Price => self.value,
        }
    }
    fn validate(&self, entry: f64, long: bool) -> Result<(), String> {
        if !self.value.is_finite() || self.value <= 0.0 {
            return Err("trail must be finite and positive".into());
        }
        match self.kind {
            TrailKind::R if self.value > 20.0 => Err("R trail must be at most 20R".into()),
            TrailKind::Percent if self.value >= 50.0 => Err("percent trail must be below 50%".into()),
            TrailKind::Price if long && self.value >= entry => Err("price trail must be below entry for a long".into()),
            _ => Ok(()),
        }
    }
}
impl std::fmt::Display for TrailSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}",
            self.value,
            match self.kind {
                TrailKind::R => "R",
                TrailKind::Percent => "%",
                TrailKind::Price => "",
            }
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanChange {
    Trail(Option<TrailSpec>),
    Breakeven(Option<f64>),
    MoveStop(f64),
}

fn validate_breakeven(value: Option<f64>) -> Result<(), String> {
    if value.is_some_and(|r| !r.is_finite() || r <= 0.0 || r > 20.0) {
        return Err("breakeven R must be finite and in (0, 20]".into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanDraft {
    pub symbol: String,
    pub entry: f64,
    pub stop: f64,
    pub target: Option<f64>,
    pub dollar_risk: f64,
    pub shares: f64,
    pub stop_source: String,
    #[serde(default)]
    pub breakeven_at_r: Option<f64>,
    #[serde(default)]
    pub trail: Option<TrailSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradePlan {
    pub id: String,
    pub symbol: String,
    pub long: bool,
    pub entry: f64,
    pub stop: f64,
    pub target: Option<f64>,
    pub dollar_risk: f64,
    pub shares: f64,
    pub stop_source: String,
    #[serde(default)]
    pub breakeven_at_r: Option<f64>,
    #[serde(default)]
    pub trail: Option<TrailSpec>,
    pub status: PlanStatus,
    pub entry_side: Option<PriceSide>,
    pub last_price: Option<f64>,
    pub last_tick_ts: Option<i64>,
    pub one_r_seen: bool,
    pub created_at: i64,
    pub triggered_at: Option<i64>,
    pub triggered_price: Option<f64>,
    pub closed_at: Option<i64>,
    pub closed_price: Option<f64>,
    #[serde(default)]
    pub initial_stop: Option<f64>,
    #[serde(default)]
    pub high_water: Option<f64>,
    #[serde(default)]
    pub breakeven_seen: bool,
    #[serde(default)]
    pub stop_moved_at: Option<i64>,
    #[serde(default)]
    pub stop_manual: bool,
}
impl TradePlan {
    pub fn initial_stop(&self) -> f64 {
        self.initial_stop.unwrap_or(self.stop)
    }
    fn tighter(&self, candidate: f64, than: f64) -> bool {
        if self.long { candidate > than } else { candidate < than }
    }
    fn at_breakeven(&self) -> bool {
        self.stop == self.entry || self.tighter(self.stop, self.entry)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanEvent {
    pub id: String,
    pub plan_id: String,
    pub symbol: String,
    pub kind: PlanEventKind,
    pub message: String,
    pub price: f64,
    pub created_at: i64,
    pub seen: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Store {
    pub version: u32,
    pub plans: Vec<TradePlan>,
    pub events: Vec<PlanEvent>,
}
impl Default for Store {
    fn default() -> Self {
        Self { version: 1, plans: Vec::new(), events: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanBatch {
    pub plans: Vec<TradePlan>,
    pub events: Vec<PlanEvent>,
    pub failures: Vec<AlertFailure>,
    pub fetched_at: i64,
    pub stale: bool,
    pub sample: bool,
}
impl PlanBatch {
    pub fn empty(now: i64) -> Self {
        Store::default().snapshot(now, false, false, Vec::new())
    }
    pub fn unread(&self) -> usize {
        self.events.iter().filter(|e| !e.seen).count()
    }
}

pub fn store_path(data_dir: &Path) -> PathBuf {
    data_dir.join("trade_plans.json")
}

impl Store {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }
    pub fn save(&self, path: &Path) {
        if path.as_os_str().is_empty() {
            return;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, bytes);
        }
    }
    pub fn arm(&mut self, draft: PlanDraft, now: i64) -> Result<TradePlan, String> {
        let security = tr_core::Security::parse(&draft.symbol).ok_or("invalid security")?;
        let symbol = security.key();
        for (label, value) in [("entry", draft.entry), ("stop", draft.stop)] {
            if !value.is_finite() || value <= 0.0 {
                return Err(format!("{label} must be finite and positive"));
            }
        }
        // Omitted command risk is explicitly an unsized plan (zero risk and shares).
        if !draft.dollar_risk.is_finite()
            || draft.dollar_risk < 0.0
            || (draft.dollar_risk == 0.0 && draft.shares != 0.0)
        {
            return Err("dollar risk must be finite and positive (or zero with size not set)".into());
        }
        if !draft.shares.is_finite() || draft.shares < 0.0 {
            return Err("shares must be finite and non-negative".into());
        }
        if draft.stop == draft.entry {
            return Err("stop must differ from entry".into());
        }
        let long = draft.stop < draft.entry;
        if let Some(target) = draft.target {
            if !target.is_finite() || target <= 0.0 {
                return Err("target must be finite and positive".into());
            }
            if (long && target <= draft.entry) || (!long && target >= draft.entry) {
                return Err("target must be on the profit side of entry".into());
            }
        }
        validate_breakeven(draft.breakeven_at_r)?;
        if let Some(trail) = &draft.trail {
            trail.validate(draft.entry, long)?;
        }
        if self.plans.iter().any(|p| p.symbol == symbol && p.status.active()) {
            return Err("cancel the existing plan first".into());
        }
        if self.plans.iter().filter(|p| p.status.active()).count() >= MAX_ACTIVE_PLANS {
            return Err(format!("at most {MAX_ACTIVE_PLANS} active trade plans"));
        }
        let base = format!("{symbol}:plan:{now}");
        let mut id = base.clone();
        let mut n = 0;
        while self.plans.iter().any(|p| p.id == id) || self.events.iter().any(|e| e.plan_id == id) {
            n += 1;
            id = format!("{base}:{n}");
        }
        let plan = TradePlan {
            id,
            symbol,
            long,
            entry: draft.entry,
            stop: draft.stop,
            target: draft.target,
            dollar_risk: draft.dollar_risk,
            shares: draft.shares,
            stop_source: draft.stop_source,
            initial_stop: Some(draft.stop),
            breakeven_at_r: draft.breakeven_at_r,
            trail: draft.trail,
            high_water: None,
            breakeven_seen: false,
            stop_moved_at: None,
            stop_manual: false,
            status: PlanStatus::Armed,
            entry_side: None,
            last_price: None,
            last_tick_ts: None,
            one_r_seen: false,
            created_at: now,
            triggered_at: None,
            triggered_price: None,
            closed_at: None,
            closed_price: None,
        };
        self.plans.insert(0, plan.clone());
        self.trim();
        Ok(plan)
    }
    pub fn update(&mut self, query: &str, change: PlanChange, now: i64) -> Result<TradePlan, String> {
        let mut matching = self.plans.iter_mut().filter(|p| p.status.active() && matches_query(p, query));
        let plan = matching.next().ok_or("no active trade plan matches that ticker or id")?;
        if matching.next().is_some() {
            return Err("more than one active trade plan matches; use a plan id".into());
        }
        match change {
            PlanChange::Trail(trail) => {
                if let Some(spec) = &trail {
                    spec.validate(plan.entry, plan.long)?;
                    if plan.status == PlanStatus::Triggered {
                        plan.high_water = plan.last_price;
                    }
                }
                plan.trail = trail;
            }
            PlanChange::Breakeven(r) => {
                validate_breakeven(r)?;
                plan.breakeven_at_r = r;
                if r.is_some() && !plan.at_breakeven() {
                    plan.breakeven_seen = false;
                }
            }
            PlanChange::MoveStop(stop) => {
                if plan.status != PlanStatus::Triggered {
                    return Err("cancel and re-arm to change an armed plan's stop".into());
                }
                let price = plan
                    .last_price
                    .filter(|p| p.is_finite() && *p > 0.0)
                    .ok_or("moving a stop needs an observed quote")?;
                if plan.last_tick_ts.is_none_or(|ts| price_alerts::is_stale(ts, now)) {
                    return Err("moving a stop needs a fresh verified observed quote".into());
                }
                if !stop.is_finite() || stop <= 0.0 || !plan.tighter(stop, plan.stop) {
                    return Err("stop must be finite, positive and strictly tighter than the current stop".into());
                }
                if !plan.tighter(price, stop) {
                    return Err("stop must be strictly on the adverse side of the last observed quote".into());
                }
                // Freeze the risk basis when a legacy plan first opts into management.
                plan.initial_stop = Some(plan.initial_stop());
                plan.stop = stop;
                plan.stop_manual = true;
                plan.stop_moved_at = Some(now);
                if plan.at_breakeven() {
                    plan.breakeven_seen = true;
                }
            }
        }
        Ok(plan.clone())
    }
    pub fn cancel(&mut self, id: &str) -> bool {
        if let Some(p) = self.plans.iter_mut().find(|p| p.id == id && p.status.active()) {
            p.status = PlanStatus::Cancelled;
            p.closed_at = Some(chrono::Utc::now().timestamp());
            true
        } else {
            false
        }
    }
    pub fn remove(&mut self, query: &str) -> usize {
        let before = self.plans.len();
        self.plans.retain(|p| p.status.active() || !matches_query(p, query));
        before - self.plans.len()
    }
    pub fn cancel_symbol(&mut self, query: &str) -> usize {
        let ids: Vec<_> =
            self.plans.iter().filter(|p| p.status.active() && matches_query(p, query)).map(|p| p.id.clone()).collect();
        ids.iter().filter(|id| self.cancel(id)).count()
    }
    pub fn mark_seen(&mut self) {
        for e in &mut self.events {
            e.seen = true;
        }
    }
    pub fn unread(&self) -> usize {
        self.events.iter().filter(|e| !e.seen).count()
    }
    pub fn active_symbols(&self) -> Vec<String> {
        self.plans
            .iter()
            .filter(|p| p.status.active())
            .map(|p| p.symbol.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    pub fn snapshot(&self, now: i64, stale: bool, sample: bool, failures: Vec<AlertFailure>) -> PlanBatch {
        PlanBatch { plans: self.plans.clone(), events: self.events.clone(), failures, fetched_at: now, stale, sample }
    }
    fn trim(&mut self) {
        while self.plans.len() > MAX_PLANS {
            let oldest = self
                .plans
                .iter()
                .enumerate()
                .filter(|(_, p)| !p.status.active())
                .min_by_key(|(_, p)| (p.closed_at.unwrap_or(p.created_at), p.created_at))
                .map(|(i, _)| i);
            if let Some(i) = oldest {
                self.plans.remove(i);
            } else {
                break;
            }
        }
        self.events.sort_by_key(|e| std::cmp::Reverse(e.created_at));
        self.events.truncate(MAX_EVENTS);
    }
}
fn matches_query(plan: &TradePlan, query: &str) -> bool {
    let query = query.trim();
    !query.is_empty() && (plan.id == query || plan.symbol == query || plan.symbol == crate::security(query).key())
}

// Keep the existing price-alert model unchanged while making plan inputs serializable.
mod price_ticks {
    use super::*;
    #[derive(Serialize, Deserialize)]
    #[serde(remote = "PriceTick")]
    struct Tick {
        symbol: String,
        price: f64,
        ts: i64,
        provenance: tr_core::Provenance,
    }
    #[derive(Serialize, Deserialize)]
    struct Wrapped(#[serde(with = "Tick")] PriceTick);
    pub fn serialize<S: serde::Serializer>(ticks: &[PriceTick], serializer: S) -> Result<S::Ok, S::Error> {
        ticks.iter().cloned().map(Wrapped).collect::<Vec<_>>().serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<PriceTick>, D::Error> {
        Ok(Vec::<Wrapped>::deserialize(deserializer)?.into_iter().map(|tick| tick.0).collect())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalInput {
    pub now: i64,
    pub sample_provider: bool,
    #[serde(with = "price_ticks")]
    pub prices: Vec<PriceTick>,
    pub failures: Vec<AlertFailure>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalOutcome {
    pub batch: PlanBatch,
    pub fired: Vec<PlanEvent>,
}

pub fn open_r(plan: &TradePlan) -> Option<f64> {
    (plan.status == PlanStatus::Triggered).then(|| plan.last_price.map(|p| observed_r(plan, p))).flatten()
}
pub fn planned_r(plan: &TradePlan, level: f64) -> f64 {
    (level - plan.entry) / (plan.entry - plan.initial_stop())
}
fn observed_r(plan: &TradePlan, price: f64) -> f64 {
    planned_r(plan, price)
}
pub fn locked_r(plan: &TradePlan) -> Option<f64> {
    (plan.stop != plan.initial_stop()).then(|| planned_r(plan, plan.stop))
}
pub fn stop_label(plan: &TradePlan) -> &'static str {
    if plan.stop == plan.initial_stop() {
        "stop"
    } else if plan.stop == plan.entry {
        "breakeven stop"
    } else if plan.stop_manual {
        "manual stop"
    } else {
        "trailing stop"
    }
}

pub fn evaluate(store: &mut Store, input: EvalInput) -> EvalOutcome {
    if input.sample_provider {
        return EvalOutcome { batch: store.snapshot(input.now, false, true, input.failures), fired: Vec::new() };
    }
    let mut failures = input.failures;
    let mut stale = false;
    let mut fired = Vec::new();
    let prices: BTreeMap<_, _> = input.prices.into_iter().map(|t| (t.symbol.clone(), t)).collect();
    for plan in store.plans.iter_mut().filter(|p| p.status.active()) {
        let mut fail = |error: String| {
            if !failures.iter().any(|f| f.symbol == plan.symbol) {
                failures.push(AlertFailure { symbol: plan.symbol.clone(), error });
            }
        };
        let Some(tick) = prices.get(&plan.symbol) else {
            fail("no quote for this name".into());
            continue;
        };
        if !tick.provenance.is_verified() {
            fail(format!("refused {:?}: trade plans need verified real data", tick.provenance));
            continue;
        }
        if !tick.price.is_finite() || tick.price <= 0.0 {
            fail("quote price must be finite and positive".into());
            continue;
        }
        if price_alerts::is_stale(tick.ts, input.now) {
            stale = true;
            continue;
        }
        if plan.last_tick_ts.is_some_and(|ts| tick.ts <= ts) {
            continue;
        }
        let p = tick.price;
        let adv_stop = if plan.long { p <= plan.stop } else { p >= plan.stop };
        let fav = |level| if plan.long { p >= level } else { p <= level };
        let kind = if plan.status == PlanStatus::Armed {
            if adv_stop {
                plan.status = PlanStatus::Invalidated;
                Some(PlanEventKind::Invalidated)
            } else if let Some(prev) = plan.last_price {
                let crossed = match plan.entry_side {
                    Some(PriceSide::Above) => prev < plan.entry && p >= plan.entry,
                    Some(PriceSide::Below) => prev > plan.entry && p <= plan.entry,
                    None => false,
                };
                if crossed {
                    plan.status = PlanStatus::Triggered;
                    plan.triggered_at = Some(tick.ts);
                    plan.triggered_price = Some(p);
                    plan.high_water = Some(p);
                    Some(PlanEventKind::Entry)
                } else {
                    None
                }
            } else {
                plan.entry_side = Some(if p < plan.entry || (p == plan.entry && plan.long) {
                    PriceSide::Above
                } else {
                    PriceSide::Below
                });
                None
            }
        } else if adv_stop {
            plan.status = PlanStatus::Stopped;
            Some(PlanEventKind::Stop)
        } else if plan.target.is_some_and(fav) {
            plan.status = PlanStatus::TargetHit;
            Some(PlanEventKind::Target)
        } else {
            let high = plan.high_water.map_or(p, |old| if plan.long { old.max(p) } else { old.min(p) });
            plan.high_water = Some(high);
            let mut candidate = plan.stop;
            let mut tighten = |value| {
                if plan.tighter(value, candidate) {
                    candidate = value;
                }
            };
            if plan.breakeven_at_r.is_some_and(|r| planned_r(plan, p) >= r) {
                tighten(plan.entry);
            }
            if let Some(trail) = &plan.trail {
                let distance = trail.distance(plan, high);
                tighten(if plan.long { high - distance } else { high + distance });
            }
            if candidate.is_finite()
                && candidate > 0.0
                && plan.tighter(candidate, plan.stop)
                && plan.tighter(p, candidate)
            {
                plan.initial_stop = Some(plan.initial_stop());
                plan.stop = candidate;
                plan.stop_moved_at = Some(tick.ts);
            }
            if !plan.one_r_seen && planned_r(plan, p) >= 1.0 {
                plan.one_r_seen = true;
                Some(PlanEventKind::OneR)
            } else if !plan.breakeven_seen && plan.at_breakeven() {
                plan.breakeven_seen = true;
                Some(PlanEventKind::Breakeven)
            } else {
                None
            }
        };
        plan.last_price = Some(p);
        plan.last_tick_ts = Some(tick.ts);
        if !plan.status.active() {
            plan.closed_at = Some(tick.ts);
            plan.closed_price = Some(p);
        }
        if let Some(kind) = kind {
            let id = format!("{}:{}", plan.id, kind.key());
            if store.events.iter().any(|e| e.id == id) {
                continue;
            }
            let ticker = crate::security(&plan.symbol).ticker;
            let r = observed_r(plan, p);
            let message = match kind {
                PlanEventKind::Entry => {
                    let size = if plan.dollar_risk == 0.0 {
                        "size not set".into()
                    } else {
                        format!("{:.0} sh, risk ${:.0}", plan.shares, plan.dollar_risk)
                    };
                    // Target R is a planned level; any displayed R carries the observation disclaimer.
                    let target = plan
                        .target
                        .map(|t| format!(", target {t:.2} ({:.2}R)", observed_r(plan, t)))
                        .unwrap_or_default();
                    let be = plan.breakeven_at_r.map(|r| format!(", BE at +{r}R")).unwrap_or_default();
                    let trail = plan.trail.as_ref().map(|s| format!(", trail {s}")).unwrap_or_default();
                    format!(
                        "{ticker} entry {:.2} triggered at {p:.2} — plan {size}, stop {:.2}{target}{be}{trail} at observed quote, not a fill; no order was changed",
                        plan.entry, plan.stop
                    )
                }
                PlanEventKind::OneR => {
                    let be = if !plan.breakeven_seen && plan.at_breakeven() {
                        plan.breakeven_seen = true;
                        format!("; stop moved to breakeven {:.2}", plan.entry)
                    } else {
                        String::new()
                    };
                    format!(
                        "{ticker} +1R reached at {p:.2} — {r:+.2}R at observed quote, not a fill{be}; no order was changed"
                    )
                }
                PlanEventKind::Breakeven => format!(
                    "{ticker} stop moved to breakeven {:.2} at {p:.2} — observed quote, no order was changed",
                    plan.entry
                ),
                PlanEventKind::Target => format!(
                    "{ticker} target {:.2} reached at {p:.2} — {r:+.2}R at observed quote, not a fill; no order was changed",
                    plan.target.unwrap()
                ),
                PlanEventKind::Stop => {
                    let initial = locked_r(plan)
                        .map(|_| format!(" (initial stop {:.2})", plan.initial_stop()))
                        .unwrap_or_default();
                    format!(
                        "{ticker} {} {:.2} traded at {p:.2} — {r:+.2}R at observed quote, not a fill{initial}; no order was changed",
                        stop_label(plan),
                        plan.stop
                    )
                }
                PlanEventKind::Invalidated => {
                    format!("{ticker} stop {:.2} traded before entry — plan disarmed", plan.stop)
                }
            };
            fired.push(PlanEvent {
                id,
                plan_id: plan.id.clone(),
                symbol: plan.symbol.clone(),
                kind,
                message,
                price: p,
                created_at: tick.ts,
                seen: false,
            });
        }
    }
    for event in &fired {
        store.events.insert(0, event.clone());
    }
    if !fired.is_empty() {
        store.trim();
    }
    EvalOutcome { batch: store.snapshot(input.now, stale, false, failures), fired }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::Provenance;

    fn draft() -> PlanDraft {
        PlanDraft {
            symbol: "NVDA".into(),
            entry: 100.0,
            stop: 90.0,
            target: Some(120.0),
            dollar_risk: 300.0,
            shares: 30.0,
            stop_source: "command".into(),
            breakeven_at_r: None,
            trail: None,
        }
    }
    fn armed() -> Store {
        let mut store = Store::default();
        store.arm(draft(), 10).unwrap();
        store
    }
    fn tick(store: &mut Store, price: f64, ts: i64) -> EvalOutcome {
        evaluate(
            store,
            EvalInput {
                now: ts,
                sample_provider: false,
                prices: vec![PriceTick { symbol: "NVDA US Equity".into(), price, ts, provenance: Provenance::Real }],
                failures: vec![],
            },
        )
    }
    fn triggered() -> Store {
        let mut store = armed();
        tick(&mut store, 99.0, 100);
        tick(&mut store, 100.0, 101);
        store
    }
    fn managed(be: Option<f64>, trail: Option<TrailSpec>, short: bool) -> Store {
        let mut d = draft();
        d.breakeven_at_r = be;
        d.trail = trail;
        d.target = Some(if short { 70.0 } else { 130.0 });
        if short {
            d.stop = 110.0;
        }
        let mut s = Store::default();
        s.arm(d, 10).unwrap();
        tick(&mut s, if short { 101.0 } else { 99.0 }, 100);
        let entry = tick(&mut s, 100.0, 101);
        assert_eq!(entry.fired[0].kind, PlanEventKind::Entry);
        assert_eq!(s.plans[0].high_water, Some(100.0));
        assert_eq!(s.plans[0].stop, if short { 110.0 } else { 90.0 });
        s
    }
    fn trail(kind: TrailKind, value: f64) -> TrailSpec {
        TrailSpec { kind, value, source: "test".into() }
    }

    #[test]
    fn legacy_plan_json_without_new_fields_loads_and_evaluates_unchanged() {
        let json = r#"{"version":1,"plans":[{"id":"legacy","symbol":"NVDA US Equity","long":true,"entry":100.0,"stop":90.0,"target":120.0,"dollar_risk":300.0,"shares":30.0,"stop_source":"command","status":"Triggered","entry_side":"Above","last_price":100.0,"last_tick_ts":101,"one_r_seen":false,"created_at":10,"triggered_at":101,"triggered_price":100.0,"closed_at":null,"closed_price":null}],"events":[]}"#;
        let mut s: Store = serde_json::from_str(json).unwrap();
        assert_eq!(s.version, 1);
        assert_eq!(s.plans[0].initial_stop, None);
        assert_eq!(s.plans[0].initial_stop(), 90.0);
        assert!(s.plans[0].trail.is_none());
        assert!(tick(&mut s, 90.0, 102).fired[0].message.contains("-1.00R"));
    }

    #[test]
    fn arm_rejects_invalid_breakeven_and_trail_specs() {
        for value in [f64::NAN, f64::INFINITY, 0.0, -1.0, 20.1] {
            let mut d = draft();
            d.breakeven_at_r = Some(value);
            assert!(Store::default().arm(d, 10).is_err());
            let mut s = triggered();
            let before = s.clone();
            assert!(s.update("NVDA", PlanChange::Breakeven(Some(value)), 102).is_err());
            assert_eq!(s, before);
        }
        for kind in [TrailKind::R, TrailKind::Percent, TrailKind::Price] {
            let limit = match kind {
                TrailKind::R => 20.1,
                TrailKind::Percent => 50.0,
                TrailKind::Price => 100.0,
            };
            for value in [f64::NAN, f64::INFINITY, 0.0, -1.0, limit] {
                let spec = trail(kind, value);
                let mut d = draft();
                d.trail = Some(spec.clone());
                assert!(Store::default().arm(d, 10).is_err());
                let mut s = triggered();
                let before = s.clone();
                assert!(s.update("NVDA", PlanChange::Trail(Some(spec)), 102).is_err());
                assert_eq!(s, before);
            }
        }
    }

    #[test]
    fn breakeven_moves_stop_to_entry_once_at_threshold() {
        let mut s = managed(Some(1.0), None, false);
        assert!(tick(&mut s, 109.0, 102).fired.is_empty());
        assert_eq!(s.plans[0].stop, 90.0);
        let out = tick(&mut s, 110.0, 103);
        assert_eq!(out.fired.len(), 1);
        assert_eq!(out.fired[0].kind, PlanEventKind::OneR);
        assert!(out.fired[0].message.contains("stop moved to breakeven 100.00"));
        assert!(out.fired[0].message.contains("observed quote"));
        assert!(out.fired[0].message.contains("no order was changed"));
        assert_eq!(s.plans[0].stop, 100.0);
        assert_eq!(s.plans[0].stop_moved_at, Some(103));
        assert!(s.plans[0].breakeven_seen);
        assert!(tick(&mut s, 111.0, 104).fired.is_empty());
        assert!(!s.events.iter().any(|e| e.kind == PlanEventKind::Breakeven));
    }

    #[test]
    fn breakeven_event_fires_alone_when_threshold_differs_from_one_r() {
        let mut s = managed(Some(0.5), None, false);
        let out = tick(&mut s, 105.0, 102);
        assert_eq!(out.fired.len(), 1);
        assert_eq!(out.fired[0].kind, PlanEventKind::Breakeven);
        assert!(out.fired[0].message.contains("observed quote, no order was changed"));
        assert_eq!(tick(&mut s, 110.0, 103).fired[0].kind, PlanEventKind::OneR);
        assert!(tick(&mut s, 112.0, 104).fired.is_empty());
        // Persisted event ids still dedupe a reset flag.
        s.plans[0].breakeven_seen = false;
        assert!(tick(&mut s, 113.0, 105).fired.is_empty());
    }

    #[test]
    fn trail_r_ratchets_up_and_never_down() {
        let mut s = managed(None, Some(trail(TrailKind::R, 1.0)), false);
        for (i, (price, stop)) in
            [(105.0, 95.0), (112.0, 102.0), (108.0, 102.0), (115.0, 105.0)].into_iter().enumerate()
        {
            let out = tick(&mut s, price, 102 + i as i64);
            assert_eq!(s.plans[0].stop, stop);
            assert!(out.fired.iter().all(|e| e.kind == PlanEventKind::OneR));
        }
        assert_eq!(s.plans[0].high_water, Some(115.0));
    }

    #[test]
    fn trail_percent_and_price_distances() {
        for (kind, value, expected) in [(TrailKind::Percent, 10.0, 103.5), (TrailKind::Price, 4.2, 110.8)] {
            let mut s = managed(None, Some(trail(kind, value)), false);
            tick(&mut s, 115.0, 102);
            assert!((s.plans[0].stop - expected).abs() < 1e-9);
            tick(&mut s, 114.0, 103);
            assert!((s.plans[0].stop - expected).abs() < 1e-9);
        }
    }

    #[test]
    fn trailed_stop_out_reports_positive_r_against_initial_stop() {
        let mut s = managed(None, Some(trail(TrailKind::R, 1.0)), false);
        tick(&mut s, 115.0, 102);
        let out = tick(&mut s, 104.0, 103);
        assert_eq!(s.plans[0].status, PlanStatus::Stopped);
        let message = &out.fired[0].message;
        for text in [
            "trailing stop 105.00",
            "+0.40R",
            "initial stop 90.00",
            "observed quote, not a fill",
            "no order was changed",
        ] {
            assert!(message.contains(text), "{message}");
        }
        // Corrected acceptance sequence, including an unsized plan.
        let mut s = managed(Some(1.0), Some(trail(TrailKind::R, 1.0)), false);
        s.plans[0].dollar_risk = 0.0;
        s.plans[0].shares = 0.0;
        tick(&mut s, 110.0, 102);
        assert_eq!(s.plans[0].stop, 100.0);
        tick(&mut s, 118.0, 103);
        assert_eq!(s.plans[0].stop, 108.0);
        let out = tick(&mut s, 107.0, 104);
        assert!(out.fired[0].message.contains("trailing stop 108.00 traded at 107.00 — +0.70R"));
    }

    #[test]
    fn stop_check_uses_pre_tick_stop_and_target_wins_same_tick() {
        let mut s = managed(Some(1.0), Some(trail(TrailKind::Price, 0.0001)), false);
        let out = tick(&mut s, 115.0, 102);
        assert_eq!(s.plans[0].status, PlanStatus::Triggered);
        assert_eq!(out.fired[0].kind, PlanEventKind::OneR);
        let stop = s.plans[0].stop;
        let high = s.plans[0].high_water;
        assert_eq!(tick(&mut s, 131.0, 103).fired[0].kind, PlanEventKind::Target);
        assert_eq!(s.plans[0].stop, stop);
        assert_eq!(s.plans[0].high_water, high);
        // Even rounding a tiny distance to the quote cannot make a stop at the quote.
        let mut s = managed(None, Some(trail(TrailKind::Price, f64::MIN_POSITIVE)), false);
        tick(&mut s, 105.0, 102);
        assert_eq!(s.plans[0].stop, 90.0);
    }

    #[test]
    fn short_plan_trail_and_breakeven_mirror() {
        let mut s = managed(Some(0.5), Some(trail(TrailKind::R, 1.0)), true);
        assert_eq!(tick(&mut s, 95.0, 102).fired[0].kind, PlanEventKind::Breakeven);
        assert_eq!(s.plans[0].stop, 100.0);
        tick(&mut s, 88.0, 103);
        assert_eq!(s.plans[0].stop, 98.0);
        tick(&mut s, 92.0, 104);
        assert_eq!(s.plans[0].stop, 98.0);
        tick(&mut s, 85.0, 105);
        assert_eq!(s.plans[0].stop, 95.0);
        assert_eq!(s.plans[0].high_water, Some(85.0));
        assert!(tick(&mut s, 96.0, 106).fired[0].message.contains("+0.40R"));
    }

    #[test]
    fn stale_unverified_sample_and_duplicate_ticks_do_not_move_stop_or_high_water() {
        let mut s = managed(Some(0.5), Some(trail(TrailKind::R, 1.0)), false);
        let before = s.clone();
        for (sample_provider, provenance, ts, now, price) in [
            (true, Provenance::Real, 102, 102, 115.0),
            (false, Provenance::Sample, 102, 102, 115.0),
            (false, Provenance::Missing, 102, 102, 115.0),
            (false, Provenance::Real, 102, 2000, 115.0),
            (false, Provenance::Real, 101, 102, 115.0),
            (false, Provenance::Real, 100, 102, 115.0),
            (false, Provenance::Real, 102, 102, f64::NAN),
            (false, Provenance::Real, 102, 102, f64::INFINITY),
            (false, Provenance::Real, 102, 102, 0.0),
            (false, Provenance::Real, 102, 102, -1.0),
        ] {
            let out = evaluate(
                &mut s,
                EvalInput {
                    now,
                    sample_provider,
                    prices: vec![PriceTick { symbol: "NVDA US Equity".into(), price, ts, provenance }],
                    failures: vec![],
                },
            );
            assert_eq!(s, before);
            assert!(out.fired.is_empty());
        }
        let out = evaluate(&mut s, EvalInput { now: 102, sample_provider: false, prices: vec![], failures: vec![] });
        assert!(out.fired.is_empty());
        assert_eq!(s, before);
    }

    #[test]
    fn move_stop_validates_tighten_only_adverse_side_and_triggered_only() {
        let mut s = armed();
        assert_eq!(
            s.update("NVDA", PlanChange::MoveStop(95.0), 102).unwrap_err(),
            "cancel and re-arm to change an armed plan's stop"
        );
        let mut s = triggered();
        let before = s.clone();
        for stop in [80.0, 90.0, 100.0, 101.0, f64::NAN, f64::INFINITY, 0.0] {
            assert!(s.update("NVDA", PlanChange::MoveStop(stop), 102).is_err());
            assert_eq!(s, before);
        }
        assert!(s.update("NVDA", PlanChange::MoveStop(95.0), 2000).is_err());
        assert_eq!(s, before);
        let p = s.update("NVDA", PlanChange::MoveStop(95.0), 102).unwrap();
        assert!(p.stop_manual);
        assert_eq!(p.stop_moved_at, Some(102));
        assert_eq!(stop_label(&p), "manual stop");
        tick(&mut s, 105.0, 103);
        let p = s.update("NVDA", PlanChange::MoveStop(100.0), 104).unwrap();
        assert!(p.breakeven_seen);
        assert_eq!(stop_label(&p), "breakeven stop");
        assert!(tick(&mut s, 106.0, 105).fired.is_empty());
        let mut s = managed(None, None, true);
        tick(&mut s, 95.0, 102);
        assert!(s.update("NVDA", PlanChange::MoveStop(94.0), 103).is_err());
        let p = s.update("NVDA", PlanChange::MoveStop(98.0), 103).unwrap();
        assert!(p.breakeven_seen);
        assert_eq!(stop_label(&p), "manual stop");
    }

    #[test]
    fn attaching_trail_resets_high_water_to_last_price() {
        let mut s = managed(None, None, false);
        tick(&mut s, 120.0, 102);
        tick(&mut s, 110.0, 103);
        s.update("NVDA", PlanChange::Trail(Some(trail(TrailKind::R, 1.0))), 104).unwrap();
        assert_eq!(s.plans[0].high_water, Some(110.0));
        tick(&mut s, 109.0, 104);
        assert_eq!(s.plans[0].status, PlanStatus::Triggered);
        assert_eq!(s.plans[0].stop, 100.0);
    }

    #[test]
    fn removing_rules_never_loosens_stop() {
        let mut s = managed(Some(1.0), Some(trail(TrailKind::R, 1.0)), false);
        tick(&mut s, 118.0, 102);
        s.update("NVDA", PlanChange::Trail(None), 103).unwrap();
        s.update("NVDA", PlanChange::Breakeven(None), 103).unwrap();
        assert_eq!(s.plans[0].stop, 108.0);
        s.update("NVDA", PlanChange::Breakeven(Some(2.0)), 103).unwrap();
        assert!(s.plans[0].breakeven_seen);
        tick(&mut s, 115.0, 103);
        assert_eq!(s.plans[0].stop, 108.0);
        let before = s.clone();
        assert!(s.update("MSFT", PlanChange::Trail(None), 104).is_err());
        assert_eq!(s, before);
    }

    #[test]
    fn locked_r_and_planned_r_use_initial_stop() {
        let mut s = managed(None, None, false);
        assert_eq!(locked_r(&s.plans[0]), None);
        tick(&mut s, 118.0, 102);
        // A legacy plan opting into management freezes its original risk, too.
        s.plans[0].initial_stop = None;
        let p = s.update("NVDA", PlanChange::MoveStop(105.0), 103).unwrap();
        assert_eq!(p.initial_stop(), 90.0);
        assert_eq!(locked_r(&p), Some(0.5));
        assert_eq!(planned_r(&p, 130.0), 3.0);
        assert_eq!(open_r(&p), Some(1.8));
        let out = tick(&mut s, 119.0, 104);
        assert_eq!(out.fired.len(), 0);
    }

    #[test]
    fn arm_validates_levels_target_side_and_duplicates() {
        for value in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            for field in 0..4 {
                let mut d = draft();
                match field {
                    0 => d.entry = value,
                    1 => d.stop = value,
                    2 => d.dollar_risk = value,
                    _ => d.target = Some(value),
                }
                assert!(Store::default().arm(d, 0).is_err());
            }
        }
        for shares in [f64::NAN, f64::INFINITY, -1.0] {
            let mut d = draft();
            d.shares = shares;
            assert!(Store::default().arm(d, 0).is_err());
        }
        let mut d = draft();
        d.stop = d.entry;
        assert_eq!(Store::default().arm(d, 0).unwrap_err(), "stop must differ from entry");
        let mut d = draft();
        d.target = Some(95.0);
        assert_eq!(Store::default().arm(d, 0).unwrap_err(), "target must be on the profit side of entry");
        let mut store = armed();
        assert_eq!(store.arm(draft(), 11).unwrap_err(), "cancel the existing plan first");
        let mut d = draft();
        d.symbol = "MSFT".into();
        d.stop = 110.0;
        d.target = Some(80.0);
        assert!(!store.arm(d, 12).unwrap().long);
        let mut d = draft();
        d.dollar_risk = 0.0;
        d.shares = 0.0;
        assert!(Store::default().arm(d, 0).is_ok());
        for i in 0..MAX_ACTIVE_PLANS - 2 {
            let mut d = draft();
            d.symbol = format!("NAME{i}");
            store.arm(d, 20).unwrap();
        }
        let mut d = draft();
        d.symbol = "EXTRA".into();
        assert!(store.arm(d, 20).unwrap_err().contains("32"));
        let mut d = draft();
        d.symbol = "".into();
        assert!(Store::default().arm(d, 0).is_err());
        for name in ["EURUSD Curncy", "BTCUSD Curncy"] {
            let mut d = draft();
            d.symbol = name.into();
            assert!(Store::default().arm(d, 0).is_ok());
        }
    }
    #[test]
    fn first_sight_seeds_and_never_fires_entry() {
        for (price, side) in [(99.0, PriceSide::Above), (100.0, PriceSide::Above), (105.0, PriceSide::Below)] {
            let mut s = armed();
            assert!(tick(&mut s, price, 100).fired.is_empty());
            assert_eq!(s.plans[0].entry_side, Some(side));
            assert_eq!(s.plans[0].last_price, Some(price));
            assert_eq!(s.plans[0].status, PlanStatus::Armed);
        }
        let mut s = Store::default();
        let mut d = draft();
        d.stop = 110.0;
        d.target = Some(80.0);
        s.arm(d, 10).unwrap();
        tick(&mut s, 100.0, 100);
        assert_eq!(s.plans[0].entry_side, Some(PriceSide::Below));
    }
    #[test]
    fn breakout_long_triggers_then_one_r_then_target() {
        let mut s = armed();
        tick(&mut s, 99.0, 100);
        let mut kinds = vec![];
        for (p, ts) in [(101.0, 101), (110.0, 102), (115.0, 103), (120.2, 104)] {
            kinds.extend(tick(&mut s, p, ts).fired.iter().map(|e| e.kind));
        }
        assert_eq!(kinds, vec![PlanEventKind::Entry, PlanEventKind::OneR, PlanEventKind::Target]);
        assert_eq!(s.plans[0].status, PlanStatus::TargetHit);
        assert!(s.events[0].message.contains("+2.02R at observed quote, not a fill"));
        assert_eq!(s.plans[0].triggered_price, Some(101.0));
        assert_eq!(s.plans[0].closed_price, Some(120.2));
        assert_eq!(s.unread(), 3);
        s.mark_seen();
        assert_eq!(s.unread(), 0);
    }
    #[test]
    fn pullback_long_triggers_on_cross_down() {
        let mut s = armed();
        tick(&mut s, 105.0, 100);
        assert_eq!(s.plans[0].entry_side, Some(PriceSide::Below));
        assert_eq!(tick(&mut s, 100.0, 101).fired[0].kind, PlanEventKind::Entry);
    }
    #[test]
    fn short_plan_mirrors_stop_and_target() {
        let mut s = Store::default();
        let mut d = draft();
        d.stop = 110.0;
        d.target = Some(80.0);
        s.arm(d, 10).unwrap();
        tick(&mut s, 105.0, 100);
        assert_eq!(tick(&mut s, 100.0, 101).fired[0].kind, PlanEventKind::Entry);
        let mut target = s.clone();
        assert_eq!(tick(&mut target, 90.0, 102).fired[0].kind, PlanEventKind::OneR);
        let out = tick(&mut target, 78.0, 103);
        assert_eq!(out.fired[0].kind, PlanEventKind::Target);
        assert!(out.fired[0].message.contains("+2.20R"));
        let out = tick(&mut s, 111.0, 102);
        assert_eq!(s.plans[0].status, PlanStatus::Stopped);
        assert!(out.fired[0].message.contains("-1.10R"));
    }
    #[test]
    fn stop_before_entry_invalidates_without_entry_event() {
        for seed in [None, Some(99.0), Some(105.0)] {
            let mut s = armed();
            if let Some(p) = seed {
                tick(&mut s, p, 100);
            }
            let out = tick(&mut s, 85.0, 101);
            assert_eq!(out.fired.len(), 1);
            assert_eq!(out.fired[0].kind, PlanEventKind::Invalidated);
            assert_eq!(s.plans[0].status, PlanStatus::Invalidated);
            assert_eq!(s.plans[0].triggered_at, None);
            assert!(s.events.iter().all(|e| e.kind != PlanEventKind::Entry));
        }
    }
    #[test]
    fn gap_through_stop_reports_observed_r_not_minus_one() {
        let mut s = triggered();
        let out = tick(&mut s, 82.0, 102);
        assert_eq!(out.fired[0].kind, PlanEventKind::Stop);
        assert!(out.fired[0].message.contains("-1.80R at observed quote, not a fill"));
    }
    #[test]
    fn one_transition_per_tick_entry_then_target_next_tick() {
        let mut s = armed();
        tick(&mut s, 99.0, 100);
        let out = tick(&mut s, 125.0, 101);
        assert_eq!(out.fired.len(), 1);
        assert_eq!(out.fired[0].kind, PlanEventKind::Entry);
        assert_eq!(s.plans[0].status, PlanStatus::Triggered);
        assert_eq!(tick(&mut s, 126.0, 102).fired[0].kind, PlanEventKind::Target);
    }
    #[test]
    fn stale_tick_does_not_fire_or_move_seed() {
        let mut s = armed();
        tick(&mut s, 99.0, 100);
        let before = s.clone();
        let out = evaluate(
            &mut s,
            EvalInput {
                now: 2000,
                sample_provider: false,
                prices: vec![PriceTick {
                    symbol: "NVDA US Equity".into(),
                    price: 105.0,
                    ts: 101,
                    provenance: Provenance::Real,
                }],
                failures: vec![],
            },
        );
        assert!(out.batch.stale);
        assert!(out.fired.is_empty());
        assert_eq!(s, before);
        assert!(tick(&mut s, 99.5, 2001).fired.is_empty());
        let mut s = armed();
        let before = s.clone();
        evaluate(
            &mut s,
            EvalInput {
                now: 2000,
                sample_provider: false,
                prices: vec![PriceTick {
                    symbol: "NVDA US Equity".into(),
                    price: 105.0,
                    ts: 101,
                    provenance: Provenance::Real,
                }],
                failures: vec![],
            },
        );
        assert_eq!(s, before);
    }
    #[test]
    fn sample_provider_and_unverified_ticks_never_change_state() {
        for s in [armed(), triggered()] {
            for (sample_provider, provenance) in [(true, Provenance::Real), (false, Provenance::Sample)] {
                let mut s = s.clone();
                let before = s.clone();
                let out = evaluate(
                    &mut s,
                    EvalInput {
                        now: 102,
                        sample_provider,
                        prices: vec![PriceTick { symbol: "NVDA US Equity".into(), price: 80.0, ts: 102, provenance }],
                        failures: vec![],
                    },
                );
                assert_eq!(s, before);
                assert!(out.fired.is_empty());
                if sample_provider {
                    assert!(out.batch.sample);
                } else {
                    assert!(out.batch.failures[0].error.contains("trade plans need verified real data"));
                }
            }
        }
    }
    #[test]
    fn missing_quote_records_failure_and_keeps_state() {
        let mut s = armed();
        let before = s.clone();
        let out = evaluate(&mut s, EvalInput { now: 100, sample_provider: false, prices: vec![], failures: vec![] });
        assert_eq!(s, before);
        assert!(out.fired.is_empty());
        assert_eq!(out.batch.failures[0].error, "no quote for this name");
        for price in [f64::NAN, f64::INFINITY, 0.0, -5.0] {
            assert!(!tick(&mut s, price, 100).batch.failures.is_empty());
            assert_eq!(s, before);
        }
    }
    #[test]
    fn out_of_order_or_duplicate_tick_ts_is_ignored() {
        let mut s = armed();
        tick(&mut s, 99.0, 100);
        let before = s.clone();
        for ts in [99, 100] {
            assert!(tick(&mut s, 125.0, ts).fired.is_empty());
            assert_eq!(s, before);
        }
        tick(&mut s, 100.0, 101);
        let before = s.clone();
        assert!(tick(&mut s, 80.0, 101).fired.is_empty());
        assert_eq!(s, before);
    }
    #[test]
    fn events_dedup_and_cap_and_closed_plans_trim() {
        let mut s = armed();
        let active_id = s.plans[0].id.clone();
        let mut template = s.plans[0].clone();
        template.status = PlanStatus::Cancelled;
        for i in 0..MAX_PLANS {
            let mut p = template.clone();
            p.id = format!("old{i}");
            p.created_at = i as i64;
            p.closed_at = Some(i as i64);
            s.plans.push(p);
        }
        s.trim();
        assert_eq!(s.plans.len(), MAX_PLANS);
        assert!(s.plans.iter().any(|p| p.id == active_id));
        assert!(!s.plans.iter().any(|p| p.id == "old0"));
        tick(&mut s, 99.0, 100);
        tick(&mut s, 100.0, 101);
        // Even if a persisted state is replayed, the event id is the dedup key.
        s.plans[0].status = PlanStatus::Armed;
        s.plans[0].last_price = Some(99.0);
        assert!(tick(&mut s, 100.0, 102).fired.is_empty());
        let event = s.events[0].clone();
        for i in 0..MAX_EVENTS {
            let mut e = event.clone();
            e.id = format!("e{i}");
            e.created_at = 200 + i as i64;
            s.events.push(e);
        }
        s.trim();
        assert_eq!(s.events.len(), MAX_EVENTS);
        assert_eq!(s.events[0].created_at, 399);
        assert_eq!(s.events.last().unwrap().created_at, 200);
    }
    #[test]
    fn lifecycle_retention_keeps_active_plans_and_newest_events() {
        let mut store = Store::default();
        let mut keep = draft();
        keep.symbol = "MSFT".into();
        let keep_id = store.arm(keep, 1).unwrap().id;
        let mut oldest = String::new();
        for i in 0..MAX_PLANS + 2 {
            let ts = 1000 + i as i64 * 4;
            let plan = store.arm(draft(), ts).unwrap();
            if i == 0 {
                oldest = plan.id;
            }
            tick(&mut store, 99.0, ts);
            tick(&mut store, 100.0, ts + 1);
            tick(&mut store, 121.0, ts + 2);
        }
        assert_eq!(store.plans.len(), MAX_PLANS);
        assert!(store.plans.iter().any(|p| p.id == keep_id && p.status == PlanStatus::Armed));
        assert!(!store.plans.iter().any(|p| p.id == oldest));
        assert_eq!(store.events.len(), MAX_EVENTS);
        assert_eq!(store.events[0].kind, PlanEventKind::Target);
        assert!(store.events.windows(2).all(|p| p[0].created_at >= p[1].created_at));
    }

    #[test]
    fn cancel_remove_and_json_roundtrip() {
        let mut s = triggered();
        let id = s.plans[0].id.clone();
        assert_eq!(s.remove("NVDA"), 0);
        assert_eq!(s.active_symbols(), vec!["NVDA US Equity"]);
        assert!(s.cancel(&id));
        assert!(s.plans[0].closed_at.is_some());
        assert!(!s.cancel(&id));
        let p = s.arm(draft(), 10).unwrap();
        assert!(p.id.ends_with(":1"));
        assert_eq!(s.cancel_symbol("nvda"), 1);
        assert_eq!(s.cancel_symbol("NVDA"), 0);
        let path = std::env::temp_dir().join(format!("trade-plan-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(Store::load(&path), Store::default());
        assert!(!path.exists());
        std::fs::write(&path, b"corrupt").unwrap();
        assert_eq!(Store::load(&path), Store::default());
        assert_eq!(std::fs::read(&path).unwrap(), b"corrupt");
        s.save(&path);
        assert_eq!(Store::load(&path), s);
        let input = EvalInput {
            now: 100,
            sample_provider: false,
            prices: vec![PriceTick {
                symbol: "NVDA US Equity".into(),
                price: 100.0,
                ts: 100,
                provenance: Provenance::Real,
            }],
            failures: vec![],
        };
        let encoded = serde_json::to_vec(&input).unwrap();
        assert_eq!(serde_json::from_slice::<EvalInput>(&encoded).unwrap(), input);
        assert_eq!(s.remove(&id), 1);
        assert_eq!(s.remove("NVDA"), 1);
        assert_eq!(s.remove(""), 0);
        assert_eq!(s.plans.len(), 0);
        s.save(Path::new(""));
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn open_r_only_for_triggered_with_price() {
        let mut s = armed();
        assert_eq!(open_r(&s.plans[0]), None);
        s.plans[0].status = PlanStatus::Triggered;
        assert_eq!(open_r(&s.plans[0]), None);
        s.plans[0].last_price = Some(104.5);
        assert_eq!(open_r(&s.plans[0]), Some(0.45));
        s.plans[0].status = PlanStatus::TargetHit;
        assert_eq!(open_r(&s.plans[0]), None);
        s.plans[0].status = PlanStatus::Triggered;
        s.plans[0].stop = 110.0;
        assert_eq!(open_r(&s.plans[0]), Some(0.45));
    }
}
