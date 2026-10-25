//! Persistent price-level and setup-change alerts.
//!
//! Rules live in `price_alerts.json`. Evaluation never invents events from sample or
//! unverified bars. Freshness and partial-failure banners follow the Step 8 news
//! pattern ([`crate::news::STALE_AFTER_SECS`], leftover list on a failed refresh).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tr_core::Provenance;

use crate::history::calendar::last_expected_session;
use crate::whats_news::SETUPS;

/// Same 15-minute stale banner as watchlist news.
pub const STALE_AFTER_SECS: i64 = crate::news::STALE_AFTER_SECS;
/// Cap so a long session cannot accumulate unbounded rules.
pub const MAX_RULES: usize = 64;
/// Fired events retained after newest-first trim.
pub const MAX_EVENTS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PriceSide {
    Above,
    Below,
}

impl PriceSide {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Above => "above",
            Self::Below => "below",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Above => "above",
            Self::Below => "below",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum SetupChange {
    Enter,
    Exit,
    Either,
}

impl SetupChange {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Exit => "exit",
            Self::Either => "either",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Enter => "enters",
            Self::Exit => "exits",
            Self::Either => "changes",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum AlertKind {
    Price { side: PriceSide, level: f64 },
    Setup { setup: String, change: SetupChange },
}

impl AlertKind {
    pub fn fingerprint(&self) -> String {
        match self {
            Self::Price { side, level } => format!("price:{}:{level:.4}", side.as_str()),
            Self::Setup { setup, change } => format!("setup:{setup}:{}", change.as_str()),
        }
    }

