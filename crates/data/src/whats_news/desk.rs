//! Watchlist-scoped PM Desk: leaders, group performance, and risk-based size.
//!
//! Rankings compare only the selected watchlist group. They are not SCAN's
//! archive-universe RS, not a published rating, and not SPY-relative.
//! Sample or unverified bars are refused. Short history is skipped, never padded.

use super::darvas;
use tr_core::{Bar, Provenance};

/// Yahoo / archive ticker for a market key (`AAPL US Equity` → `AAPL`).
pub fn archive_ticker(market_key: &str) -> String {
    let sec = crate::security(market_key);
    crate::history::universe::normalize(&crate::yahoo::yahoo_symbol(&sec))
        .unwrap_or_else(|_| sec.ticker.to_ascii_uppercase().replace('.', "-"))
}

/// Wilder ATR lookback used as the stop fallback.
pub const ATR_LEN: usize = 14;
/// ATR multiples below (long) or above (short) entry when no user/box stop exists.
pub const ATR_MULT: f64 = 2.0;
/// Bars needed for a 21-bar close return (`close[n-1] / close[n-22] - 1`).
pub const RETURN_BARS: usize = 22;

/// How the planned stop was chosen. User and box beat ATR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSource {
    User,
    Box,
    Atr,
}

impl StopSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "user stop",
            Self::Box => "box stop (20-bar high / 3-bar confirmation, same rule as SCAN)",
            Self::Atr => "ATR fallback (2 × ATR(14) from entry)",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Box => "box",
            Self::Atr => "ATR",
        }
    }
}

/// One watchlist member after archive reads. `skip` means no rank, not a fabricated number.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberRow {
    pub key: String,
    pub ticker: String,
    pub last_date: Option<String>,
    pub close: Option<f64>,
    pub change_pct: Option<f64>,
    pub ret_21d_pct: Option<f64>,
    pub box_top: Option<f64>,
    pub box_bottom: Option<f64>,
    pub box_state: Option<&'static str>,
    pub atr14: Option<f64>,
    pub provenance: Provenance,
    pub skip: Option<String>,
    /// Competition rank inside this watchlist's same-date cohort (`None` if skipped).
    pub watchlist_rank: Option<usize>,
    /// Size of that cohort (same last bar date in this group).
    pub watchlist_n: Option<usize>,
}

impl MemberRow {
    pub fn from_bars(key: &str, ticker: &str, bars: &[Bar]) -> Result<Self, String> {
        Bar::require_verified(bars, "PM Desk")?;
        if bars.is_empty() {
            return Ok(Self::skipped(key, ticker, Provenance::Missing, "no daily history"));
        }
        let last = *bars.last().unwrap();
        let last_date = chrono::DateTime::from_timestamp(last.ts, 0).map(|t| t.date_naive().to_string());
        if bars.len() < RETURN_BARS {
            return Ok(Self {
                key: key.into(),
                ticker: ticker.into(),
                last_date,
                close: Some(last.close).filter(|v| v.is_finite()),
                change_pct: None,
                ret_21d_pct: None,
                box_top: None,
                box_bottom: None,
                box_state: None,
                atr14: last_atr(bars, ATR_LEN),
                provenance: Bar::provenance(bars),
                skip: Some(format!(
                    "insufficient daily history: need {RETURN_BARS} bars for a 21-bar return, have {}",
                    bars.len()
                )),
                watchlist_rank: None,
                watchlist_n: None,
            });
        }
        let prev = bars[bars.len() - 2].close;
        let base = bars[bars.len() - RETURN_BARS].close;
        if !(last.close.is_finite() && prev.is_finite() && prev > 0.0 && base.is_finite() && base > 0.0) {
            return Ok(Self::skipped(key, ticker, Bar::provenance(bars), "invalid close for 21-bar return"));
        }
        let box_ = darvas(bars);
        Ok(Self {
            key: key.into(),
            ticker: ticker.into(),
            last_date,
            close: Some(last.close),
            change_pct: Some((last.close / prev - 1.0) * 100.0),
            ret_21d_pct: Some((last.close / base - 1.0) * 100.0),
            box_top: box_.map(|b| b.0),
            box_bottom: box_.map(|b| b.1),
            box_state: box_.map(|b| b.2),
            atr14: last_atr(bars, ATR_LEN),
            provenance: Bar::provenance(bars),
            skip: None,
            watchlist_rank: None,
            watchlist_n: None,
        })
    }

