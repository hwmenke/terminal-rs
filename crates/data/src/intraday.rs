//! US cash-session math for intraday bars.
//!
//! Equity, ETF and cash-index series are interpreted in `America/New_York`.
//! 1m/5m/15m/30m buckets follow the New York clock (the same instants as a UTC
//! clock, because the offset is a whole hour and those steps divide 60).
//! 1h buckets start at 09:30, not on the UTC hour. Missing opens inside a session
//! are reported and never filled. Overnight, weekends and holidays are session
//! boundaries, not gaps. Stats are computed only from bars that are actually present.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use chrono::{Datelike, NaiveDate, Timelike, Weekday};
use tr_core::{AssetClass, Bar, Provenance, Timeframe};

use crate::history::calendar::{is_session, previous_session};

/// Regular-session open, 09:30 New York.
pub const OPEN_MIN: u32 = 9 * 60 + 30;
/// Pre-market open, 04:00.
pub const PRE_MIN: u32 = 4 * 60;
/// Post-market ends at 20:00 (exclusive).
pub const POST_END_MIN: u32 = 20 * 60;

/// How long a successful intraday fetch may be reused before another provider call.
/// Daily and higher always refresh: research and the cold-start path depend on that.
pub fn refresh_ttl_secs(tf: Timeframe) -> Option<i64> {
    Some(match tf {
        Timeframe::M1 => 45,
        Timeframe::M5 => 60,
        Timeframe::M15 => 120,
        Timeframe::M30 => 180,
        Timeframe::H1 => 300,
        Timeframe::H4 => 600,
        _ => return None,
    })
}

/// `fetched_at` is still inside `ttl` as of `now`. A missing TTL never counts as fresh.
pub fn fetched_recently(fetched_at: Option<i64>, now: i64, ttl: Option<i64>) -> bool {
    matches!((fetched_at, ttl), (Some(at), Some(ttl)) if now >= at && now.saturating_sub(at) < ttl)
}

/// Minimum spacing between Yahoo chart/quote calls so a burst of intraday loads
/// does not open a connection per pane at the same instant.
#[derive(Debug, Clone)]
pub struct Pace {
    gap: Duration,
    /// When the next request is allowed to start.
    next: Option<Instant>,
}

impl Pace {
    pub fn yahoo() -> Self {
        Self { gap: Duration::from_millis(200), next: None }
    }

    /// Wait before starting a request, then reserve the following slot one gap later.
    pub fn delay(&mut self, now: Instant) -> Duration {
        let start = match self.next {
            Some(ready) if ready > now => ready,
            _ => now,
        };
        self.next = Some(start + self.gap);
        start.saturating_duration_since(now)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Closed,
    Pre,
    Regular,
    Post,
}

/// Half day: the Friday after Thanksgiving, Christmas Eve, and July 3.
/// Matches the research calendar. A date that is not a session stays closed.
pub fn early_close(date: NaiveDate) -> bool {
    (date.month() == 11 && date.weekday() == Weekday::Fri && (23..=29).contains(&date.day()))
        || (date.month() == 12 && date.day() == 24)
        || (date.month() == 7 && date.day() == 3)
}

pub fn close_minute(date: NaiveDate) -> u32 {
    if early_close(date) { 13 * 60 } else { 16 * 60 }
}

fn minute_of(ts: i64) -> (NaiveDate, u32) {
    let wall = tr_core::ny::civil(ts);
    (wall.date(), wall.hour() * 60 + wall.minute())
}

pub fn phase(ts: i64) -> Phase {
    let (date, m) = minute_of(ts);
    if !is_session(date) {
        return Phase::Closed;
    }
    let close = close_minute(date);
    if (PRE_MIN..OPEN_MIN).contains(&m) {
        Phase::Pre
    } else if (OPEN_MIN..close).contains(&m) {
        Phase::Regular
    } else if (close..POST_END_MIN).contains(&m) {
        Phase::Post
    } else {
        Phase::Closed
    }
}

pub fn in_regular_session(ts: i64) -> bool {
    phase(ts) == Phase::Regular
}

/// Drop bars outside the regular cash session. Bar OHLC is unchanged.
pub fn regular_only(bars: Vec<Bar>) -> Vec<Bar> {
    bars.into_iter().filter(|b| in_regular_session(b.ts)).collect()
}

/// Pre-market, regular session, or after hours. Overnight and closed days are not.
pub fn in_extended_session(ts: i64) -> bool {
    matches!(phase(ts), Phase::Pre | Phase::Regular | Phase::Post)
}

/// Keep bars the cash clock calls pre-market, regular, or after hours.
/// OHLC is unchanged. Nothing is inserted for a missing print.
pub fn keep_extended(bars: Vec<Bar>) -> Vec<Bar> {
    bars.into_iter().filter(|b| in_extended_session(b.ts)).collect()
}

/// First cash half-hour. A shorter or longer bar cannot stand in for it.
pub const OPENING_MINUTES: u32 = 30;

/// High and low of the latest regular session's first [`OPENING_MINUTES`], when every bucket is present.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpeningRange {
    pub high: f64,
    pub low: f64,
}

/// Opening range of the latest regular session in `bars`.
/// `None` when the timeframe cannot tile 30 minutes, any bucket in that window is missing,
/// a bar is unverified, or a pre-market print is the only high. Nothing is filled in.
pub fn opening_range(tf: Timeframe, bars: &[Bar]) -> Option<OpeningRange> {
    let step = step_minutes(tf)?;
    if step == 0 || OPENING_MINUTES % step != 0 {
        return None;
    }
    let need = (OPENING_MINUTES / step) as usize;
    let mut by_day: BTreeMap<NaiveDate, BTreeMap<i64, (f64, f64, bool)>> = BTreeMap::new();
    for b in bars {
        let Some(bucket) = regular_bucket(tf, b.ts) else { continue };
        let entry = by_day.entry(minute_of(b.ts).0).or_default().entry(bucket).or_insert((
            f64::NEG_INFINITY,
            f64::INFINITY,
            true,
        ));
        if b.high.is_finite() {
            entry.0 = entry.0.max(b.high);
        }
        if b.low.is_finite() {
            entry.1 = entry.1.min(b.low);
        }
        entry.2 &= b.provenance.is_verified();
    }
    let (date, session) = by_day.pop_last()?;
    let window: Vec<i64> = regular_opens(date, tf).into_iter().take(need).collect();
    if window.len() != need {
        return None;
    }
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    for open in window {
        let (h, l, verified) = session.get(&open)?;
        if !verified || !h.is_finite() || !l.is_finite() || *h < *l {
            return None;
        }
        high = high.max(*h);
        low = low.min(*l);
    }
    (high.is_finite() && low.is_finite() && high >= low).then_some(OpeningRange { high, low })
}

