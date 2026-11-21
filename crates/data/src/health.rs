//! Feed health contains observations, never replacement market values.
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feed {
    Bars,
    Quotes,
    Options,
    Book,
    Curve,
    Ticks,
}
impl Feed {
    pub const ALL: [Self; 6] = [Self::Bars, Self::Quotes, Self::Options, Self::Book, Self::Curve, Self::Ticks];
    pub fn label(self) -> &'static str {
        match self {
            Self::Bars => "BAR",
            Self::Quotes => "QTE",
            Self::Options => "OPT",
            Self::Book => "BK",
            Self::Curve => "CRV",
            Self::Ticks => "WS",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataMode {
    Live,
    ExplicitSample,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownKind {
    Offline,
    RateLimited,
    Empty,
    Unsupported,
    Startup,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable {
    pub feed: Feed,
    pub kind: DownKind,
    pub reason: String,
    pub since: i64,
    pub retry_at: Option<i64>,
}
impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} unavailable: {}", self.feed.label(), self.reason)
    }
}
impl std::error::Error for Unavailable {}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FeedState {
    #[default]
    Unknown,
    Live {
        last_ok: i64,
    },
    Stale {
        age_secs: i64,
        source: String,
        reason: String,
        retry_at: Option<i64>,
    },
    Unavailable(Unavailable),
    Unsupported {
        reason: String,
    },
}
impl FeedState {
    pub fn reason(&self) -> &str {
        match self {
            Self::Stale { reason, .. } | Self::Unsupported { reason } => reason,
            Self::Unavailable(e) => &e.reason,
            _ => "Waiting for real data",
        }
    }
    pub fn retry_at(&self) -> Option<i64> {
        match self {
            Self::Stale { retry_at, .. } => *retry_at,
            Self::Unavailable(e) => e.retry_at,
            _ => None,
        }
    }
    pub fn is_down(&self) -> bool {
        matches!(self, Self::Unavailable(_) | Self::Unsupported { .. })
    }
    pub fn stale(&self, timestamp: i64, source: &str, now: i64) -> Self {
        if matches!(self, Self::Unavailable(_) | Self::Stale { .. }) {
            Self::Stale {
                age_secs: (now - timestamp).max(0),
                source: source.into(),
                reason: self.reason().into(),
                retry_at: self.retry_at(),
            }
        } else {
            self.clone()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Outcome {
    #[default]
    None,
    Ok,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedHealth {
    pub state: FeedState,
    pub source: String,
    pub last_ok: Option<i64>,
    pub last_err: Option<String>,
    pub ok: u32,
    pub failed: u32,
    pub streak: u32,
    pub recent: [Outcome; 60],
}
pub type Snapshot = [FeedHealth; 6];
pub struct HealthBoard(Mutex<Snapshot>);
impl HealthBoard {
    pub fn new(source: &str) -> Self {
        Self(Mutex::new(std::array::from_fn(|_| FeedHealth {
            state: FeedState::Unknown,
            source: source.into(),
            last_ok: None,
            last_err: None,
            ok: 0,
            failed: 0,
            streak: 0,
            recent: [Outcome::None; 60],
        })))
    }
    pub fn snapshot(&self) -> Snapshot {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn record_ok(&self, feed: Feed, now: i64) {
        let mut board = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let h = &mut board[feed as usize];
        h.ok = h.ok.saturating_add(1);
        h.streak = 0;
        h.last_ok = Some(now);
        h.state = FeedState::Live { last_ok: now };
        h.recent.rotate_left(1);
        h.recent[59] = Outcome::Ok;
    }
    pub fn record_err(&self, feed: Feed, kind: DownKind, reason: String, now: i64) -> Unavailable {
        let mut board = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let h = &mut board[feed as usize];
        h.failed = h.failed.saturating_add(1);
        h.streak = h.streak.saturating_add(1);
        let since = match &h.state {
            FeedState::Unavailable(e) => e.since,
            _ => now,
        };
        let retry_at = (kind != DownKind::Unsupported).then(|| now + (5 * (1_i64 << (h.streak - 1).min(5))).min(120));
        let e = Unavailable { feed, kind, reason: reason.clone(), since, retry_at };
        h.last_err = Some(reason.clone());
        h.state = if kind == DownKind::Unsupported {
            FeedState::Unsupported { reason }
        } else {
            FeedState::Unavailable(e.clone())
        };
        h.recent.rotate_left(1);
        h.recent[59] = Outcome::Failed;
        e
    }
    /// Initialization degradation remains visible even after another source succeeds.
    pub fn note_degraded(&self, feed: Feed, reason: String) {
        self.record_err(feed, DownKind::Startup, reason, chrono::Utc::now().timestamp());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn health_board_tracks_ok_err_backoff() {
        let b = HealthBoard::new("fixture");
        for streak in 1..=10 {
            let e = b.record_err(Feed::Bars, DownKind::Offline, "offline".into(), 100);
            assert_eq!(e.retry_at, Some(100 + (5 * (1_i64 << (streak - 1).min(5))).min(120)));
            assert_eq!(b.snapshot()[0].streak, streak);
        }
        b.record_ok(Feed::Bars, 200);
        let h = &b.snapshot()[0];
        assert_eq!((h.ok, h.failed, h.streak, h.last_ok), (1, 10, 0, Some(200)));
        assert_eq!(h.state, FeedState::Live { last_ok: 200 });
        assert_eq!(b.record_err(Feed::Bars, DownKind::Empty, "empty".into(), 201).retry_at, Some(206));
    }
}
