//! Local command-bar catalogue and ranking. Symbol discovery never reads prices or
//! changes their provenance; the optional archive catalogue must be verified first.
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use tr_data::{
    SearchHit,
    whats_news::{Snapshot, Symbol},
};

use super::TerminalApp;
use crate::{
    command,
    palette::{self, Target},
    worker::Request,
};

#[derive(Clone, Debug)]
pub(super) struct Suggestion {
    pub key: String,
    pub name: String,
    pub class: Option<tr_core::AssetClass>,
    pub keywords: String,
}
impl Suggestion {
    fn ticker(key: &str, name: &str) -> Self {
        let sec = tr_data::security(key);
        Self { key: sec.key(), name: name.into(), class: Some(sec.class), keywords: String::new() }
    }
    pub fn label(&self) -> &'static str {
        self.class.map_or("CMD", |c| c.label())
    }
}

#[derive(Default)]
pub(super) struct Catalogue {
    pub symbols: Vec<SearchHit>,
    requested: Option<Instant>,
}
impl Catalogue {
    pub fn request(&mut self, worker: &crate::worker::Worker) {
        if self.requested.is_none_or(|at| at.elapsed() > Duration::from_secs(300)) {
            self.requested = Some(Instant::now());
            worker.send(Request::AutocompleteUniverse);
        }
    }
    pub fn accept(&mut self, result: Result<Snapshot<Vec<Symbol>>, String>) {
        self.symbols = result
            .ok()
            .filter(|s| s.provenance.is_verified())
            .map(|s| {
                s.data
                    .iter()
                    .map(|s| {
                        let sec = tr_data::security(&s.symbol);
                        SearchHit {
                            key: sec.key(),
                            name: s.name.clone().unwrap_or_default(),
                            class: sec.class,
                            exchange: None,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
}

fn rank(mut items: Vec<Suggestion>, recent: &[String], query: &str) -> Vec<Suggestion> {
    // Merge sources, preserving the first known name (the built-in master wins).
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut unique: Vec<Suggestion> = Vec::new();
    for item in items.drain(..) {
        let key = item.key.to_ascii_uppercase();
        if let Some(&at) = by_key.get(&key) {
            if unique[at].name.is_empty() {
                unique[at].name = item.name;
            }
        } else {
            by_key.insert(key, unique.len());
            unique.push(item);
        }
    }
    let q = query.trim().to_ascii_uppercase();
    let mut scored: Vec<_> = unique
        .into_iter()
        .filter_map(|item| {
            let key = item.key.to_ascii_uppercase();
            let ticker = key.split_whitespace().next().unwrap_or_default();
            let exact = key == q || (item.class.is_some() && ticker == q);
            let prefix = key.starts_with(&q);
            let fuzzy = palette::fuzzy(&q, &key)
                .into_iter()
                .chain(palette::fuzzy(&q, &item.name))
                .chain(palette::fuzzy(&q, &item.keywords))
                .max()?;
            // Separate ranking tiers: recency cannot displace an exact ticker or a prefix.
            let tier = if q.is_empty() {
                0
            } else if exact && item.class.is_some() {
                4
            } else if exact {
                3
            } else if prefix {
                2
            } else {
                1
            };
            let boost =
                recent.iter().position(|r| r.eq_ignore_ascii_case(&item.key)).map_or(0, |at| 100 - at.min(99) as i32);
            Some(((tier, boost, fuzzy), item))
        })
        .collect();
    scored.sort_by(|(a, x), (b, y)| b.cmp(a).then_with(|| x.key.cmp(&y.key)));
    scored.into_iter().take(8).map(|(_, item)| item).collect()
}

impl TerminalApp {
    pub(super) fn suggestions(&self) -> Vec<Suggestion> {
        let mut items: Vec<_> =
            tr_core::catalog::CATALOG.iter().map(|s| Suggestion::ticker(&s.key(), s.name)).collect();
        for s in self.watchlist.iter().chain(&self.recent_symbols).filter(|s| tr_core::Security::parse(s).is_some()) {
            let name = tr_core::Security::parse(s).and_then(|s| s.name()).unwrap_or_default();
            items.push(Suggestion::ticker(s, name));
        }
        for h in &self.command_catalogue.symbols {
            items.push(Suggestion::ticker(&h.key, &h.name));
        }
        if self.net_hits.0.eq_ignore_ascii_case(self.cmd.trim()) {
            for h in &self.net_hits.1 {
                items.push(Suggestion::ticker(&h.key, &h.name));
            }
        }
        for item in super::palette_view::bar_items() {
            if let Target::Run(key) = item.target {
                items.push(Suggestion { key, name: item.subtitle, class: None, keywords: item.keywords });
            }
        }
        for d in tr_core::indicators::registry::all() {
            items.push(Suggestion {
                key: d.code.into(),
                name: d.name.into(),
                class: None,
                keywords: d.aliases.join(" "),
            });
        }
        for setup in tr_data::whats_news::SETUPS {
            items.push(Suggestion {
                key: format!("SCAN {setup}"),
                name: format!("Scan archived {} setups", setup.to_ascii_lowercase().replace('_', " ")),
                class: None,
                keywords: "whats-news research".into(),
            });
        }
        rank(items, &self.recent_symbols, &self.cmd)
    }

    pub(super) fn command_changed(&mut self) {
        self.suggestion = None;
        self.cmd_hist_pos = None;
        let q = self.cmd.trim();
        // Unknown tickers need discovery too; don't require an existing local match.
        self.search_pending = (q.len() >= 2
            && tr_core::Security::parse(q).is_some()
            && !q.contains(char::is_whitespace)
            && !matches!(command::parse(q), Some(command::Command::Function(_))))
        .then(|| (q.to_string(), Instant::now()));
    }

    pub(super) fn accept_command(&mut self, picked: Option<&str>) {
        let input = picked.map(str::to_owned).unwrap_or_else(|| std::mem::take(&mut self.cmd));
        self.cmd.clear();
        self.suggestion = None;
        self.cmd_hist_pos = None;
        self.search_pending = None;
        if !input.trim().is_empty() {
            self.execute(&input);
        }
        // Give the chart its keyboard back after loading; further typing opens the bar.
        self.focus_cmd = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Command, Function};

    fn cmd(key: &str, name: &str) -> Suggestion {
        Suggestion { key: key.into(), name: name.into(), class: None, keywords: String::new() }
    }

    #[test]
    fn exact_ticker_then_prefix_then_fuzzy_even_when_fuzzy_is_recent() {
        let items = vec![
            Suggestion::ticker("APLE", "Apple Hospitality"),
            Suggestion::ticker("AAPL", "Apple"),
            Suggestion::ticker("AA", "Alcoa"),
            cmd("AREA", "Area chart"),
        ];
        let hits = rank(items, &["AAPL US Equity".into()], "aa");
        assert_eq!(hits[0].key, "AA US Equity");
        assert_eq!(hits[1].key, "AAPL US Equity");
        let hits = rank(
            vec![Suggestion::ticker("APLE", "Hospitality"), Suggestion::ticker("AAPL", "Apple")],
            &["AAPL US Equity".into()],
            "ap",
        );
        assert_eq!(hits[0].key, "APLE US Equity", "a recent fuzzy hit cannot beat a prefix");
    }

    #[test]
    fn recent_boost_dedup_company_names_and_mixed_functions() {
        let items = vec![
            Suggestion::ticker("AAPL", ""),
            Suggestion::ticker("AAPL", "Apple Inc"),
            Suggestion::ticker("AMZN", "Amazon"),
            cmd("ALRT", "Alerts"),
        ];
        let hits = rank(items.clone(), &["AMZN US Equity".into()], "a");
        assert_eq!(hits[0].key, "AMZN US Equity");
        assert_eq!(hits.iter().filter(|h| h.key == "AAPL US Equity").count(), 1);
        assert!(hits.iter().any(|h| h.class.is_none()));
        assert_eq!(rank(items, &[], "apple")[0].name, "Apple Inc");
        assert!(rank(vec![cmd("HELP", "Help")], &[], "xyz").is_empty());
        assert_eq!(rank(vec![cmd("HELP", "Help")], &[], "hlp")[0].key, "HELP");
    }

    #[test]
    fn catalogue_accepts_only_verified_metadata_and_throttles_background_requests() {
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut cat = Catalogue::default();
        cat.request(&worker);
        cat.request(&worker);
        assert!(matches!(requests.try_recv(), Ok(Request::AutocompleteUniverse)));
        assert!(requests.try_recv().is_err());
        let symbol =
            Symbol { symbol: "ZZTEST".into(), name: Some("Test company".into()), group_tag: None, last_date: None };
        for provenance in [
            tr_core::Provenance::Sample,
            tr_core::Provenance::Missing,
            tr_core::Provenance::Real,
            tr_core::Provenance::Cached,
        ] {
            cat.accept(Ok(Snapshot { provenance, source: "test".into(), data: vec![symbol.clone()] }));
            assert_eq!(cat.symbols.len(), usize::from(provenance.is_verified()));
        }
        cat.accept(Err("offline".into()));
        assert!(cat.symbols.is_empty());
    }

    #[test]
    fn every_function_family_and_indicator_is_discoverable() {
        let mut app = super::super::tests::test_app();
        for code in [
            "HIST",
            "UNIV",
            "SCAN",
            "TRACK",
            "UNTRACK",
            "WL ADD",
            "WL DEL",
            "LAYOUT",
            "NEW",
            "LINK",
            "SIDEBAR",
            "THEME",
            "TITLE",
            "BUY",
            "SELL",
            "BROKER",
            "DEMO",
            "MODE LIVE",
            "PORT",
            "ACCT",
            "POS",
            "ORD",
            "ACTV",
            "TRADE",
            "OMON",
            "CONG",
            "POL",
            "ALRT",
            "REFRESH",
            "WL",
            "N",
            "OB",
            "WEI",
            "GC",
            "SECF",
            "LAYOUTS",
            "SPLIT",
            "POP",
            "CLOSE",
            "PREFS",
            "UPDATE",
            "HELP",
            "CANDLE",
            "BAR",
            "LINE",
            "AREA",
            "LOG",
            "IND",
            "CLEAR",
            "CMP",
            "TL",
            "HL",
            "VL",
            "FIB",
            "MAGNET",
            "CLEAR DRAW",
            "30",
            "RANGE 5Y",
            "YTD",
        ] {
            app.cmd = code.into();
            assert!(app.suggestions().iter().any(|s| s.key == code && !s.name.is_empty()), "missing {code}");
            assert!(matches!(command::parse(code), Some(Command::Function(_))), "{code}");
        }
        for d in tr_core::indicators::registry::all() {
            app.cmd = d.code.into();
            assert!(app.suggestions().iter().any(|s| s.key == d.code), "{}", d.code);
            assert!(matches!(command::parse(d.code), Some(Command::Function(Function::Indicator(_)))));
        }
        // An unknown symbol still schedules asynchronous discovery.
        app.cmd = "ZZZZUNLISTED".into();
        app.command_changed();
        assert!(app.search_pending.is_some());
    }
}
