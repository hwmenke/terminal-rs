//! Persisted connection choices and a bounded, read-only health snapshot.
use super::*;
use serde::{Deserialize, Serialize};

pub const DEFAULT_URL: &str = "http://127.0.0.1:8050";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Empty means auto-detect. An explicit missing path is never replaced.
    pub archive_path: String,
    pub http_url: String,
    pub prefer_http: bool,
    pub enabled: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            archive_path: std::env::var("WHATS_NEWS_DB").unwrap_or_default(),
            http_url: std::env::var("WHATS_NEWS_URL").unwrap_or_else(|_| DEFAULT_URL.into()),
            prefer_http: true,
            enabled: std::env::var("WHATS_NEWS_OFF").as_deref() != Ok("1"),
        }
    }
}
pub fn detect_archive(home: &std::path::Path) -> Option<PathBuf> {
    ["wn-fix/finance.db", "Whats-News/finance.db", "whats-news/finance.db"]
        .into_iter()
        .map(|p| home.join(p))
        .find(|p| p.is_file())
}
impl Config {
    pub fn archive(&self) -> PathBuf {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let path = self.archive_path.trim();
        if path.is_empty() {
            return detect_archive(&home).unwrap_or_default();
        }
        path.strip_prefix("~/").map_or_else(|| PathBuf::from(path), |p| home.join(p))
    }
    pub fn bridge(&self) -> Bridge {
        let url = self.http_url.trim();
        let mut bridge =
            Bridge::new(self.archive(), (!url.is_empty() && url != "off").then(|| url.into()), self.enabled);
        bridge.prefer_http = self.prefer_http;
        bridge
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    Http,
    Archive,
    Disconnected,
    NotConfigured,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    pub state: LinkState,
    pub source: String,
    pub symbol_count: usize,
    pub latest_date: Option<String>,
    pub message: String,
    pub archive_validation: String,
}
impl Health {
    pub fn pending() -> Self {
        Self {
            state: LinkState::NotConfigured,
            source: String::new(),
            symbol_count: 0,
            latest_date: None,
            message: "Checking Whats-News connection…".into(),
            archive_validation: "Archive validation pending…".into(),
        }
    }
    pub fn connected(&self) -> bool {
        matches!(self.state, LinkState::Http | LinkState::Archive)
    }
    pub fn label(&self) -> &'static str {
        match self.state {
            LinkState::Http => "Connected (HTTP)",
            LinkState::Archive => "Connected (archive file)",
            LinkState::Disconnected => "Disconnected",
            LinkState::NotConfigured => "Not configured",
        }
    }
    pub fn summary(&self) -> String {
        format!(
            "Whats-News: {} · {} · {} symbols · latest {}",
            self.label(),
            self.source,
            self.symbol_count,
            self.latest_date.as_deref().unwrap_or("no daily data")
        )
    }
}
/// Healthy checks every 30s; failures retry at 5, 10, 20, 40, 80, then 120s.
pub fn retry_delay(failures: u32) -> Duration {
    Duration::from_secs(if failures == 0 { 30 } else { (5u64 << failures.saturating_sub(1).min(5)).min(120) })
}
impl Bridge {
    pub async fn health(&self) -> Health {
        let source = format!(
            "{}{}{}",
            self.db.display(),
            if self.url.is_some() { " / " } else { "" },
            self.url.as_deref().unwrap_or_default()
        );
        let result = self.universe_with_warning().await;
        // Validate the configured file even when HTTP is the active source. Never
        // create a missing database or initialize/migrate an upstream schema.
        let archive_validation = if !self.enabled {
            "Archive validation disabled with the connection.".into()
        } else if self.db.as_os_str().is_empty() {
            "No archive file detected. Choose finance.db in Settings › Whats-News to enable file access.".into()
        } else if result.as_ref().is_ok_and(|(s, _)| s.source.starts_with("SQLite read-only:")) {
            format!("Archive file validated: {}", self.db.display())
        } else {
            match self
                .sql(|c| {
                    c.prepare("SELECT symbol, name, group_tag FROM symbols LIMIT 0")?;
                    c.prepare("SELECT symbol, freq, date, open, high, low, close, volume FROM ohlcv LIMIT 0")?;
                    if let Ok(mut stmt) = c.prepare("SELECT DISTINCT provenance FROM ohlcv WHERE freq='daily'") {
                        for value in stmt.query_map([], |r| r.get::<_, Option<String>>(0))? {
                            let provenance = value?
                                .map(|value| {
                                    serde_json::from_value(serde_json::Value::String(value)).unwrap_or_default()
                                })
                                .unwrap_or_default();
                            verify(provenance, "Whats-News archive validation")?;
                        }
                    }
                    let count: i64 = c.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
                    let latest: Option<String> =
                        c.query_row("SELECT MAX(date) FROM ohlcv WHERE freq='daily'", [], |r| r.get(0))?;
                    Ok((count, latest))
                })
                .await
            {
                Ok(snapshot) if snapshot.data.0 > 0 && snapshot.data.1.is_some() => {
                    format!("Archive file validated: {}", self.db.display())
                }
                Ok(_) => "Whats-News archive is empty or has no daily price history. Load data in Whats-News.".into(),
                Err(error) => format!("{error:#}"),
            }
        };
        match result {
            Ok((snapshot, warning)) => {
                let latest_date = snapshot.data.iter().filter_map(|s| s.last_date.clone()).max();
                let empty = snapshot.data.is_empty() || latest_date.is_none();
                Health {
                    state: if empty {
                        LinkState::Disconnected
                    } else if snapshot.source.starts_with("Whats-News HTTP") {
                        LinkState::Http
                    } else {
                        LinkState::Archive
                    },
                    source: snapshot.source.clone(),
                    symbol_count: snapshot.data.len(),
                    archive_validation,
                    latest_date,
                    message: if empty {
                        "Whats-News archive is empty or has no daily price history. Load data in Whats-News, then test the connection again.".into()
                    } else {
                        warning
                    },
                }
            }
            Err(error) => Health {
                state: if !self.enabled || (self.db.as_os_str().is_empty() && self.url.is_none()) {
                    LinkState::NotConfigured
                } else {
                    LinkState::Disconnected
                },
                source,
                archive_validation,
                symbol_count: 0,
                latest_date: None,
                message: format!("{error:#}"),
            },
        }
    }
}