    pub fn skipped(key: &str, ticker: &str, provenance: Provenance, reason: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ticker: ticker.into(),
            last_date: None,
            close: None,
            change_pct: None,
            ret_21d_pct: None,
            box_top: None,
            box_bottom: None,
            box_state: None,
            atr14: None,
            provenance,
            skip: Some(reason.into()),
            watchlist_rank: None,
            watchlist_n: None,
        }
    }

    pub fn rank_label(&self) -> String {
        match (self.watchlist_rank, self.watchlist_n, &self.last_date) {
            (Some(r), Some(n), Some(d)) => {
                format!(
                    "watchlist rank {r}/{n} as of {d} (this group only; not archive-universe SCAN rank, not SPY-relative)"
                )
            }
            (Some(r), Some(n), None) => {
                format!("watchlist rank {r}/{n} (this group only; not archive-universe SCAN rank, not SPY-relative)")
            }
            _ => self.skip.clone().unwrap_or_else(|| "unranked in this watchlist".into()),
        }
    }
}

/// Group-level 21-bar return summary for the selected watchlist only.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupSummary {
    pub group: String,
    pub members: usize,
    pub ranked: usize,
    pub skipped: usize,
    pub advancing: usize,
    pub declining: usize,
    pub unchanged: usize,
    pub mean_ret: Option<f64>,
    pub median_ret: Option<f64>,
    pub as_of: Option<String>,
    pub mixed_dates: bool,
}

impl GroupSummary {
    pub fn from_rows(group: &str, rows: &[MemberRow]) -> Self {
        let ranked: Vec<&MemberRow> = rows.iter().filter(|r| r.ret_21d_pct.is_some() && r.skip.is_none()).collect();
        let mut rets: Vec<f64> = ranked.iter().filter_map(|r| r.ret_21d_pct).collect();
        let advancing = rets.iter().filter(|v| **v > 0.0).count();
        let declining = rets.iter().filter(|v| **v < 0.0).count();
        let unchanged = rets.len() - advancing - declining;
        let mean_ret = (!rets.is_empty()).then(|| rets.iter().sum::<f64>() / rets.len() as f64);
        let median_ret = (!rets.is_empty()).then(|| {
            rets.sort_by(|a, b| a.total_cmp(b));
            let mid = rets.len() / 2;
            if rets.len() % 2 == 1 { rets[mid] } else { (rets[mid - 1] + rets[mid]) / 2.0 }
        });
        let mut dates: Vec<&str> = ranked.iter().filter_map(|r| r.last_date.as_deref()).collect();
        dates.sort_unstable();
        dates.dedup();
        let as_of = dates
            .iter()
            .max_by_key(|d| ranked.iter().filter(|r| r.last_date.as_deref() == Some(**d)).count())
            .copied()
            .map(str::to_string);
        Self {
            group: group.into(),
            members: rows.len(),
            ranked: ranked.len(),
            skipped: rows.iter().filter(|r| r.skip.is_some()).count(),
            advancing,
            declining,
            unchanged,
            mean_ret,
            median_ret,
            as_of,
            mixed_dates: dates.len() > 1,
        }
    }
}

/// Rank eligible members inside the selected watchlist. Same last-bar date only;
/// ties share competition rank. Different dates never compete.
pub fn rank_watchlist(rows: &mut [MemberRow]) {
    for row in rows.iter_mut() {
        row.watchlist_rank = None;
        row.watchlist_n = None;
    }
    let mut cohorts = std::collections::BTreeMap::<Option<String>, Vec<usize>>::new();
    for (i, row) in rows.iter().enumerate() {
        if row.skip.is_none() && row.ret_21d_pct.is_some() {
            cohorts.entry(row.last_date.clone()).or_default().push(i);
        }
    }
    for indices in cohorts.values_mut() {
        indices.sort_by(|a, b| rows[*b].ret_21d_pct.unwrap().total_cmp(&rows[*a].ret_21d_pct.unwrap()));
        let count = indices.len();
        let mut rank = 1;
        let mut previous = None;
        for (place, i) in indices.iter().enumerate() {
            let ret = rows[*i].ret_21d_pct.unwrap();
            if previous.is_some_and(|prev| prev != ret) {
                rank = place + 1;
            }
            previous = Some(ret);
            rows[*i].watchlist_rank = Some(rank);
            rows[*i].watchlist_n = Some(count);
        }
    }
    rows.sort_by(|a, b| match (a.watchlist_rank, b.watchlist_rank) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.ticker.cmp(&b.ticker)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.ticker.cmp(&b.ticker),
    });
}

