//! Append-only experiment identities in the bar database. Report eviction, new
//! bars and changed windows never reset the search family. No memory fallback.
use crate::{
    BarCache,
    backtest_honesty::{self as h, Estimate, HonestReport, HonestySettings, Reason, VariantReport},
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tr_core::Bar;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Protocol {
    Holdout { fraction: f64 },
    WalkForward(HonestySettings),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Experiment {
    Daily {
        strategy: crate::backtest::StrategySpec,
        costs: crate::backtest::CostModel,
        allow_short: bool,
        protocol: Protocol,
    },
    Intraday {
        setup: crate::intraday_backtest::Setup,
        costs: crate::backtest::CostModel,
        allow_short: bool,
        protocol: Protocol,
    },
}
impl Experiment {
    pub fn validate(&self) -> Result<(), Reason> {
        let costs = match self {
            Self::Daily { costs, .. } | Self::Intraday { costs, .. } => costs,
        };
        if !costs.commission_bps.is_finite()
            || !costs.slippage_bps.is_finite()
            || costs.commission_bps < 0.
            || costs.slippage_bps < 0.
            || costs.per_side_frac() >= 1.
        {
            return Err(Reason::InvalidSettings);
        }
        match self.protocol() {
            Protocol::Holdout { fraction } if !fraction.is_finite() || !(0.05..=0.80).contains(fraction) => {
                return Err(Reason::InvalidSettings);
            }
            Protocol::WalkForward(s) if !(2..=6).contains(&s.folds) || s.embargo_sessions > 252 => {
                return Err(Reason::InvalidSettings);
            }
            _ => {}
        }
        if let Self::Daily { strategy, .. } = self {
            use crate::backtest::StrategySpec;
            if crate::backtest::strategy_warmup(strategy) > 10_000 {
                return Err(Reason::InvalidSettings);
            }
            let valid = match strategy {
                StrategySpec::BuyAndHold => true,
                StrategySpec::AdaptiveTrend { atr_n, confirm, method, .. } => {
                    *atr_n > 0 && confirm.is_finite() && *confirm >= 0. && method.is_finite()
                }
                StrategySpec::SmaCross { fast, slow } => *fast > 0 && slow > fast,
                StrategySpec::RsiReversion { period, oversold, overbought } => {
                    *period >= 2
                        && oversold.is_finite()
                        && overbought.is_finite()
                        && *oversold >= 0.
                        && *overbought <= 100.
                        && oversold < overbought
                }
            };
            if !valid {
                return Err(Reason::InvalidSettings);
            }
        }
        Ok(())
    }
    /// Struct/enum serde fields have fixed order; the full descriptor, rather than
    /// its hash, is the primary key. This also makes collisions harmless.
    pub fn canonical(&self) -> String {
        serde_json::to_string(self).expect("validated finite experiment")
    }
    pub fn label(&self) -> String {
        match self {
            Self::Daily { strategy, .. } => strategy.label(),
            Self::Intraday { setup, .. } => setup.label().into(),
        }
    }
    pub fn benchmark(&self) -> bool {
        match self {
            Self::Daily { strategy, .. } => strategy.is_benchmark(),
            Self::Intraday { setup, .. } => setup.is_benchmark(),
        }
    }
    pub fn protocol(&self) -> &Protocol {
        match self {
            Self::Daily { protocol, .. } | Self::Intraday { protocol, .. } => protocol,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trial {
    pub id: String,
    pub experiment: Experiment,
    pub first_seen: i64,
    pub original_digest: String,
    pub original_start: i64,
    pub original_end: i64,
    pub status: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FamilySnapshot {
    pub scope: String,
    pub tracked_since: i64,
    pub revision: u64,
    pub digest: String,
    pub trials: Vec<Trial>,
}
pub fn scope(symbol: &str, intraday: bool, tf: tr_core::Timeframe, basis: &str) -> String {
    serde_json::to_string(&(
        crate::security(symbol).key(),
        if intraday { "IBT" } else { "BT" },
        tf.label(),
        basis,
        h::METHOD,
    ))
    .unwrap()
}
pub(crate) fn init(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS backtest_trial_families (
      scope TEXT PRIMARY KEY NOT NULL, tracked_since INTEGER NOT NULL, revision INTEGER NOT NULL
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS backtest_trials (
      family TEXT NOT NULL, descriptor TEXT NOT NULL, first_seen INTEGER NOT NULL,
      original_digest TEXT NOT NULL, original_start INTEGER NOT NULL, original_end INTEGER NOT NULL,
      status TEXT NOT NULL, PRIMARY KEY (family,descriptor)
    ) WITHOUT ROWID;",
    )?;
    Ok(())
}
impl BarCache {
    /// Atomic upsert + immutable snapshot. The caller releases its database lock
    /// before replay. Repeated identical requests do not advance revision.
    pub fn backtest_register(
        &mut self,
        scope: &str,
        experiments: &[Experiment],
        bars: &[Bar],
        now: i64,
    ) -> anyhow::Result<(FamilySnapshot, bool)> {
        if self.conn.path().is_none_or(|p| p.is_empty()) {
            anyhow::bail!("trial register is not durable");
        }
        self.backtest_register_inner(scope, experiments, bars, now)
    }
    fn backtest_register_inner(
        &mut self,
        scope: &str,
        experiments: &[Experiment],
        bars: &[Bar],
        now: i64,
    ) -> anyhow::Result<(FamilySnapshot, bool)> {
        for e in experiments {
            e.validate().map_err(|r| anyhow::anyhow!(r))?;
        }
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("INSERT OR IGNORE INTO backtest_trial_families VALUES (?1,?2,0)", params![scope, now])?;
        let digest = format!("{:016x}", crate::stable_hash::bars_digest(bars));
        let mut changed = false;
        for experiment in experiments {
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO backtest_trials VALUES (?1,?2,?3,?4,?5,?6,'attempted')",
                params![
                    scope,
                    experiment.canonical(),
                    now,
                    digest,
                    bars.first().map_or(0, |b| b.ts),
                    bars.last().map_or(0, |b| b.ts)
                ],
            )?;
            changed |= inserted > 0;
            if inserted > 0 {
                tx.execute("UPDATE backtest_trial_families SET revision=revision+1 WHERE scope=?1", [scope])?;
            }
        }
        let family = read_snapshot(&tx, scope)?;
        tx.commit()?;
        Ok((family, changed))
    }
    pub fn backtest_family(&self, scope: &str) -> anyhow::Result<FamilySnapshot> {
        read_snapshot(&self.conn, scope)
    }
    pub fn backtest_status(&mut self, scope: &str, id: &str, status: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE backtest_trials SET status=?3 WHERE family=?1 AND descriptor=?2",
            params![scope, id, status],
        )?;
        Ok(())
    }
}
fn read_snapshot(conn: &rusqlite::Connection, scope: &str) -> anyhow::Result<FamilySnapshot> {
    let (tracked_since, revision) =
        conn.query_row("SELECT tracked_since,revision FROM backtest_trial_families WHERE scope=?1", [scope], |r| {
            Ok((r.get(0)?, r.get::<_, i64>(1)? as u64))
        })?;
    let mut st=conn.prepare("SELECT descriptor,first_seen,original_digest,original_start,original_end,status FROM backtest_trials WHERE family=?1 ORDER BY descriptor")?;
    let raw = st
        .query_map([scope], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut trials = Vec::new();
    let mut hash = crate::stable_hash::Fnv64::default();
    for (id, first_seen, original_digest, original_start, original_end, status) in raw {
        hash.write_str(&id);
        trials.push(Trial {
            experiment: serde_json::from_str(&id)?,
            id,
            first_seen,
            original_digest,
            original_start,
            original_end,
            status,
        });
    }
    Ok(FamilySnapshot {
        scope: scope.into(),
        tracked_since,
        revision,
        digest: format!("{:016x}", hash.finish()),
        trials,
    })
}

pub fn daily_experiments(s: &crate::backtest::RunSettings, wf: HonestySettings) -> Vec<Experiment> {
    crate::backtest::ensure_benchmark(s.strategies.clone())
        .into_iter()
        .flat_map(|strategy| {
            [Protocol::WalkForward(wf), Protocol::Holdout { fraction: s.holdout_frac }].map(|protocol| {
                Experiment::Daily { strategy: strategy.clone(), costs: s.costs, allow_short: s.allow_short, protocol }
            })
        })
        .collect()
}
pub fn intraday_experiments(s: &crate::intraday_backtest::RunSettings, wf: HonestySettings) -> Vec<Experiment> {
    crate::intraday_backtest::ensure_benchmark(s.setups.clone())
        .into_iter()
        .flat_map(|setup| {
            [Protocol::WalkForward(wf), Protocol::Holdout { fraction: s.holdout_frac }]
                .map(|protocol| Experiment::Intraday { setup, costs: s.costs, allow_short: s.allow_short, protocol })
        })
        .collect()
}
/// Immutable worker-side replay; all declared variants are inspected, never ranked.
/// Current WF equity can survive an unavailable register or an oversized family.
pub fn compute(
    bars: &[Bar],
    tf: tr_core::Timeframe,
    settings: HonestySettings,
    current: &[Experiment],
    family: Estimate<FamilySnapshot>,
    admission: String,
) -> HonestReport {
    let mut results = BTreeMap::new();
    let replay = |e: &Experiment| {
        e.validate()?;
        match e {
            Experiment::Daily { strategy, costs, allow_short, protocol } => {
                crate::backtest::honest_evaluation(bars, strategy, *costs, *allow_short, protocol)
            }
            Experiment::Intraday { setup, costs, allow_short, protocol } => {
                crate::intraday_backtest::honest_evaluation(bars, tf, *setup, *costs, *allow_short, protocol)
            }
        }
    };
    let own_budget =
        bars.len().saturating_mul(current.len()).saturating_mul(settings.folds.saturating_add(1)) > h::MAX_OPERATIONS;
    let mut variants = Vec::new();
    for e in current.iter().filter(|e| matches!(e.protocol(), Protocol::WalkForward(_))) {
        let id = e.canonical();
        let result = if own_budget { Err(Reason::WorkBudgetExceeded { trials: current.len() }) } else { replay(e) };
        variants.push(VariantReport {
            id: id.clone(),
            label: e.label(),
            benchmark: e.benchmark(),
            evaluation: result.clone().into(),
        });
        results.insert(id, result);
    }
    let family_statistics = match family.value() {
        None => Estimate::Unavailable(Reason::RegisterUnavailable),
        Some(f)
            if f.trials.len() > h::MAX_TRIALS
                || bars.len().saturating_mul(f.trials.len()).saturating_mul(14) > h::MAX_OPERATIONS =>
        {
            Estimate::Unavailable(Reason::WorkBudgetExceeded { trials: f.trials.len() })
        }
        Some(f) => {
            for t in &f.trials {
                results.entry(t.id.clone()).or_insert_with(|| replay(&t.experiment));
            }
            let tapes = f
                .trials
                .iter()
                .map(|t| (t.id.clone(), results[&t.id].as_ref().map(|r| r.returns.clone()).map_err(Clone::clone)))
                .collect();
            h::compare_family(&tapes).into()
        }
    };
    let replay_statuses = results
        .iter()
        .map(|(id, r)| {
            (
                id.clone(),
                match r {
                    Ok(_) => "computed".into(),
                    Err(e) => format!("failed: {e}"),
                },
            )
        })
        .collect();
    let closed_through =
        variants.first().and_then(|v| v.evaluation.value()).and_then(|e| e.returns.last()).map_or_else(
            || {
                if tf.is_intraday() {
                    crate::intraday_backtest::admitted_through(tf, bars).unwrap_or(0)
                } else {
                    bars.last().map_or(0, |b| crate::backtest::daily_close(b.ts))
                }
            },
            |p| p.period_end,
        );
    let inputs = current
        .iter()
        .filter(|e| matches!(e.protocol(), Protocol::WalkForward(_)))
        .map(|e| match e {
            Experiment::Daily { strategy, costs, allow_short, .. } => {
                format!("{}: {}; {}; shorts {}", strategy.label(), strategy.params_line(), costs.label(), allow_short)
            }
            Experiment::Intraday { setup, costs, allow_short, .. } => format!(
                "{}: {}; {}; shorts {}",
                setup.label(),
                setup.params_line(*allow_short),
                costs.label(),
                allow_short
            ),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let execution = format!(
        "Fixed variants; {} native bars; net simple session returns; risk-free 0%; 252 sessions/year. Historical OOS replay is not new unseen evidence. Each test starts flat; daily test-end liquidation pays exit costs. No future training in a forward fold. Serial dependence is not corrected by moment-based PSR/DSR.\nAccepted inputs:\n{}",
        tf.label(),
        inputs
    );
    HonestReport {
        replay_statuses,
        methodology_version: h::METHOD.into(),
        settings,
        trial_family: family,
        family_statistics,
        variants,
        closed_through,
        admission,
        execution,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn experiment(cost: f64, protocol: Protocol) -> Experiment {
        Experiment::Daily {
            strategy: crate::backtest::StrategySpec::BuyAndHold,
            costs: crate::backtest::CostModel { commission_bps: cost, slippage_bps: 0. },
            allow_short: false,
            protocol,
        }
    }
    #[test]
    fn backtest_trials_dedup_revision_reopen_failed_attempt_and_new_history() {
        let path = std::env::temp_dir().join(format!(
            "tr-trials-{}-{}.sqlite",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let wf = Protocol::WalkForward(Default::default());
        let a = experiment(0., wf.clone());
        let mut cache = BarCache::open(&path).unwrap();
        let bars = [Bar::new(1, 1., 1., 1., 1., 1.)];
        let (first, changed) = cache.backtest_register("scope", std::slice::from_ref(&a), &bars, 100).unwrap();
        assert!(changed);
        assert_eq!(first.revision, 1);
        cache.backtest_status("scope", &a.canonical(), "failed: ruin").unwrap();
        let (again, changed) = cache.backtest_register("scope", std::slice::from_ref(&a), &[], 200).unwrap();
        assert!(!changed);
        assert_eq!(again.revision, 1);
        assert_eq!(again.tracked_since, 100);
        assert_eq!(again.trials[0].original_start, 1);
        assert_eq!(again.trials[0].status, "failed: ruin");
        drop(cache);
        let mut cache = BarCache::open(&path).unwrap();
        assert_eq!(cache.backtest_family("scope").unwrap(), again);
        let mut short = a.clone();
        if let Experiment::Daily { allow_short, .. } = &mut short {
            *allow_short = true;
        }
        let mut param = a.clone();
        if let Experiment::Daily { strategy, .. } = &mut param {
            *strategy = crate::backtest::StrategySpec::SmaCross { fast: 10, slow: 40 };
        }
        let additions = vec![a, experiment(1., wf), experiment(0., Protocol::Holdout { fraction: 0.2 }), short, param];
        let (more, changed) = cache.backtest_register("scope", &additions, &[], 300).unwrap();
        assert!(changed);
        assert_eq!(more.revision, 5);
        assert_eq!(more.trials.len(), 5);
        let (_, changed) =
            cache.backtest_register("scope", &additions.into_iter().rev().collect::<Vec<_>>(), &[], 400).unwrap();
        assert!(!changed);
        drop(cache);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn backtest_trials_migration_preserves_old_bars_and_memory_is_not_durable() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE bars(symbol TEXT,tf TEXT,ts INTEGER,o REAL,h REAL,l REAL,c REAL,v REAL,PRIMARY KEY(symbol,tf,ts));INSERT INTO bars VALUES('AAPL','1D',1,1,1,1,1,1);").unwrap();
        let mut cache = BarCache::init(conn).unwrap();
        assert_eq!(cache.load("AAPL", tr_core::Timeframe::D1, 10).unwrap().len(), 1);
        assert!(cache.backtest_register("scope", &[], &[], 0).is_err());
        assert!(
            cache
                .backtest_register_inner("scope", &[experiment(0., Protocol::WalkForward(Default::default()))], &[], 1)
                .is_ok()
        );
    }
    #[test]
    fn backtest_trials_scope_excludes_dates_counts_and_panes() {
        let a = scope("AAPL", false, tr_core::Timeframe::D1, "yahoo archive");
        assert_eq!(a, scope("AAPL US Equity", false, tr_core::Timeframe::D1, "yahoo archive"));
        assert_ne!(a, scope("MSFT", false, tr_core::Timeframe::D1, "yahoo archive"));
        assert_ne!(a, scope("AAPL", true, tr_core::Timeframe::M5, "yahoo archive"));
        assert_ne!(a, scope("AAPL", false, tr_core::Timeframe::D1, "other source"));
        assert_ne!(
            scope("AAPL", true, tr_core::Timeframe::M5, "yahoo archive"),
            scope("AAPL", true, tr_core::Timeframe::M15, "yahoo archive")
        );
    }
    #[test]
    fn backtest_trials_hidden_variants_and_failed_replays_remain_in_the_family() {
        let mut date = chrono::NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
        let mut bars = Vec::new();
        for i in 0..160 {
            let price = 100. + i as f64 * 0.1 + (i as f64 * 0.4).sin();
            bars.push(
                Bar::new(
                    date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(),
                    price,
                    price + 1.,
                    price - 1.,
                    price + 0.2,
                    100.,
                )
                .with_provenance(tr_core::Provenance::Real),
            );
            date = crate::history::calendar::next_session(date);
        }
        let wf = HonestySettings::default();
        let current = daily_experiments(
            &crate::backtest::RunSettings {
                strategies: vec![crate::backtest::StrategySpec::BuyAndHold],
                ..Default::default()
            },
            wf,
        );
        let mut hidden = experiment(12., Protocol::WalkForward(wf));
        let mut cache = BarCache::open_in_memory().unwrap();
        cache.backtest_register_inner("scope", std::slice::from_ref(&hidden), &bars, 1).unwrap();
        let (family, _) = cache.backtest_register_inner("scope", &current, &bars, 2).unwrap();
        assert_eq!(family.trials.len(), 3);
        let report = compute(&bars, tr_core::Timeframe::D1, wf, &current, Estimate::Available(family), "test".into());
        assert_eq!(report.variants.len(), 1);
        assert_eq!(report.replay_statuses.len(), 3);
        assert_eq!(report.family_statistics.value().unwrap().comparisons.len(), 3);
        assert!(report.family_statistics.value().unwrap().comparisons.contains_key(&hidden.canonical()));

        // Valid declaration, but its warm-up cannot be replayed on this history.
        if let Experiment::Daily { strategy, .. } = &mut hidden {
            *strategy = crate::backtest::StrategySpec::SmaCross { fast: 10, slow: 1000 };
        }
        let (family, _) = cache.backtest_register_inner("scope", &[hidden.clone()], &bars, 3).unwrap();
        assert_eq!(family.trials.len(), 4);
        let report = compute(&bars, tr_core::Timeframe::D1, wf, &current, Estimate::Available(family), "test".into());
        assert!(report.variants[0].evaluation.value().is_some());
        assert_eq!(report.family_statistics, Estimate::Unavailable(Reason::IncompleteFamily));
        assert!(report.replay_statuses[&hidden.canonical()].starts_with("failed:"));
    }
}