/// High, low, and close of the regular session before the latest one in `bars`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriorSession {
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

/// Prior cash session, when that calendar session is fully present and verified.
/// The latest session can be a single bar. A missing bucket stays blank, and an older
/// complete day does not stand in. Premarket and after-hours prints are ignored.
pub fn prior_session(tf: Timeframe, bars: &[Bar]) -> Option<PriorSession> {
    if step_minutes(tf).is_none() {
        return None;
    }
    #[derive(Clone, Copy)]
    struct Acc {
        high: f64,
        low: f64,
        close: f64,
        verified: bool,
    }
    let mut by_day: BTreeMap<NaiveDate, BTreeMap<i64, Acc>> = BTreeMap::new();
    for b in bars {
        let Some(bucket) = regular_bucket(tf, b.ts) else { continue };
        let entry = by_day.entry(minute_of(b.ts).0).or_default().entry(bucket).or_insert(Acc {
            high: f64::NEG_INFINITY,
            low: f64::INFINITY,
            close: f64::NAN,
            verified: true,
        });
        if b.high.is_finite() {
            entry.high = entry.high.max(b.high);
        }
        if b.low.is_finite() {
            entry.low = entry.low.min(b.low);
        }
        if b.close.is_finite() {
            entry.close = b.close;
        }
        entry.verified &= b.provenance.is_verified();
    }
    let (latest, _) = by_day.pop_last()?;
    let session = by_day.remove(&previous_session(latest))?;
    let window = regular_opens(previous_session(latest), tf);
    if window.is_empty() {
        return None;
    }
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    let mut close = f64::NAN;
    for open in window {
        let bar = session.get(&open)?;
        if !bar.verified
            || !bar.high.is_finite()
            || !bar.low.is_finite()
            || !bar.close.is_finite()
            || bar.high < bar.low
        {
            return None;
        }
        high = high.max(bar.high);
        low = low.min(bar.low);
        close = bar.close;
    }
    (high.is_finite() && low.is_finite() && close.is_finite() && high >= low).then_some(PriorSession {
        high,
        low,
        close,
    })
}

/// How many kept bars sit outside the regular session. Zero is reported as no bars, not as a price.
pub fn extended_summary(bars: &[Bar]) -> String {
    let pre = bars.iter().filter(|b| phase(b.ts) == Phase::Pre).count();
    let post = bars.iter().filter(|b| phase(b.ts) == Phase::Post).count();
    if pre == 0 && post == 0 {
        "Extended hours: no pre-market or after-hours bars in this read.".into()
    } else {
        format!("Extended hours: {pre} pre-market · {post} after hours")
    }
}

fn step_minutes(tf: Timeframe) -> Option<u32> {
    match tf {
        Timeframe::M1 => Some(1),
        Timeframe::M5 => Some(5),
        Timeframe::M15 => Some(15),
        Timeframe::M30 => Some(30),
        Timeframe::H1 => Some(60),
        _ => None,
    }
}

/// Open time of the regular-session bucket containing `ts`.
/// `None` outside the regular session, or for a timeframe that has no cash grid.
pub fn regular_bucket(tf: Timeframe, ts: i64) -> Option<i64> {
    let step = step_minutes(tf)?;
    if !in_regular_session(ts) {
        return None;
    }
    let (date, m) = minute_of(ts);
    let start = OPEN_MIN + (m - OPEN_MIN) / step * step;
    Some(tr_core::ny::to_utc(date.and_hms_opt(start / 60, start % 60, 0).unwrap()))
}

/// Expected regular-session opens for one New York date. Empty on weekends and holidays.
pub fn regular_opens(date: NaiveDate, tf: Timeframe) -> Vec<i64> {
    let Some(step) = step_minutes(tf) else { return Vec::new() };
    if !is_session(date) {
        return Vec::new();
    }
    let close = close_minute(date);
    let mut m = OPEN_MIN;
    let mut out = Vec::new();
    while m < close {
        out.push(tr_core::ny::to_utc(date.and_hms_opt(m / 60, m % 60, 0).unwrap()));
        m += step;
    }
    out
}

/// Missing regular-session opens between the first and last bar of each session day.
/// Nothing is inserted. The gap from the close to the next open is not a hole.
pub fn session_holes(tf: Timeframe, bars: &[Bar]) -> Vec<i64> {
    if step_minutes(tf).is_none() {
        return Vec::new();
    }
    let mut by_day: BTreeMap<NaiveDate, Vec<i64>> = BTreeMap::new();
    for b in bars {
        let Some(bucket) = regular_bucket(tf, b.ts) else { continue };
        by_day.entry(minute_of(b.ts).0).or_default().push(bucket);
    }
    let mut holes = Vec::new();
    for (date, mut present) in by_day {
        present.sort_unstable();
        present.dedup();
        let (Some(&first), Some(&last)) = (present.first(), present.last()) else { continue };
        for open in regular_opens(date, tf) {
            if open >= first && open <= last && !present.contains(&open) {
                holes.push(open);
            }
        }
    }
    holes
}

fn minutes_from_open(ts: i64) -> Option<u32> {
    if !in_regular_session(ts) {
        return None;
    }
    Some(minute_of(ts).1 - OPEN_MIN)
}

fn latest_regular(tf: Timeframe, bars: &[Bar]) -> Vec<&Bar> {
    let mut days: BTreeMap<NaiveDate, Vec<&Bar>> = BTreeMap::new();
    for b in bars {
        if regular_bucket(tf, b.ts).is_some() {
            days.entry(minute_of(b.ts).0).or_default().push(b);
        }
    }
    days.pop_last().map(|(_, v)| v).unwrap_or_default()
}

/// Session VWAP of the latest regular session, `hlc3 * volume`.
/// `None` when that session has no positive finite volume or no finite typical price.
pub fn session_vwap(tf: Timeframe, bars: &[Bar]) -> Option<f64> {
    let mut pv = 0.0;
    let mut vol = 0.0;
    for b in latest_regular(tf, bars) {
        let typical = b.hlc3();
        if typical.is_finite() && b.volume.is_finite() && b.volume > 0.0 {
            pv += typical * b.volume;
            vol += b.volume;
        }
    }
    (vol > 0.0 && pv.is_finite()).then_some(pv / vol)
}