/// Prefer a user stop, then a box level on the correct side of entry, then ATR.
pub fn resolve_stop(
    user: Option<f64>,
    box_level: Option<f64>,
    atr: Option<f64>,
    entry: f64,
    long: bool,
    atr_mult: f64,
) -> Option<(f64, StopSource)> {
    let usable = |p: f64| p.is_finite() && p > 0.0 && (p - entry).abs() > 1e-9;
    if let Some(p) = user.filter(|&p| usable(p)) {
        return Some((p, StopSource::User));
    }
    let box_ok = |p: f64| usable(p) && if long { p < entry } else { p > entry };
    if let Some(p) = box_level.filter(|&p| box_ok(p)) {
        return Some((p, StopSource::Box));
    }
    let offset = atr.filter(|a| a.is_finite() && *a > 0.0 && atr_mult.is_finite() && atr_mult > 0.0)?;
    let stop = if long { entry - atr_mult * offset } else { entry + atr_mult * offset };
    usable(stop).then_some((stop, StopSource::Atr))
}

/// Box level used as a stop: bottom for a long, top for a short.
pub fn box_stop(row: &MemberRow, long: bool) -> Option<f64> {
    if long { row.box_bottom } else { row.box_top }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RiskPlan {
    pub entry: f64,
    pub stop: f64,
    pub target: Option<f64>,
    pub dollar_risk: f64,
    pub stop_source: StopSource,
    pub long: bool,
    pub risk_per_share: f64,
    pub shares: f64,
    pub whole_shares: f64,
    pub position_value: f64,
    pub r_multiple: Option<f64>,
}

/// Desk prices copied onto an order ticket. They describe the plan that was reviewed.
/// They are not a stop order, a limit, or a bracket, and nothing here is submitted by itself.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanContext {
    pub entry: f64,
    pub stop: f64,
    pub target: Option<f64>,
    pub dollar_risk: f64,
    /// Exact whole shares for a long ticket. This is `floor(shares)`, never raised to 1.
    pub quantity: u64,
}

/// Why a reviewed plan cannot open an order ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffBlock {
    /// A short plan would need short-opening. A sell ticket only closes shares already held.
    Short,
    /// `floor(shares)` is 0, so there is no whole share to send.
    ZeroShares,
    /// Share count is non-finite, negative, or not the plan's floored quantity.
    InvalidQuantity,
    /// Prices or the reward side are not a plan that can be shown as context.
    InvalidPlan,
}

impl HandoffBlock {
    pub fn explain(self) -> &'static str {
        match self {
            Self::Short => {
                "Short plans cannot open an order ticket. A sell order closes shares you already hold; it does not open a short. Short-opening is not supported yet."
            }
            Self::ZeroShares => {
                "Handoff disabled: whole-share quantity is 0. Increase dollar risk or tighten the stop before opening a ticket."
            }
            Self::InvalidQuantity => "Handoff disabled: whole-share quantity is invalid.",
            Self::InvalidPlan => "Handoff disabled: the plan prices are invalid.",
        }
    }
}

impl RiskPlan {
    /// Long ticket for this plan's exact whole-share quantity, or why handoff is disabled.
    ///
    /// Short plans are refused: the ticket's sell side closes a long and does not open a short.
    /// A sub-one-share budget stays a plan (the size is still shown) but does not become a ticket.
    pub fn handoff(&self) -> Result<PlanContext, HandoffBlock> {
        if !self.long {
            return Err(HandoffBlock::Short);
        }
        if !self.shares.is_finite() || self.shares < 0.0 || !self.whole_shares.is_finite() || self.whole_shares < 0.0 {
            return Err(HandoffBlock::InvalidQuantity);
        }
        if !self.entry.is_finite()
            || self.entry <= 0.0
            || !self.stop.is_finite()
            || self.stop >= self.entry
            || !self.dollar_risk.is_finite()
            || self.dollar_risk <= 0.0
            || !self.risk_per_share.is_finite()
            || self.risk_per_share <= 0.0
            || !self.position_value.is_finite()
            || self.target.is_some_and(|t| !t.is_finite() || t <= self.entry)
            || self.r_multiple.is_some_and(|r| !r.is_finite() || r <= 0.0)
        {
            return Err(HandoffBlock::InvalidPlan);
        }
        let whole = self.shares.floor();
        if !whole.is_finite() || (self.whole_shares - whole).abs() > 1e-6 {
            return Err(HandoffBlock::InvalidQuantity);
        }
        if whole < 1.0 {
            return Err(HandoffBlock::ZeroShares);
        }
        if whole > u64::MAX as f64 {
            return Err(HandoffBlock::InvalidQuantity);
        }
        let quantity = whole as u64;
        if (quantity as f64 - whole).abs() > 1e-6 {
            return Err(HandoffBlock::InvalidQuantity);
        }
        Ok(PlanContext {
            entry: self.entry,
            stop: self.stop,
            target: self.target,
            dollar_risk: self.dollar_risk,
            quantity,
        })
    }
}