    pub fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Price { side: a, level: x }, Self::Price { side: b, level: y }) => {
                a == b && x.is_finite() && y.is_finite() && (x - y).abs() < 1e-6
            }
            (Self::Setup { setup: a, change: c }, Self::Setup { setup: b, change: d }) => a == b && c == d,
            _ => false,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Price { side, level } => format!("{} {level:.2}", side.label()),
            Self::Setup { setup, change } => format!("{setup} {}", change.label()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertRule {
    pub id: String,
    pub symbol: String,
    pub kind: AlertKind,
    pub created_at: i64,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FiredAlert {
    pub id: String,
    pub rule_id: String,
    pub symbol: String,
    pub message: String,
    pub created_at: i64,
    pub seen: bool,
    pub kind: AlertKind,
    pub trigger_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertFailure {
    pub symbol: String,
    pub error: String,
}

/// Deduplicated rules and events plus partial-failure / freshness state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertBatch {
    pub rules: Vec<AlertRule>,
    pub events: Vec<FiredAlert>,
    pub failures: Vec<AlertFailure>,
    /// Market keys that had at least one enabled rule this eval.
    pub queried: Vec<String>,
    pub fetched_at: i64,
    /// Cache / quote / last bar older than [`STALE_AFTER_SECS`], or a refresh failed.
    pub stale: bool,
    /// Sample / offline provider: no events are invented.
    pub sample: bool,
}

impl AlertBatch {
    pub fn empty(now: i64) -> Self {
        Self {
            rules: Vec::new(),
            events: Vec::new(),
            failures: Vec::new(),
            queried: Vec::new(),
            fetched_at: now,
            stale: false,
            sample: false,
        }
    }

    pub fn sample_isolated(now: i64) -> Self {
        Self { sample: true, ..Self::empty(now) }
    }

    pub fn is_partial(&self) -> bool {
        !self.failures.is_empty() && (!self.events.is_empty() || self.rules.iter().any(|r| r.enabled))
    }

    pub fn is_stale_at(&self, now: i64) -> bool {
        self.stale || is_stale(self.fetched_at, now)
    }

    pub fn unread(&self) -> usize {
        self.events.iter().filter(|e| !e.seen).count()
    }

    pub fn price_levels_for(&self, symbol: &str) -> Vec<(f64, PriceSide)> {
        self.rules
            .iter()
            .filter(|r| r.enabled && r.symbol == symbol)
            .filter_map(|r| match r.kind {
                AlertKind::Price { side, level } => Some((level, side)),
                AlertKind::Setup { .. } => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RuleState {
    pub last_price: Option<f64>,
    pub last_match: Option<bool>,
    pub last_eval_at: i64,
    pub last_bar_ts: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Store {
    #[serde(default)]
    pub rules: Vec<AlertRule>,
    #[serde(default)]
    pub events: Vec<FiredAlert>,
    #[serde(default)]
    pub states: BTreeMap<String, RuleState>,
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

    pub fn snapshot(&self, now: i64, stale: bool, sample: bool, failures: Vec<AlertFailure>) -> AlertBatch {
        let mut queried = Vec::new();
        for r in self.rules.iter().filter(|r| r.enabled) {
            if !queried.iter().any(|s| s == &r.symbol) {
                queried.push(r.symbol.clone());
            }
        }
        AlertBatch {
            rules: self.rules.clone(),
            events: self.events.clone(),
            failures,
            queried,
            fetched_at: now,
            stale,
            sample,
        }
    }

    /// Insert a rule once per (symbol, kind). Returns the existing row on a duplicate.
    pub fn add_rule(&mut self, symbol: impl Into<String>, kind: AlertKind, now: i64) -> Result<AlertRule, String> {
        let symbol = crate::security(&symbol.into()).key();
        if symbol.is_empty() {
            return Err("invalid security".into());
        }
        if let AlertKind::Price { level, .. } = kind {
            if !level.is_finite() || level <= 0.0 {
                return Err("price level must be a positive number".into());
            }
        }
        if let AlertKind::Setup { ref setup, .. } = kind {
            normalize_setup(setup).ok_or_else(|| format!("unknown setup {setup}"))?;
        }
        if let Some(existing) = self.rules.iter().find(|r| r.symbol == symbol && r.kind.same(&kind)) {
            return Ok(existing.clone());
        }
        if self.rules.len() >= MAX_RULES {
            return Err(format!("at most {MAX_RULES} price/setup rules"));
        }
        let rule = AlertRule {
            id: format!("{symbol}:{}:{now}", kind.fingerprint()),
            symbol,
            kind,
            created_at: now,
            enabled: true,
        };
        self.rules.insert(0, rule.clone());
        Ok(rule)
    }

    pub fn remove(&mut self, query: &str) -> usize {
        let q = query.trim();
        if q.is_empty() {
            return 0;
        }
        let key = crate::security(q).key();
        let before = self.rules.len();
        self.rules.retain(|r| r.id != q && r.symbol != q && r.symbol != key);
        let gone: Vec<String> =
            self.states.keys().filter(|id| !self.rules.iter().any(|r| r.id == **id)).cloned().collect();
        for id in gone {
            self.states.remove(&id);
        }
        before - self.rules.len()
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> bool {
        if let Some(r) = self.rules.iter_mut().find(|r| r.id == id) {
            r.enabled = enabled;
            true
        } else {
            false
        }
    }

    pub fn mark_seen(&mut self) {
        for e in &mut self.events {
            e.seen = true;
        }
    }

    fn try_fire(&mut self, rule: &AlertRule, message: String, trigger_key: String, now: i64) -> Option<FiredAlert> {
        if self.events.iter().any(|e| e.trigger_key == trigger_key) {
            return None;
        }
        let event = FiredAlert {
            id: trigger_key.clone(),
            rule_id: rule.id.clone(),
            symbol: rule.symbol.clone(),
            message,
            created_at: now,
            seen: false,
            kind: rule.kind.clone(),
            trigger_key,
        };
        self.events.insert(0, event.clone());
        self.events.truncate(MAX_EVENTS);
        Some(event)
    }
}

/// A verified last price used for crossing checks.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceTick {
    pub symbol: String,
    pub price: f64,
    pub ts: i64,
    pub provenance: Provenance,
}

/// Current SCAN setup membership for one name, from verified daily bars.
#[derive(Debug, Clone, PartialEq)]
pub struct SetupTick {
    pub symbol: String,
    pub active: BTreeMap<String, bool>,
    pub bar_ts: i64,
    pub provenance: Provenance,
    pub stale: bool,
}

pub struct EvalInput {
    pub now: i64,
    pub sample_provider: bool,
    pub prices: Vec<PriceTick>,
    pub setups: Vec<SetupTick>,
    pub failures: Vec<AlertFailure>,
}

pub struct EvalOutcome {
    pub batch: AlertBatch,
    pub fired: Vec<FiredAlert>,
}

pub fn is_stale(fetched_at: i64, now: i64) -> bool {
    fetched_at > 0 && now - fetched_at > STALE_AFTER_SECS
}

pub fn normalize_setup(name: &str) -> Option<String> {
    let up = name.trim().to_ascii_uppercase();
    SETUPS.iter().find(|n| **n == up).map(|n| (*n).to_string())
}

/// Daily setup feed is stale when the last bar is older than the last expected session
/// (same calendar as UNIV / SCAN).
pub fn setup_bars_stale(bar_ts: i64, now: i64) -> bool {
    let Some(bar) = Utc.timestamp_opt(bar_ts, 0).single() else {
        return true;
    };
    let Some(now) = Utc.timestamp_opt(now, 0).single() else {
        return true;
    };
    bar.date_naive() < last_expected_session(now)
}

fn crossed(prev: f64, price: f64, side: PriceSide, level: f64) -> bool {
    match side {
        PriceSide::Above => prev < level && price >= level,
        PriceSide::Below => prev > level && price <= level,
    }
}

/// Evaluate enabled rules. First observation of a name only seeds state (no fire).
/// Sample / unverified ticks never produce events.
pub fn evaluate(store: &mut Store, input: EvalInput) -> EvalOutcome {
    if input.sample_provider {
        let mut batch = store.snapshot(input.now, false, true, Vec::new());
        batch.sample = true;
        return EvalOutcome { batch, fired: Vec::new() };
    }
    let mut failures = input.failures;
    let mut stale = false;
    let mut fired = Vec::new();
    let prices: BTreeMap<_, _> = input.prices.into_iter().map(|t| (t.symbol.clone(), t)).collect();
    let setups: BTreeMap<_, _> = input.setups.into_iter().map(|t| (t.symbol.clone(), t)).collect();
    let rules = store.rules.clone();
    for rule in rules.iter().filter(|r| r.enabled) {
        match &rule.kind {
            AlertKind::Price { side, level } => {
                let Some(tick) = prices.get(&rule.symbol) else {
                    failures.push(AlertFailure { symbol: rule.symbol.clone(), error: "no quote for this name".into() });
                    continue;
                };
                if !tick.provenance.is_verified() {
                    failures.push(AlertFailure {
                        symbol: rule.symbol.clone(),
                        error: format!(
                            "refused {}: price/setup alerts need verified real data",
                            tick.provenance.label()
                        ),
                    });
                    continue;
                }
                if !tick.price.is_finite() || tick.price <= 0.0 {
                    failures.push(AlertFailure { symbol: rule.symbol.clone(), error: "invalid last price".into() });
                    continue;
                }
                if is_stale(tick.ts, input.now) {
                    stale = true;
                    continue;
                }
                let prev = store.states.get(&rule.id).and_then(|s| s.last_price);
                if let Some(prev) = prev
                    && crossed(prev, tick.price, *side, *level)
                {
                    let key = format!("{}:{}:{:.4}:{}", rule.id, side.as_str(), level, tick.ts);
                    let ticker = crate::security(&rule.symbol).ticker;
                    let message = format!("{ticker} crossed {} {level:.2} at {:.2}", side.label(), tick.price);
                    if let Some(event) = store.try_fire(rule, message, key, input.now) {
                        fired.push(event);
                    }
                }
                let state = store.states.entry(rule.id.clone()).or_default();
                state.last_price = Some(tick.price);
                state.last_eval_at = input.now;
            }
            AlertKind::Setup { setup, change } => {
                let Some(tick) = setups.get(&rule.symbol) else {
                    failures.push(AlertFailure {
                        symbol: rule.symbol.clone(),
                        error: "no verified daily history for this setup".into(),
                    });
                    continue;
                };
                if !tick.provenance.is_verified() {
                    failures.push(AlertFailure {
                        symbol: rule.symbol.clone(),
                        error: format!(
                            "refused {}: price/setup alerts need verified real data",
                            tick.provenance.label()
                        ),
                    });
                    continue;
                }
                if tick.stale {
                    stale = true;
                    continue;
                }
                let Some(&now_match) = tick.active.get(setup) else {
                    failures.push(AlertFailure {
                        symbol: rule.symbol.clone(),
                        error: format!("setup {setup} could not be evaluated"),
                    });
                    continue;
                };
                let prev = store.states.get(&rule.id).and_then(|s| s.last_match);
                if let Some(prev) = prev {
                    let transition = match (prev, now_match) {
                        (false, true) => Some(SetupChange::Enter),
                        (true, false) => Some(SetupChange::Exit),
                        _ => None,
                    };
                    if let Some(t) = transition
                        && (*change == SetupChange::Either || *change == t)
                    {
                        let key = format!("{}:{}:{}", rule.id, t.as_str(), tick.bar_ts);
                        let ticker = crate::security(&rule.symbol).ticker;
                        let verb = if t == SetupChange::Enter { "entered" } else { "exited" };
                        let message = format!("{ticker} {verb} {setup}");
                        if let Some(event) = store.try_fire(rule, message, key, input.now) {
                            fired.push(event);
                        }
                    }
                }
                let state = store.states.entry(rule.id.clone()).or_default();
                state.last_match = Some(now_match);
                state.last_bar_ts = Some(tick.bar_ts);
                state.last_eval_at = input.now;
            }
        }
    }
    EvalOutcome { batch: store.snapshot(input.now, stale, false, failures), fired }
}

/// Build a [`SetupTick`] from verified daily bars. Sample / unverified history is refused.
pub fn setup_tick(
    symbol: impl Into<String>,
    bars: &[tr_core::Bar],
    wanted: &[String],
    now: i64,
) -> Result<SetupTick, String> {
    let symbol = crate::security(&symbol.into()).key();
    tr_core::Bar::require_verified(bars, "price/setup alert")?;
    let last = bars.last().ok_or_else(|| "no daily history".to_string())?;
    let mut active = BTreeMap::new();
    for name in wanted {
        let setup = normalize_setup(name).ok_or_else(|| format!("unknown setup {name}"))?;
        active.insert(setup.clone(), crate::whats_news::setup_active(&setup, bars)?);
    }
    Ok(SetupTick {
        symbol,
        active,
        bar_ts: last.ts,
        provenance: tr_core::Bar::provenance(bars),
        stale: setup_bars_stale(last.ts, now),
    })
}

/// Persist path under the terminal data directory.
pub fn store_path(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join("price_alerts.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::Bar;

    fn price(symbol: &str, last: f64, ts: i64, provenance: Provenance) -> PriceTick {
        PriceTick { symbol: crate::security(symbol).key(), price: last, ts, provenance }
    }

    fn setup(symbol: &str, name: &str, on: bool, bar_ts: i64, provenance: Provenance, stale: bool) -> SetupTick {
        let mut active = BTreeMap::new();
        active.insert(name.into(), on);
        SetupTick { symbol: crate::security(symbol).key(), active, bar_ts, provenance, stale }
    }

    fn eval(store: &mut Store, now: i64, prices: Vec<PriceTick>, setups: Vec<SetupTick>) -> EvalOutcome {
        evaluate(store, EvalInput { now, sample_provider: false, prices, setups, failures: Vec::new() })
    }

    #[test]
    fn rules_dedup_by_symbol_and_kind() {
        let mut s = Store::default();
        let a = s.add_rule("aapl", AlertKind::Price { side: PriceSide::Above, level: 150.0 }, 1).unwrap();
        let b = s.add_rule("AAPL US Equity", AlertKind::Price { side: PriceSide::Above, level: 150.0 }, 2).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(s.rules.len(), 1);
        s.add_rule("aapl", AlertKind::Price { side: PriceSide::Below, level: 150.0 }, 3).unwrap();
        s.add_rule("aapl", AlertKind::Setup { setup: "NEAR_HIGH".into(), change: SetupChange::Enter }, 4).unwrap();
        assert_eq!(s.rules.len(), 3);
    }

    #[test]
    fn price_cross_fires_once_then_recrosses() {
        let mut s = Store::default();
        let rule = s.add_rule("aapl", AlertKind::Price { side: PriceSide::Above, level: 100.0 }, 10).unwrap();
        let first = eval(&mut s, 20, vec![price("AAPL", 99.0, 20, Provenance::Real)], vec![]);
        assert!(first.fired.is_empty(), "first observation only seeds");
        let cross = eval(&mut s, 30, vec![price("AAPL", 101.0, 30, Provenance::Real)], vec![]);
        assert_eq!(cross.fired.len(), 1);
        assert!(cross.fired[0].message.contains("crossed above 100.00"));
        assert_eq!(cross.fired[0].rule_id, rule.id);
        let again = eval(&mut s, 40, vec![price("AAPL", 102.0, 40, Provenance::Real)], vec![]);
        assert!(again.fired.is_empty(), "still above does not re-fire");
        let _ = eval(&mut s, 50, vec![price("AAPL", 99.0, 50, Provenance::Real)], vec![]);
        let recross = eval(&mut s, 60, vec![price("AAPL", 100.5, 60, Provenance::Real)], vec![]);
        assert_eq!(recross.fired.len(), 1);
        assert_eq!(s.events.len(), 2);
    }

    #[test]
    fn below_cross_and_event_dedup_key() {
        let mut s = Store::default();
        s.add_rule("msft", AlertKind::Price { side: PriceSide::Below, level: 400.0 }, 1).unwrap();
        let _ = eval(&mut s, 2, vec![price("MSFT", 410.0, 2, Provenance::Real)], vec![]);
        let fire = eval(&mut s, 3, vec![price("MSFT", 399.0, 3, Provenance::Real)], vec![]);
        assert_eq!(fire.fired.len(), 1);
        let dup = eval(&mut s, 4, vec![price("MSFT", 398.0, 3, Provenance::Real)], vec![]);
        assert!(dup.fired.is_empty(), "same tick ts must not insert twice");
    }

    #[test]
    fn setup_enter_exit_and_either() {
        let mut s = Store::default();
        s.add_rule("nvda", AlertKind::Setup { setup: "NEAR_HIGH".into(), change: SetupChange::Enter }, 1).unwrap();
        s.add_rule("nvda", AlertKind::Setup { setup: "NEAR_HIGH".into(), change: SetupChange::Exit }, 2).unwrap();
        let seed = setup("NVDA", "NEAR_HIGH", false, 100, Provenance::Cached, false);
        assert!(eval(&mut s, 10, vec![], vec![seed.clone()]).fired.is_empty());
        let entered = setup("NVDA", "NEAR_HIGH", true, 200, Provenance::Cached, false);
        let fire = eval(&mut s, 20, vec![], vec![entered]);
        assert_eq!(fire.fired.len(), 1);
        assert!(fire.fired[0].message.contains("entered NEAR_HIGH"));
        let exited = setup("NVDA", "NEAR_HIGH", false, 300, Provenance::Cached, false);
        let fire = eval(&mut s, 30, vec![], vec![exited]);
        assert_eq!(fire.fired.len(), 1);
        assert!(fire.fired[0].message.contains("exited NEAR_HIGH"));
    }

    #[test]
    fn sample_provider_and_sample_ticks_never_invent_events() {
        let mut s = Store::default();
        s.add_rule("aapl", AlertKind::Price { side: PriceSide::Above, level: 1.0 }, 1).unwrap();
        let outcome = evaluate(
            &mut s,
            EvalInput {
                now: 10,
                sample_provider: true,
                prices: vec![price("AAPL", 2.0, 10, Provenance::Sample)],
                setups: vec![],
                failures: vec![],
            },
        );
        assert!(outcome.fired.is_empty());
        assert!(outcome.batch.sample);
        assert!(outcome.batch.events.is_empty());
        let refused = eval(&mut s, 11, vec![price("AAPL", 2.0, 11, Provenance::Sample)], vec![]);
        assert!(refused.fired.is_empty());
        assert_eq!(refused.batch.failures.len(), 1);
        assert!(refused.batch.failures[0].error.contains("SAMPLE"));
        let missing = eval(&mut s, 12, vec![price("AAPL", 2.0, 12, Provenance::Missing)], vec![]);
        assert!(missing.fired.is_empty());
        assert!(missing.batch.failures[0].error.contains("UNVERIFIED"));
    }

    #[test]
    fn stale_quote_and_stale_setup_do_not_fire() {
        let mut s = Store::default();
        s.add_rule("aapl", AlertKind::Price { side: PriceSide::Above, level: 10.0 }, 1).unwrap();
        let _ = eval(&mut s, 100, vec![price("AAPL", 9.0, 100, Provenance::Real)], vec![]);
        let stale_now = 100 + STALE_AFTER_SECS + 1;
        let out = eval(&mut s, stale_now, vec![price("AAPL", 12.0, 100, Provenance::Real)], vec![]);
        assert!(out.fired.is_empty());
        assert!(out.batch.stale);
        s.add_rule("msft", AlertKind::Setup { setup: "EP".into(), change: SetupChange::Enter }, 2).unwrap();
        let _ = eval(&mut s, 200, vec![], vec![setup("MSFT", "EP", false, 50, Provenance::Real, false)]);
        let out = eval(&mut s, 210, vec![], vec![setup("MSFT", "EP", true, 60, Provenance::Real, true)]);
        assert!(out.fired.is_empty());
        assert!(out.batch.stale);
    }

    #[test]
    fn partial_failures_keep_successful_events() {
        let mut s = Store::default();
        s.add_rule("aapl", AlertKind::Price { side: PriceSide::Above, level: 10.0 }, 1).unwrap();
        s.add_rule("msft", AlertKind::Price { side: PriceSide::Above, level: 10.0 }, 2).unwrap();
        let _ = eval(&mut s, 3, vec![price("AAPL", 9.0, 3, Provenance::Real)], vec![]);
        let out = eval(&mut s, 4, vec![price("AAPL", 11.0, 4, Provenance::Real)], vec![]);
        assert_eq!(out.fired.len(), 1);
        assert!(out.batch.is_partial());
        assert_eq!(out.batch.failures[0].symbol, "MSFT US Equity");
        assert_eq!(out.batch.events.len(), 1);
    }

    #[test]
    fn persist_roundtrip_and_remove() {
        let dir = std::env::temp_dir().join(format!("tr-alerts-{}-{}", std::process::id(), 7));
        let _ = std::fs::create_dir_all(&dir);
        let path = store_path(&dir);
        let mut s = Store::default();
        s.add_rule("aapl", AlertKind::Price { side: PriceSide::Above, level: 12.5 }, 9).unwrap();
        s.save(&path);
        let mut loaded = Store::load(&path);
        assert_eq!(loaded.rules.len(), 1);
        assert_eq!(loaded.remove("AAPL"), 1);
        assert!(loaded.rules.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn setup_tick_refuses_sample_bars() {
        let bars = vec![Bar::new(1, 1.0, 1.0, 1.0, 1.0, 1.0).with_provenance(Provenance::Sample)];
        let err = setup_tick("AAPL", &bars, &["NEAR_HIGH".into()], 10).unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");
    }

    #[test]
    fn unknown_setup_is_rejected() {
        let mut s = Store::default();
        assert!(s.add_rule("aapl", AlertKind::Setup { setup: "NOPE".into(), change: SetupChange::Enter }, 1).is_err());
        assert_eq!(normalize_setup("near_high").as_deref(), Some("NEAR_HIGH"));
    }

    #[test]
    fn sample_batch_matches_news_isolation_shape() {
        let b = AlertBatch::sample_isolated(1);
        assert!(b.sample && b.events.is_empty() && b.failures.is_empty());
        assert!(!is_stale(1, 10));
        assert!(is_stale(1, 1 + STALE_AFTER_SECS + 1));
    }
}