/// Last regular-session print and its session VWAP. `None` when that session has no VWAP.
/// The close is the last regular bar, so a pre-market print cannot stand in for it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SessionPrint {
    pub close: f64,
    pub vwap: f64,
    pub bar_ts: i64,
}

/// How many 5-minute bars a VWAP alert reads. One cash session is 78 bars; this covers that session.
pub const VWAP_ALERT_BARS: usize = 120;

pub fn session_print(tf: Timeframe, bars: &[Bar]) -> Option<SessionPrint> {
    let session = latest_regular(tf, bars);
    let last = *session.last()?;
    let vwap = session_vwap(tf, bars)?;
    (last.close.is_finite() && last.close > 0.0).then_some(SessionPrint { close: last.close, vwap, bar_ts: last.ts })
}

/// Today's regular-session volume through the last bar's offset from the open,
/// divided by the mean of earlier sessions at the same offset.
/// `None` until a prior session has volume at that offset.
pub fn relative_volume(tf: Timeframe, bars: &[Bar]) -> Option<f64> {
    step_minutes(tf)?;
    let mut days: BTreeMap<NaiveDate, Vec<&Bar>> = BTreeMap::new();
    for b in bars {
        if in_regular_session(b.ts) && b.volume.is_finite() && b.volume >= 0.0 {
            days.entry(minute_of(b.ts).0).or_default().push(b);
        }
    }
    let (today, today_bars) = days.pop_last()?;
    let offset = minutes_from_open(today_bars.last()?.ts)?;
    let cum = |rows: &[&Bar]| -> f64 {
        rows.iter().filter(|b| minutes_from_open(b.ts).is_some_and(|m| m <= offset)).map(|b| b.volume).sum()
    };
    let today_vol = cum(&today_bars);
    if !(today_vol.is_finite() && today_vol > 0.0) {
        return None;
    }
    let mut priors = Vec::new();
    for (date, rows) in &days {
        if *date == today {
            continue;
        }
        let vol = cum(rows);
        if vol.is_finite() && vol > 0.0 {
            priors.push(vol);
        }
    }
    if priors.is_empty() {
        return None;
    }
    let mean = priors.iter().sum::<f64>() / priors.len() as f64;
    (mean.is_finite() && mean > 0.0).then_some(today_vol / mean)
}

/// One price bin of a volume profile built from real bar volume.
#[derive(Debug, Clone, PartialEq)]
pub struct VolumeBin {
    /// Center of the bin. The edges stay inside the session's high/low.
    pub price: f64,
    pub volume: f64,
}

/// Volume of the latest regular session, each bar assigned to the bin of its typical price.
/// Empty when there is no positive volume or no finite high/low. Bins with no volume are omitted.
pub fn volume_profile(tf: Timeframe, bars: &[Bar], bins: usize) -> Vec<VolumeBin> {
    let session: Vec<&Bar> = latest_regular(tf, bars)
        .into_iter()
        .filter(|b| b.high.is_finite() && b.low.is_finite() && b.volume.is_finite() && b.volume > 0.0)
        .collect();
    if session.is_empty() {
        return Vec::new();
    }
    let lo = session.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
    let hi = session.iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max);
    if !lo.is_finite() || !hi.is_finite() || hi < lo {
        return Vec::new();
    }
    let total: f64 = session.iter().map(|b| b.volume).sum();
    if (hi - lo).abs() < 1e-9 {
        return vec![VolumeBin { price: lo, volume: total }];
    }
    let bins = bins.clamp(1, 48);
    let mut acc = vec![0.0; bins];
    let width = (hi - lo) / bins as f64;
    for b in &session {
        let px = b.hlc3();
        if !px.is_finite() {
            continue;
        }
        let mut i = ((px - lo) / width).floor() as usize;
        if i >= bins {
            i = bins - 1;
        }
        acc[i] += b.volume;
    }
    acc.into_iter()
        .enumerate()
        .filter(|(_, v)| *v > 0.0)
        .map(|(i, volume)| VolumeBin { price: lo + (i as f64 + 0.5) * width, volume })
        .collect()
}

/// Derived intraday stats. Every field stays empty when the inputs do not support it.
#[derive(Debug, Clone, PartialEq)]
pub struct BarStats {
    pub vwap: Option<f64>,
    pub relative_volume: Option<f64>,
    pub profile: Vec<VolumeBin>,
    pub gaps: Vec<i64>,
}

impl BarStats {
    /// Price bin that received the most real volume.
    pub fn poc(&self) -> Option<f64> {
        self.profile.iter().max_by(|a, b| a.volume.total_cmp(&b.volume)).map(|b| b.price)
    }
}

pub fn bar_stats(tf: Timeframe, bars: &[Bar]) -> BarStats {
    BarStats {
        vwap: session_vwap(tf, bars),
        relative_volume: relative_volume(tf, bars),
        profile: volume_profile(tf, bars, 24),
        gaps: session_holes(tf, bars),
    }
}

/// How the last bar's age should be read. Cash equities stop at the close;
/// FX, crypto, futures and rates do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketClock {
    UsCash,
    Continuous,
}

impl MarketClock {
    pub fn for_class(class: AssetClass) -> Self {
        match class {
            AssetClass::Equity | AssetClass::Etf | AssetClass::Index => Self::UsCash,
            _ => Self::Continuous,
        }
    }
}

/// Age of the latest bar relative to `now`. Nothing here invents a missing bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// The latest bar is the one forming, or the previous one still allowed to print.
    Current,
    /// The cash session is over and the last bar covers that session.
    Closed,
    /// The last bar is older than the one the clock says should be on screen.
    Stale { lag_secs: i64 },
}

/// `None` for daily and higher. Those charts are not aged by this clock.
pub fn freshness(clock: MarketClock, tf: Timeframe, last_bar_ts: i64, now: i64) -> Option<Freshness> {
    if !tf.is_intraday() {
        return None;
    }
    Some(match clock {
        MarketClock::Continuous => continuous_freshness(tf, last_bar_ts, now),
        MarketClock::UsCash => cash_freshness(tf, last_bar_ts, now),
    })
}

/// Why a market order must not use this print. Limit and stop prices are the user's.
/// Sample and missing quotes are not judged here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketBlock {
    Stale { lag_secs: i64 },
    Closed,
}