/// Shares = dollar risk / |entry − stop|. Fractional shares are kept; `whole_shares` floors
/// and is not raised to 1. A target, when given, must be a positive finite price strictly on
/// the profitable side of entry (above a long entry, below a short entry). Equal-entry and
/// wrong-side targets are rejected, so a losing target is never reported as a positive R.
pub fn size_position(
    entry: f64,
    stop: f64,
    target: Option<f64>,
    dollar_risk: f64,
    stop_source: StopSource,
) -> Result<RiskPlan, String> {
    if !entry.is_finite() || entry <= 0.0 {
        return Err("entry must be a positive price".into());
    }
    if !stop.is_finite() || stop <= 0.0 {
        return Err("stop must be a positive price".into());
    }
    if (entry - stop).abs() < 1e-9 {
        return Err("stop must differ from entry".into());
    }
    if !dollar_risk.is_finite() || dollar_risk <= 0.0 {
        return Err("dollar risk must be positive".into());
    }
    let long = stop < entry;
    let risk_per_share = (entry - stop).abs();
    if !risk_per_share.is_finite() || risk_per_share <= 0.0 {
        return Err("stop distance is invalid".into());
    }
    let shares = dollar_risk / risk_per_share;
    let whole_shares = shares.floor();
    let position_value = shares * entry;
    if !shares.is_finite() || !whole_shares.is_finite() || shares < 0.0 {
        return Err("share quantity is invalid".into());
    }
    if !position_value.is_finite() {
        return Err("position value is invalid".into());
    }
    let (target, r_multiple) = match target {
        None => (None, None),
        Some(t) => {
            if !t.is_finite() {
                return Err("target must be a finite price".into());
            }
            if t <= 0.0 {
                return Err("target must be a positive price".into());
            }
            if (t - entry).abs() < 1e-9 {
                return Err("target must differ from entry".into());
            }
            let reward = if long { t - entry } else { entry - t };
            if !reward.is_finite() {
                return Err("reward multiple is invalid".into());
            }
            if reward <= 0.0 {
                return Err(if long {
                    "long target must be above entry".into()
                } else {
                    "short target must be below entry".into()
                });
            }
            let r_multiple = reward / risk_per_share;
            if !r_multiple.is_finite() || r_multiple <= 0.0 {
                return Err("reward multiple is invalid".into());
            }
            (Some(t), Some(r_multiple))
        }
    };
    Ok(RiskPlan {
        entry,
        stop,
        target,
        dollar_risk,
        stop_source,
        long,
        risk_per_share,
        shares,
        whole_shares,
        position_value,
        r_multiple,
    })
}

/// Latest Wilder ATR(`n`). Needs `n + 1` bars so the first true range has a previous close.
pub fn last_atr(bars: &[Bar], n: usize) -> Option<f64> {
    if n == 0 || bars.len() <= n {
        return None;
    }
    let tr = |i: usize| {
        let (h, l) = (bars[i].high, bars[i].low);
        let pc = bars[i - 1].close;
        (h - l).max((h - pc).abs()).max((l - pc).abs())
    };
    let mut atr = (1..=n).map(tr).sum::<f64>() / n as f64;
    for i in (n + 1)..bars.len() {
        atr = (atr * (n - 1) as f64 + tr(i)) / n as f64;
    }
    atr.is_finite().then_some(atr)
}