impl MarketBlock {
    pub fn badge(self) -> &'static str {
        match self {
            Self::Stale { .. } => "STALE",
            Self::Closed => "SESSION CLOSED",
        }
    }

    pub fn explain(self) -> &'static str {
        match self {
            Self::Stale { .. } => {
                "Market order blocked: the last print is stale. A market order needs a current price."
            }
            Self::Closed => {
                "Market order blocked: the session is closed. The last print is the close, not a live market."
            }
        }
    }
}

/// `None` when a market order may use the print: it is current, or it is not a verified quote.
/// The clock is the 1-minute intraday clock, the same one the chart uses for a last trade.
pub fn market_order_block(class: AssetClass, provenance: Provenance, quote_ts: i64, now: i64) -> Option<MarketBlock> {
    if !provenance.is_verified() {
        return None;
    }
    if quote_ts <= 0 {
        return Some(MarketBlock::Stale { lag_secs: 0 });
    }
    match freshness(MarketClock::for_class(class), Timeframe::M1, quote_ts, now)? {
        Freshness::Current => None,
        Freshness::Closed => Some(MarketBlock::Closed),
        Freshness::Stale { lag_secs } => Some(MarketBlock::Stale { lag_secs }),
    }
}

fn continuous_freshness(tf: Timeframe, last_bar_ts: i64, now: i64) -> Freshness {
    let limit = tf.seconds().saturating_mul(2);
    let age = now.saturating_sub(last_bar_ts);
    if age <= limit { Freshness::Current } else { Freshness::Stale { lag_secs: age - limit } }
}

fn cash_freshness(tf: Timeframe, last_bar_ts: i64, now: i64) -> Freshness {
    if in_regular_session(now) {
        return if step_minutes(tf).is_some() {
            session_bar_freshness(tf, last_bar_ts, now)
        } else {
            continuous_freshness(tf, last_bar_ts, now)
        };
    }
    let (date, minute) = minute_of(now);
    let due = if is_session(date) && minute >= OPEN_MIN { date } else { previous_session(date) };
    if step_minutes(tf).is_none() {
        return if minute_of(last_bar_ts).0 == due {
            Freshness::Closed
        } else {
            Freshness::Stale { lag_secs: now.saturating_sub(last_bar_ts) }
        };
    }
    let Some(&final_open) = regular_opens(due, tf).last() else {
        return Freshness::Stale { lag_secs: now.saturating_sub(last_bar_ts) };
    };
    let step_secs = step_minutes(tf).unwrap_or(1) as i64 * 60;
    let close_min = close_minute(due);
    let close_ts = tr_core::ny::to_utc(due.and_hms_opt(close_min / 60, close_min % 60, 0).unwrap());
    let ttl = refresh_ttl_secs(tf).unwrap_or(0);
    let within_grace = now >= close_ts && now < close_ts.saturating_add(ttl);
    let required = if within_grace { final_open - step_secs } else { final_open };
    let last = regular_bucket(tf, last_bar_ts).unwrap_or(last_bar_ts);
    if last >= required { Freshness::Closed } else { Freshness::Stale { lag_secs: required.saturating_sub(last) } }
}

/// While the session is open, the bar still forming and the one before it count as current.
fn session_bar_freshness(tf: Timeframe, last_bar_ts: i64, now: i64) -> Freshness {
    let Some(forming) = regular_bucket(tf, now) else {
        return continuous_freshness(tf, last_bar_ts, now);
    };
    let step_secs = step_minutes(tf).unwrap_or(1) as i64 * 60;
    let acceptable = forming - step_secs;
    let last = regular_bucket(tf, last_bar_ts).unwrap_or(last_bar_ts);
    if last >= acceptable { Freshness::Current } else { Freshness::Stale { lag_secs: acceptable.saturating_sub(last) } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn wall(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        tr_core::ny::to_utc(NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
    }

    fn bar(ts: i64, volume: f64) -> Bar {
        Bar::new(ts, 10.0, 12.0, 8.0, 11.0, volume)
    }

    #[test]
    fn phases_follow_new_york_including_dst_and_the_early_close() {
        assert_eq!(phase(wall(2026, 3, 9, 8, 0)), Phase::Pre);
        assert_eq!(phase(wall(2026, 3, 9, 9, 30)), Phase::Regular);
        assert_eq!(phase(wall(2026, 3, 9, 15, 59)), Phase::Regular);
        assert_eq!(phase(wall(2026, 3, 9, 16, 0)), Phase::Post);
        assert_eq!(phase(wall(2026, 3, 9, 20, 0)), Phase::Closed);
        // 13:30 UTC that Monday is the open (EDT); the same UTC clock in November is pre-market.
        let march_open =
            NaiveDate::from_ymd_opt(2026, 3, 9).unwrap().and_hms_opt(13, 30, 0).unwrap().and_utc().timestamp();
        let november_same_utc =
            NaiveDate::from_ymd_opt(2026, 11, 2).unwrap().and_hms_opt(13, 30, 0).unwrap().and_utc().timestamp();
        assert_eq!(phase(march_open), Phase::Regular);
        assert_eq!(phase(november_same_utc), Phase::Pre);
        assert_eq!(phase(wall(2026, 11, 2, 9, 30)), Phase::Regular);
        // Thanksgiving Friday 2026 closes at 13:00.
        assert_eq!(phase(wall(2026, 11, 27, 12, 59)), Phase::Regular);
        assert_eq!(phase(wall(2026, 11, 27, 13, 0)), Phase::Post);
        assert_eq!(phase(wall(2026, 3, 7, 10, 0)), Phase::Closed); // Saturday
        assert_eq!(phase(wall(2026, 4, 3, 10, 0)), Phase::Closed); // Good Friday
    }

    #[test]
    fn minute_buckets_match_the_utc_clock_and_hours_start_at_the_open() {
        let ts = wall(2026, 3, 9, 10, 7);
        for tf in [Timeframe::M1, Timeframe::M5, Timeframe::M15, Timeframe::M30] {
            let bucket = regular_bucket(tf, ts).unwrap();
            assert_eq!(bucket, ts.div_euclid(tf.seconds()) * tf.seconds(), "{tf}");
        }
        assert_eq!(regular_bucket(Timeframe::M5, ts), Some(wall(2026, 3, 9, 10, 5)));
        assert_eq!(regular_bucket(Timeframe::M15, ts), Some(wall(2026, 3, 9, 10, 0)));
        assert_eq!(regular_bucket(Timeframe::M1, wall(2026, 3, 9, 10, 7)), Some(wall(2026, 3, 9, 10, 7)));
        // 10:07 belongs to the 09:30 hour, not the 10:00 UTC hour.
        assert_eq!(regular_bucket(Timeframe::H1, ts), Some(wall(2026, 3, 9, 9, 30)));
        assert_ne!(regular_bucket(Timeframe::H1, ts), Some(ts.div_euclid(3600) * 3600));
        assert_eq!(regular_bucket(Timeframe::H1, wall(2026, 3, 9, 15, 45)), Some(wall(2026, 3, 9, 15, 30)));
        assert_eq!(regular_bucket(Timeframe::H1, wall(2026, 11, 27, 12, 45)), Some(wall(2026, 11, 27, 12, 30)));
        assert_eq!(regular_bucket(Timeframe::H1, wall(2026, 3, 9, 9, 29)), None);
        assert_eq!(regular_bucket(Timeframe::M5, wall(2026, 11, 2, 9, 32)), Some(wall(2026, 11, 2, 9, 30)));
    }

    #[test]
    fn opens_cover_the_session_and_stop_at_the_early_close() {
        let day = NaiveDate::from_ymd_opt(2026, 3, 9).unwrap();
        assert_eq!(regular_opens(day, Timeframe::M1).len(), 390);
        assert_eq!(regular_opens(day, Timeframe::M5).len(), 78);
        assert_eq!(regular_opens(day, Timeframe::M15).len(), 26);
        assert_eq!(regular_opens(day, Timeframe::H1).len(), 7);
        assert_eq!(regular_opens(day, Timeframe::H1)[0], wall(2026, 3, 9, 9, 30));
        assert_eq!(*regular_opens(day, Timeframe::H1).last().unwrap(), wall(2026, 3, 9, 15, 30));
        let early = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
        assert_eq!(regular_opens(early, Timeframe::H1).len(), 4);
        assert_eq!(*regular_opens(early, Timeframe::H1).last().unwrap(), wall(2026, 11, 27, 12, 30));
        assert!(regular_opens(NaiveDate::from_ymd_opt(2026, 4, 3).unwrap(), Timeframe::M5).is_empty());
        assert!(regular_opens(day, Timeframe::D1).is_empty());
    }

    #[test]
    fn holes_stay_inside_the_session_and_do_not_invent_bars() {
        let monday = [
            bar(wall(2026, 3, 9, 9, 30), 1.0),
            bar(wall(2026, 3, 9, 9, 40), 1.0),
            bar(wall(2026, 3, 8, 18, 0), 9.0), // Sunday, ignored
            bar(wall(2026, 3, 9, 8, 0), 9.0),  // pre-market, ignored
        ];
        let n = monday.len();
        let holes = session_holes(Timeframe::M5, &monday);
        assert_eq!(holes, vec![wall(2026, 3, 9, 9, 35)]);
        assert_eq!(monday.len(), n, "gap detection must not insert bars");
        assert!(regular_only(monday.to_vec()).iter().all(|b| b.volume == 1.0));

        // Friday close to Monday open is a boundary, not a hole.
        let weekend = [bar(wall(2026, 3, 6, 15, 55), 1.0), bar(wall(2026, 3, 9, 9, 30), 1.0)];
        assert!(session_holes(Timeframe::M5, &weekend).is_empty());

        // Good Friday has no expected opens between Thursday and Monday.
        let holiday = [bar(wall(2026, 4, 2, 15, 55), 1.0), bar(wall(2026, 4, 6, 9, 30), 1.0)];
        assert!(session_holes(Timeframe::M5, &holiday).is_empty());

        let early = [bar(wall(2026, 11, 27, 9, 30), 1.0), bar(wall(2026, 11, 27, 12, 30), 1.0)];
        let missing = session_holes(Timeframe::H1, &early);
        assert_eq!(missing, vec![wall(2026, 11, 27, 10, 30), wall(2026, 11, 27, 11, 30)]);
    }

    #[test]
    fn extended_hours_keep_pre_and_post_and_drop_the_overnight() {
        let pre = Bar::new(wall(2026, 3, 9, 8, 0), 1.5, 2.5, 0.5, 2.0, 10.0).with_provenance(Provenance::Real);
        let regular = Bar::new(wall(2026, 3, 9, 9, 30), 3.0, 4.0, 2.0, 3.5, 20.0).with_provenance(Provenance::Real);
        let post = Bar::new(wall(2026, 3, 9, 16, 0), 5.0, 6.0, 4.0, 5.5, 7.0).with_provenance(Provenance::Real);
        let early_post = Bar::new(wall(2026, 11, 27, 13, 0), 8.0, 8.0, 8.0, 8.0, 1.0).with_provenance(Provenance::Real);
        let overnight = bar(wall(2026, 3, 9, 3, 0), 9.0);
        let closed = bar(wall(2026, 3, 9, 20, 0), 9.0);
        let sunday = bar(wall(2026, 3, 8, 18, 0), 9.0);
        let kept = keep_extended(vec![pre, overnight, regular, post, closed, sunday, early_post]);
        assert_eq!(kept, vec![pre, regular, post, early_post]);
        assert_eq!(kept[0].close, 2.0);
        assert_eq!(extended_summary(&kept), "Extended hours: 1 pre-market · 2 after hours");
        assert_eq!(extended_summary(&[regular]), "Extended hours: no pre-market or after-hours bars in this read.");
        assert!(regular_only(vec![pre, regular, post]).iter().all(|b| b.ts == regular.ts));
    }

    fn m5(day_h: u32, day_m: u32, high: f64, low: f64) -> Bar {
        Bar::new(wall(2026, 3, 9, day_h, day_m), low + 0.5, high, low, low + 0.4, 10.0)
            .with_provenance(Provenance::Real)
    }

    #[test]
    fn opening_range_uses_only_a_complete_first_half_hour() {
        let mut bars = Vec::new();
        for (minute, high, low) in
            [(30, 11.0, 10.0), (35, 12.0, 9.5), (40, 13.0, 8.0), (45, 12.5, 9.0), (50, 11.5, 9.2), (55, 11.2, 9.1)]
        {
            bars.push(m5(9, minute, high, low));
        }
        bars.push(Bar::new(wall(2026, 3, 9, 8, 0), 1.0, 40.0, 0.5, 2.0, 50.0).with_provenance(Provenance::Real));
        bars.push(m5(10, 0, 20.0, 7.0));
        let range = opening_range(Timeframe::M5, &bars).unwrap();
        assert!((range.high - 13.0).abs() < 1e-9);
        assert!((range.low - 8.0).abs() < 1e-9);

        bars.retain(|b| b.ts != wall(2026, 3, 9, 9, 40));
        assert!(opening_range(Timeframe::M5, &bars).is_none(), "a missing bucket is not filled");

        let friday: Vec<Bar> = (0..6)
            .map(|i| {
                Bar::new(wall(2026, 3, 6, 9, 30 + i * 5), 2.0, 4.0, 1.0, 3.0, 1.0).with_provenance(Provenance::Real)
            })
            .collect();
        let mut both = friday;
        both.push(m5(9, 30, 11.0, 10.0));
        assert!(opening_range(Timeframe::M5, &both).is_none(), "a partial new session does not reuse yesterday");

        let half_hour = [Bar::new(wall(2026, 3, 9, 9, 30), 3.0, 6.0, 2.5, 5.0, 8.0).with_provenance(Provenance::Real)];
        let m30 = opening_range(Timeframe::M30, &half_hour).unwrap();
        assert!((m30.high - 6.0).abs() < 1e-9 && (m30.low - 2.5).abs() < 1e-9);
        assert!(opening_range(Timeframe::H1, &half_hour).is_none());
        assert!(opening_range(Timeframe::D1, &half_hour).is_none());

        let mut sample = half_hour;
        sample[0].provenance = Provenance::Sample;
        assert!(opening_range(Timeframe::M30, &sample).is_none());
    }

    fn session_bars(y: i32, m: u32, d: u32, tf: Timeframe) -> Vec<Bar> {
        regular_opens(NaiveDate::from_ymd_opt(y, m, d).unwrap(), tf)
            .into_iter()
            .map(|ts| Bar::new(ts, 10.4, 11.0, 10.0, 10.4, 8.0).with_provenance(Provenance::Real))
            .collect()
    }

    #[test]
    fn prior_session_uses_only_the_complete_previous_cash_day() {
        let mut friday = session_bars(2026, 3, 6, Timeframe::M30);
        assert!(friday.len() > 4, "a full Friday has a cash grid");
        friday[1].high = 12.0;
        let low_at = friday.len() - 4;
        friday[low_at].low = 9.0;
        friday.last_mut().unwrap().close = 10.5;
        friday.push(Bar::new(wall(2026, 3, 6, 8, 0), 1.0, 80.0, 0.5, 2.0, 40.0).with_provenance(Provenance::Real));
        friday.push(Bar::new(wall(2026, 3, 6, 16, 0), 1.0, 70.0, 0.4, 3.0, 40.0).with_provenance(Provenance::Real));
        let mut bars = friday.clone();
        bars.push(Bar::new(wall(2026, 3, 9, 10, 0), 40.0, 50.0, 30.0, 45.0, 8.0).with_provenance(Provenance::Real));
        let prior = prior_session(Timeframe::M30, &bars).unwrap();
        assert!((prior.high - 12.0).abs() < 1e-9);
        assert!((prior.low - 9.0).abs() < 1e-9);
        assert!((prior.close - 10.5).abs() < 1e-9);

        let mut gapped = bars.clone();
        let missing = friday[3].ts;
        gapped.retain(|b| b.ts != missing);
        assert!(prior_session(Timeframe::M30, &gapped).is_none(), "a missing prior bucket is not filled");

        let mut older = session_bars(2026, 3, 5, Timeframe::M30);
        older[0].high = 30.0;
        older.extend(gapped);
        assert!(prior_session(Timeframe::M30, &older).is_none(), "an older complete day does not replace Friday");

        assert!(prior_session(Timeframe::M30, &bars[bars.len() - 1..]).is_none(), "today alone has no prior session");
        assert!(prior_session(Timeframe::D1, &bars).is_none());

        let mut early = session_bars(2026, 11, 27, Timeframe::M30);
        assert_eq!(early.len(), 7, "early close ends at 13:00");
        for bar in &mut early {
            bar.high = 6.0;
            bar.low = 4.0;
            bar.close = 5.0;
        }
        early.push(Bar::new(wall(2026, 11, 27, 13, 0), 5.0, 20.0, 1.0, 9.0, 3.0).with_provenance(Provenance::Real));
        early.push(Bar::new(wall(2026, 11, 30, 10, 0), 5.0, 50.0, 1.0, 8.0, 3.0).with_provenance(Provenance::Real));
        let early_prior = prior_session(Timeframe::M30, &early).unwrap();
        assert!((early_prior.high - 6.0).abs() < 1e-9);
        assert!((early_prior.low - 4.0).abs() < 1e-9);
        assert!((early_prior.close - 5.0).abs() < 1e-9);

        let mut holiday = session_bars(2026, 4, 2, Timeframe::M30);
        for bar in &mut holiday {
            bar.high = 7.0;
            bar.low = 4.0;
            bar.close = 5.0;
        }
        holiday[2].high = 8.0;
        holiday[2].low = 3.0;
        holiday.last_mut().unwrap().close = 4.5;
        holiday.push(Bar::new(wall(2026, 4, 6, 10, 0), 5.0, 9.0, 2.0, 6.0, 3.0).with_provenance(Provenance::Real));
        let skipped = prior_session(Timeframe::M30, &holiday).unwrap();
        assert!((skipped.high - 8.0).abs() < 1e-9 && (skipped.low - 3.0).abs() < 1e-9);
        assert!((skipped.close - 4.5).abs() < 1e-9, "Good Friday is not a missing bucket");

        let mut sample = bars.clone();
        sample[0].provenance = Provenance::Sample;
        assert!(prior_session(Timeframe::M30, &sample).is_none());
    }

    #[test]
    fn vwap_relative_volume_and_profile_use_only_real_bars() {
        let day = |d, vol| bar(wall(2026, 3, d, 9, 30), vol);
        let one = [day(9, 0.0)];
        assert_eq!(session_vwap(Timeframe::M5, &one), None);
        assert_eq!(relative_volume(Timeframe::M5, &one), None);
        assert!(volume_profile(Timeframe::M5, &one, 8).is_empty());

        let bars = [
            Bar::new(wall(2026, 3, 6, 9, 30), 10.0, 12.0, 8.0, 11.0, 100.0),
            Bar::new(wall(2026, 3, 6, 9, 35), 10.0, 12.0, 8.0, 11.0, 100.0),
            Bar::new(wall(2026, 3, 9, 9, 30), 10.0, 12.0, 8.0, 11.0, 300.0),
            Bar::new(wall(2026, 3, 9, 9, 35), 20.0, 22.0, 18.0, 21.0, 100.0),
            Bar::new(wall(2026, 3, 9, 8, 0), 1.0, 1.0, 1.0, 1.0, 1_000.0), // pre-market excluded
        ];
        let typical = (12.0 + 8.0 + 11.0) / 3.0;
        let later = (22.0 + 18.0 + 21.0) / 3.0;
        let vwap = session_vwap(Timeframe::M5, &bars).unwrap();
        assert!((vwap - (typical * 300.0 + later * 100.0) / 400.0).abs() < 1e-9);
        let print = session_print(Timeframe::M5, &bars).unwrap();
        assert_eq!(print.bar_ts, wall(2026, 3, 9, 9, 35));
        assert!((print.close - 21.0).abs() < 1e-9);
        assert!((print.vwap - vwap).abs() < 1e-9);
        // Prior session at this offset is 200; today is 400.
        assert!((relative_volume(Timeframe::M5, &bars).unwrap() - 2.0).abs() < 1e-9);
        let only_today = &bars[2..4];
        assert_eq!(relative_volume(Timeframe::M5, only_today), None, "no prior session");
        let profile = volume_profile(Timeframe::M5, &bars, 4);
        let summed: f64 = profile.iter().map(|b| b.volume).sum();
        assert!((summed - 400.0).abs() < 1e-9);
        assert!(profile.iter().all(|b| b.price >= 8.0 && b.price <= 22.0));
        let stats = bar_stats(Timeframe::M5, &bars);
        assert!(stats.gaps.is_empty(), "adjacent 5m bars are not a gap");
        assert!(stats.poc().is_some());
        assert!(bar_stats(Timeframe::D1, &bars).vwap.is_none());
    }

    #[test]
    fn ttl_and_pace_limit_intraday_refreshes() {
        let now = 1_000_000;
        assert!(!fetched_recently(Some(now), now, refresh_ttl_secs(Timeframe::D1)));
        assert!(fetched_recently(Some(now), now + 44, refresh_ttl_secs(Timeframe::M1)));
        assert!(!fetched_recently(Some(now), now + 45, refresh_ttl_secs(Timeframe::M1)));
        assert!(!fetched_recently(None, now, refresh_ttl_secs(Timeframe::M5)));
        assert_eq!(refresh_ttl_secs(Timeframe::M5), Some(60));
        assert_eq!(refresh_ttl_secs(Timeframe::M15), Some(120));
        assert_eq!(refresh_ttl_secs(Timeframe::H1), Some(300));

        let mut pace = Pace { gap: Duration::from_millis(200), next: None };
        let t0 = Instant::now();
        assert_eq!(pace.delay(t0), Duration::ZERO);
        assert_eq!(pace.delay(t0), Duration::from_millis(200));
        assert_eq!(pace.delay(t0 + Duration::from_millis(200)), Duration::from_millis(200));
        assert_eq!(pace.delay(t0 + Duration::from_millis(600)), Duration::ZERO);
    }

    #[test]
    fn freshness_tracks_the_forming_bar_the_close_and_a_continuous_clock() {
        let cash = MarketClock::UsCash;
        let always = MarketClock::Continuous;
        let monday = wall(2026, 3, 9, 10, 7);
        assert_eq!(freshness(cash, Timeframe::D1, monday, monday), None);
        assert_eq!(freshness(always, Timeframe::W1, 0, monday), None);
        assert_eq!(MarketClock::for_class(AssetClass::Equity), cash);
        assert_eq!(MarketClock::for_class(AssetClass::Etf), cash);
        assert_eq!(MarketClock::for_class(AssetClass::Index), cash);
        assert_eq!(MarketClock::for_class(AssetClass::Crypto), always);
        assert_eq!(MarketClock::for_class(AssetClass::Fx), always);
        assert_eq!(MarketClock::for_class(AssetClass::Cmdty), always);
        assert_eq!(MarketClock::for_class(AssetClass::Govt), always);

        // 10:07 belongs to the 10:05 bar. 10:00 is the one-bar grace; 09:55 is behind.
        assert_eq!(freshness(cash, Timeframe::M5, wall(2026, 3, 9, 10, 5), monday), Some(Freshness::Current));
        assert_eq!(freshness(cash, Timeframe::M5, wall(2026, 3, 9, 10, 0), monday), Some(Freshness::Current));
        assert_eq!(
            freshness(cash, Timeframe::M5, wall(2026, 3, 9, 9, 55), monday),
            Some(Freshness::Stale { lag_secs: 300 })
        );
        // 10:31 is the 10:30 hour. The 09:30 bar may still be printing; 08:30 may not.
        let hour = wall(2026, 3, 9, 10, 31);
        assert_eq!(freshness(cash, Timeframe::H1, wall(2026, 3, 9, 10, 30), hour), Some(Freshness::Current));
        assert_eq!(freshness(cash, Timeframe::H1, wall(2026, 3, 9, 9, 30), hour), Some(Freshness::Current));
        assert_eq!(
            freshness(cash, Timeframe::H1, wall(2026, 3, 9, 8, 30), hour),
            Some(Freshness::Stale { lag_secs: 3600 })
        );
        // November open is 09:30 EST, not the March UTC instant.
        let nov = wall(2026, 11, 2, 9, 32);
        assert_eq!(freshness(cash, Timeframe::M5, wall(2026, 11, 2, 9, 30), nov), Some(Freshness::Current));
        assert!(matches!(freshness(cash, Timeframe::M5, wall(2026, 11, 2, 9, 20), nov), Some(Freshness::Stale { .. })));

        // Early-close hour: 12:45 belongs to 12:30, so 11:30 is the grace bar.
        let early_now = wall(2026, 11, 27, 12, 45);
        assert_eq!(freshness(cash, Timeframe::H1, wall(2026, 11, 27, 11, 30), early_now), Some(Freshness::Current));
        assert!(matches!(
            freshness(cash, Timeframe::H1, wall(2026, 11, 27, 10, 30), early_now),
            Some(Freshness::Stale { .. })
        ));

        // Thirty seconds after the bell, the prior 5m bar is still an honest close.
        // Ninety seconds later the TTL has passed and that bar is stale.
        let bell = wall(2026, 3, 9, 16, 0);
        assert_eq!(freshness(cash, Timeframe::M5, wall(2026, 3, 9, 15, 50), bell + 30), Some(Freshness::Closed));
        assert_eq!(freshness(cash, Timeframe::M5, wall(2026, 3, 9, 15, 55), bell + 30), Some(Freshness::Closed));
        assert_eq!(
            freshness(cash, Timeframe::M5, wall(2026, 3, 9, 15, 45), bell + 30),
            Some(Freshness::Stale { lag_secs: 300 })
        );
        assert_eq!(
            freshness(cash, Timeframe::M5, wall(2026, 3, 9, 15, 50), bell + 90),
            Some(Freshness::Stale { lag_secs: 300 })
        );
        assert_eq!(freshness(cash, Timeframe::M5, wall(2026, 3, 9, 15, 55), bell + 90), Some(Freshness::Closed));

        // Half day: grace accepts 11:30; after the 1h TTL only 12:30 counts as the close.
        let early_bell = wall(2026, 11, 27, 13, 0);
        assert_eq!(
            freshness(cash, Timeframe::H1, wall(2026, 11, 27, 11, 30), early_bell + 30),
            Some(Freshness::Closed)
        );
        assert_eq!(
            freshness(cash, Timeframe::H1, wall(2026, 11, 27, 10, 30), early_bell + 30),
            Some(Freshness::Stale { lag_secs: 3600 })
        );
        assert_eq!(
            freshness(cash, Timeframe::H1, wall(2026, 11, 27, 11, 30), early_bell + 360),
            Some(Freshness::Stale { lag_secs: 3600 })
        );
        assert_eq!(
            freshness(cash, Timeframe::H1, wall(2026, 11, 27, 12, 30), early_bell + 360),
            Some(Freshness::Closed)
        );

        // Premarket, the weekend, and Good Friday all ask for the previous session's close.
        let friday_close = wall(2026, 3, 6, 15, 55);
        let friday_prior = wall(2026, 3, 6, 15, 50);
        assert_eq!(freshness(cash, Timeframe::M5, friday_close, wall(2026, 3, 9, 8, 0)), Some(Freshness::Closed));
        assert!(matches!(
            freshness(cash, Timeframe::M5, friday_prior, wall(2026, 3, 9, 8, 0)),
            Some(Freshness::Stale { .. })
        ));
        assert_eq!(freshness(cash, Timeframe::M5, friday_close, wall(2026, 3, 7, 12, 0)), Some(Freshness::Closed));
        let thursday = wall(2026, 4, 2, 15, 55);
        assert_eq!(freshness(cash, Timeframe::M5, thursday, wall(2026, 4, 3, 11, 0)), Some(Freshness::Closed));
        assert!(matches!(
            freshness(cash, Timeframe::M5, wall(2026, 4, 1, 15, 55), wall(2026, 4, 3, 11, 0)),
            Some(Freshness::Stale { .. })
        ));

        // 4h has no cash grid: age it like a continuous bar during the session,
        // and call the session closed afterward when the last bar is that New York date.
        let late_morning = wall(2026, 3, 9, 11, 0);
        assert_eq!(freshness(cash, Timeframe::H4, wall(2026, 3, 9, 3, 0), late_morning), Some(Freshness::Current));
        assert_eq!(
            freshness(cash, Timeframe::H4, wall(2026, 3, 9, 2, 59), late_morning),
            Some(Freshness::Stale { lag_secs: 60 })
        );
        assert_eq!(
            freshness(cash, Timeframe::H4, wall(2026, 3, 6, 15, 0), wall(2026, 3, 7, 12, 0)),
            Some(Freshness::Closed)
        );
        assert!(matches!(
            freshness(cash, Timeframe::H4, wall(2026, 3, 5, 15, 0), wall(2026, 3, 7, 12, 0)),
            Some(Freshness::Stale { .. })
        ));

        let now = 1_700_000_000;
        assert_eq!(freshness(always, Timeframe::M5, now - 600, now), Some(Freshness::Current));
        assert_eq!(freshness(always, Timeframe::M5, now - 601, now), Some(Freshness::Stale { lag_secs: 1 }));
        assert!(matches!(
            freshness(always, Timeframe::M1, now - 86_400, now),
            Some(Freshness::Stale { lag_secs }) if lag_secs > 0
        ));
        assert!(!matches!(freshness(always, Timeframe::M5, now - 86_400, now), Some(Freshness::Closed)));
    }

    #[test]
    fn market_orders_require_a_current_verified_print() {
        let cash = AssetClass::Equity;
        let monday = wall(2026, 3, 9, 10, 7);
        assert_eq!(
            market_order_block(cash, Provenance::Real, wall(2026, 3, 9, 10, 7), monday),
            None,
            "the forming minute is a live market"
        );
        assert_eq!(
            market_order_block(cash, Provenance::Real, wall(2026, 3, 9, 10, 6), monday),
            None,
            "the previous minute may still be printing"
        );
        let behind = market_order_block(cash, Provenance::Cached, wall(2026, 3, 9, 10, 5), monday).unwrap();
        assert!(matches!(behind, MarketBlock::Stale { lag_secs: 60 }));
        assert_eq!(behind.badge(), "STALE");
        assert!(behind.explain().contains("stale"));

        let saturday = wall(2026, 3, 7, 12, 0);
        let closed = market_order_block(cash, Provenance::Real, wall(2026, 3, 6, 15, 59), saturday).unwrap();
        assert_eq!(closed, MarketBlock::Closed);
        assert_eq!(closed.badge(), "SESSION CLOSED");
        assert!(closed.explain().contains("session is closed"));
        assert!(matches!(
            market_order_block(cash, Provenance::Real, wall(2026, 3, 6, 15, 58), saturday),
            Some(MarketBlock::Stale { .. })
        ));

        let bell = wall(2026, 3, 9, 16, 0);
        assert_eq!(
            market_order_block(cash, Provenance::Real, wall(2026, 3, 9, 15, 58), bell + 20),
            Some(MarketBlock::Closed),
            "just after the bell the prior minute is the close, not a live market"
        );

        assert_eq!(market_order_block(cash, Provenance::Sample, wall(2026, 3, 6, 9, 30), saturday), None);
        assert_eq!(market_order_block(cash, Provenance::Missing, monday, monday), None);
        assert_eq!(market_order_block(cash, Provenance::Real, 0, monday), Some(MarketBlock::Stale { lag_secs: 0 }));

        let now = 1_700_000_000;
        assert_eq!(market_order_block(AssetClass::Crypto, Provenance::Real, now - 120, now), None);
        assert!(matches!(
            market_order_block(AssetClass::Fx, Provenance::Real, now - 121, now),
            Some(MarketBlock::Stale { lag_secs: 1 })
        ));
        assert!(!matches!(
            market_order_block(AssetClass::Cmdty, Provenance::Real, now - 86_400, now),
            Some(MarketBlock::Closed)
        ));
    }
}