/// Horizontal-line prices from chart drawings, used as user-stop candidates.
pub fn drawing_stops(prices: &[f64]) -> Vec<f64> {
    let mut out: Vec<f64> = prices.iter().copied().filter(|p| p.is_finite() && *p > 0.0).collect();
    out.sort_by(|a, b| a.total_cmp(b));
    out.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(i: i64, close: f64, high: f64, low: f64, provenance: Provenance) -> Bar {
        Bar::new(i * 86_400, close, high, low, close, 1_000.0).with_provenance(provenance)
    }

    fn series(n: usize, last: f64) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let c = if i + 1 == n { last } else { 100.0 + i as f64 };
                bar(i as i64, c, c + 1.0, c - 1.0, Provenance::Real)
            })
            .collect()
    }

    #[test]
    fn ranks_only_the_supplied_watchlist_and_labels_the_scope() {
        let mut rows = vec![
            MemberRow::from_bars("NVDA US Equity", "NVDA", &series(30, 140.0)).unwrap(),
            MemberRow::from_bars("AAPL US Equity", "AAPL", &series(30, 110.0)).unwrap(),
            MemberRow::from_bars("MSFT US Equity", "MSFT", &series(30, 90.0)).unwrap(),
        ];
        rank_watchlist(&mut rows);
        assert_eq!(rows.iter().map(|r| r.ticker.as_str()).collect::<Vec<_>>(), ["NVDA", "AAPL", "MSFT"]);
        assert_eq!(rows[0].watchlist_rank, Some(1));
        assert_eq!(rows[1].watchlist_rank, Some(2));
        assert_eq!(rows[2].watchlist_rank, Some(3));
        assert_eq!(rows[0].watchlist_n, Some(3));
        assert!(rows[0].rank_label().contains("watchlist rank 1/3"));
        assert!(rows[0].rank_label().contains("this group only"));
        assert!(rows[0].rank_label().contains("not archive-universe"));
        assert!(rows[0].rank_label().contains("not SPY-relative"));
        let summary = GroupSummary::from_rows("Growth", &rows);
        assert_eq!((summary.members, summary.ranked, summary.skipped), (3, 3, 0));
        assert_eq!(summary.group, "Growth");
        assert!(summary.mean_ret.is_some());
    }

    #[test]
    fn ties_share_rank_and_different_dates_do_not_compete() {
        let mut a = MemberRow::from_bars("A", "A", &series(30, 120.0)).unwrap();
        let mut b = MemberRow::from_bars("B", "B", &series(30, 120.0)).unwrap();
        let mut late = MemberRow::from_bars("C", "C", &series(30, 200.0)).unwrap();
        late.last_date = Some("2099-01-01".into());
        a.last_date = Some("2026-10-24".into());
        b.last_date = Some("2026-10-24".into());
        let mut rows = vec![a, b, late];
        rank_watchlist(&mut rows);
        let same: Vec<_> = rows.iter().filter(|r| r.last_date.as_deref() == Some("2026-10-24")).collect();
        assert_eq!(same.len(), 2);
        assert!(same.iter().all(|r| r.watchlist_rank == Some(1) && r.watchlist_n == Some(2)));
        let other = rows.iter().find(|r| r.ticker == "C").unwrap();
        assert_eq!((other.watchlist_rank, other.watchlist_n), (Some(1), Some(1)));
    }

    #[test]
    fn sample_bars_are_refused_and_short_history_is_not_padded() {
        let sample: Vec<Bar> = series(30, 110.0)
            .into_iter()
            .map(|b| Bar::new(b.ts, b.open, b.high, b.low, b.close, b.volume).with_provenance(Provenance::Sample))
            .collect();
        assert!(MemberRow::from_bars("X", "X", &sample).unwrap_err().contains("SAMPLE"));
        let short = MemberRow::from_bars("Y", "Y", &series(10, 100.0)).unwrap();
        assert!(short.skip.as_deref().unwrap().contains("need 22"));
        assert!(short.ret_21d_pct.is_none());
        assert!(short.watchlist_rank.is_none());
    }

    #[test]
    fn stop_prefers_user_then_box_then_atr() {
        let entry = 100.0;
        assert_eq!(resolve_stop(Some(95.0), Some(90.0), Some(2.0), entry, true, 2.0), Some((95.0, StopSource::User)));
        assert_eq!(resolve_stop(None, Some(90.0), Some(2.0), entry, true, 2.0), Some((90.0, StopSource::Box)));
        assert_eq!(resolve_stop(None, Some(110.0), Some(2.0), entry, true, 2.0), Some((96.0, StopSource::Atr)));
        assert_eq!(resolve_stop(None, Some(110.0), Some(2.0), entry, false, 2.0), Some((110.0, StopSource::Box)));
        assert_eq!(resolve_stop(None, Some(90.0), Some(2.0), entry, false, 2.0), Some((104.0, StopSource::Atr)));
        assert!(resolve_stop(None, None, None, entry, true, 2.0).is_none());
        assert_eq!(StopSource::Box.label().contains("SCAN"), true);
        assert_eq!(StopSource::Atr.short(), "ATR");
    }

    #[test]
    fn position_size_is_dollar_risk_over_stop_distance() {
        let plan = size_position(100.0, 95.0, Some(110.0), 250.0, StopSource::Box).unwrap();
        assert!(plan.long);
        assert!((plan.risk_per_share - 5.0).abs() < 1e-12);
        assert!((plan.shares - 50.0).abs() < 1e-12);
        assert_eq!(plan.whole_shares, 50.0);
        assert!((plan.position_value - 5_000.0).abs() < 1e-9);
        assert!((plan.r_multiple.unwrap() - 2.0).abs() < 1e-12);
        assert_eq!(plan.stop_source, StopSource::Box);
        let short = size_position(50.0, 55.0, Some(40.0), 100.0, StopSource::User).unwrap();
        assert!(!short.long);
        assert!((short.shares - 20.0).abs() < 1e-12);
        assert!((short.r_multiple.unwrap() - 2.0).abs() < 1e-12);
        assert!(size_position(100.0, 100.0, None, 100.0, StopSource::User).is_err());
        assert!(size_position(100.0, 95.0, None, 0.0, StopSource::User).is_err());
    }

    #[test]
    fn wrong_side_and_equal_entry_targets_are_rejected() {
        let long_wrong = size_position(100.0, 95.0, Some(90.0), 250.0, StopSource::User).unwrap_err();
        assert!(long_wrong.contains("above entry"), "{long_wrong}");
        let equal = size_position(100.0, 95.0, Some(100.0), 250.0, StopSource::User).unwrap_err();
        assert!(equal.contains("differ from entry"), "{equal}");
        let almost = size_position(100.0, 95.0, Some(100.0 + 1e-12), 250.0, StopSource::User).unwrap_err();
        assert!(almost.contains("differ from entry"), "{almost}");
        let short_wrong = size_position(50.0, 55.0, Some(60.0), 100.0, StopSource::User).unwrap_err();
        assert!(short_wrong.contains("below entry"), "{short_wrong}");
        // A correct-side short target still sizes, and its R is the reward, not the absolute distance to a loser.
        let short = size_position(50.0, 55.0, Some(40.0), 100.0, StopSource::User).unwrap();
        assert!((short.r_multiple.unwrap() - 2.0).abs() < 1e-12);
    }

    #[test]
    fn nonfinite_inputs_and_derived_quantities_are_rejected() {
        assert!(size_position(f64::NAN, 95.0, None, 100.0, StopSource::User).unwrap_err().contains("entry"));
        assert!(size_position(f64::INFINITY, 95.0, None, 100.0, StopSource::User).unwrap_err().contains("entry"));
        assert!(size_position(100.0, f64::NAN, None, 100.0, StopSource::User).unwrap_err().contains("stop"));
        assert!(size_position(100.0, f64::INFINITY, None, 100.0, StopSource::User).unwrap_err().contains("stop"));
        assert!(size_position(100.0, 95.0, None, f64::NAN, StopSource::User).unwrap_err().contains("dollar risk"));
        assert!(size_position(100.0, 95.0, None, f64::INFINITY, StopSource::User).unwrap_err().contains("dollar risk"));
        assert!(size_position(100.0, 95.0, Some(f64::NAN), 100.0, StopSource::User).unwrap_err().contains("finite"));
        assert!(
            size_position(100.0, 95.0, Some(f64::NEG_INFINITY), 100.0, StopSource::User)
                .unwrap_err()
                .contains("finite")
        );
        assert!(size_position(100.0, 95.0, Some(0.0), 100.0, StopSource::User).unwrap_err().contains("positive"));
        assert!(size_position(100.0, 95.0, Some(-5.0), 100.0, StopSource::User).unwrap_err().contains("positive"));
        // dollar risk / a tiny stop distance overflows; that quantity is not a plan.
        let overflow = size_position(1.0, 1.0 - 1e-8, None, f64::MAX, StopSource::User).unwrap_err();
        assert!(overflow.contains("invalid"), "{overflow}");
    }

    #[test]
    fn handoff_keeps_exact_long_quantity_and_blocks_zero_and_shorts() {
        let plan = size_position(100.0, 95.0, Some(110.0), 250.0, StopSource::Box).unwrap();
        let ctx = plan.handoff().unwrap();
        assert_eq!(ctx.quantity, 50);
        assert!((ctx.entry - 100.0).abs() < 1e-12);
        assert!((ctx.stop - 95.0).abs() < 1e-12);
        assert_eq!(ctx.target, Some(110.0));
        assert!((ctx.dollar_risk - 250.0).abs() < 1e-12);

        let two = size_position(100.0, 95.0, None, 10.0, StopSource::User).unwrap();
        assert!(two.target.is_none());
        assert!(two.r_multiple.is_none());
        assert_eq!(two.handoff().unwrap().quantity, 2);

        // 1.9 shares floors to 1. A budget under one share does not get promoted to 1.
        let floored = size_position(100.0, 90.0, None, 19.0, StopSource::User).unwrap();
        assert!((floored.shares - 1.9).abs() < 1e-12);
        assert_eq!(floored.handoff().unwrap().quantity, 1);
        let tiny = size_position(100.0, 99.0, None, 0.5, StopSource::User).unwrap();
        assert!(tiny.shares < 1.0);
        assert_eq!(tiny.whole_shares, 0.0);
        assert_eq!(tiny.handoff(), Err(HandoffBlock::ZeroShares));
        assert!(tiny.handoff().unwrap_err().explain().contains("whole-share quantity is 0"));

        let short = size_position(50.0, 55.0, Some(40.0), 100.0, StopSource::User).unwrap();
        assert_eq!(short.handoff(), Err(HandoffBlock::Short));
        let why = short.handoff().unwrap_err().explain();
        assert!(why.contains("does not open a short"));
        assert!(why.contains("not supported"));

        let mut broken = plan.clone();
        broken.whole_shares = f64::NAN;
        assert_eq!(broken.handoff(), Err(HandoffBlock::InvalidQuantity));
        broken.whole_shares = 1.0;
        assert_eq!(broken.handoff(), Err(HandoffBlock::InvalidQuantity));
        broken.whole_shares = plan.whole_shares;
        broken.entry = f64::NAN;
        assert_eq!(broken.handoff(), Err(HandoffBlock::InvalidPlan));
    }

    #[test]
    fn atr_uses_wilder_smoothing_and_needs_n_plus_one_bars() {
        let bars: Vec<Bar> = (0..16).map(|i| bar(i, 10.0, 11.0, 9.0, Provenance::Real)).collect();
        // Each TR is 2.0 (high-low; prev close is inside the range).
        assert!((last_atr(&bars, 14).unwrap() - 2.0).abs() < 1e-12);
        assert!(last_atr(&bars[..14], 14).is_none());
        let empty = drawing_stops(&[0.0, -1.0, f64::NAN]);
        assert!(empty.is_empty());
        assert_eq!(drawing_stops(&[10.0, 10.0, 12.0]), vec![10.0, 12.0]);
    }

    #[test]
    fn archive_ticker_uses_the_yahoo_symbol_not_the_market_key() {
        assert_eq!(archive_ticker("AAPL US Equity"), "AAPL");
        assert_eq!(archive_ticker("MSFT"), "MSFT");
    }

    #[test]
    fn group_summary_stays_inside_the_watchlist() {
        let mut rows = vec![
            MemberRow::from_bars("A", "A", &series(30, 130.0)).unwrap(),
            MemberRow::from_bars("B", "B", &series(30, 80.0)).unwrap(),
            MemberRow::skipped("C", "C", Provenance::Missing, "no daily history"),
        ];
        rank_watchlist(&mut rows);
        let g = GroupSummary::from_rows("Core", &rows);
        assert_eq!((g.members, g.ranked, g.skipped, g.advancing, g.declining), (3, 2, 1, 1, 1));
        assert!(!g.mixed_dates);
    }
}
