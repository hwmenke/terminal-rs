//! Securities across asset classes, addressed with Bloomberg-style market keys:
//! `AAPL US Equity`, `SPX Index`, `BTCUSD Curncy`, `EURUSD Curncy`, `US10Y Govt`, `CL1 Comdty`.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::catalog;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AssetClass {
    Equity,
    Etf,
    Index,
    Fx,
    Crypto,
    Govt,
    Cmdty,
}

/// The Bloomberg "yellow key" a security lives under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum YellowKey {
    Equity,
    Index,
    Curncy,
    Govt,
    Comdty,
}

impl YellowKey {
    pub const ALL: [YellowKey; 5] =
        [YellowKey::Equity, YellowKey::Index, YellowKey::Curncy, YellowKey::Govt, YellowKey::Comdty];

    /// Accepts `<CRNCY>`, `CURNCY`, `crncy`, `CMDTY`, `COMDTY`, `GOVT`, `INDEX`, `EQUITY`, `EQ`, `ETF`.
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim().trim_start_matches('<').trim_end_matches('>').to_ascii_uppercase();
        Some(match t.as_str() {
            "EQUITY" | "EQ" | "EQTY" | "ETF" => YellowKey::Equity,
            "INDEX" | "IDX" => YellowKey::Index,
            "CRNCY" | "CURNCY" | "CURRENCY" | "CCY" | "CRYPTO" | "FX" => YellowKey::Curncy,
            "GOVT" | "GOV" | "RATES" => YellowKey::Govt,
            "CMDTY" | "COMDTY" | "COMMODITY" | "FUT" => YellowKey::Comdty,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            YellowKey::Equity => "Equity",
            YellowKey::Index => "Index",
            YellowKey::Curncy => "Curncy",
            YellowKey::Govt => "Govt",
            YellowKey::Comdty => "Comdty",
        }
    }
}

impl AssetClass {
    pub fn yellow_key(self) -> YellowKey {
        match self {
            AssetClass::Equity | AssetClass::Etf => YellowKey::Equity,
            AssetClass::Index => YellowKey::Index,
            AssetClass::Fx | AssetClass::Crypto => YellowKey::Curncy,
            AssetClass::Govt => YellowKey::Govt,
            AssetClass::Cmdty => YellowKey::Comdty,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AssetClass::Equity => "Equity",
            AssetClass::Etf => "ETF",
            AssetClass::Index => "Index",
            AssetClass::Fx => "FX",
            AssetClass::Crypto => "Crypto",
            AssetClass::Govt => "Govt",
            AssetClass::Cmdty => "Comdty",
        }
    }

    /// Annualised volatility used for synthetic sample data.
    pub fn sample_vol(self) -> f64 {
        match self {
            AssetClass::Equity => 0.30,
            AssetClass::Etf | AssetClass::Index => 0.18,
            AssetClass::Fx => 0.08,
            AssetClass::Crypto => 0.60,
            AssetClass::Govt => 0.20,
            AssetClass::Cmdty => 0.30,
        }
    }

    /// Exchanges trade crypto around the clock; everything else has sessions.
    pub fn trades_24_7(self) -> bool {
        self == AssetClass::Crypto
    }
}

const FIAT: &[&str] = &[
    "USD", "EUR", "JPY", "GBP", "CHF", "AUD", "CAD", "NZD", "SEK", "NOK", "DKK", "CNH", "CNY", "HKD", "SGD", "MXN",
    "BRL", "ZAR", "TRY", "INR", "KRW", "PLN", "HUF", "CZK", "ILS", "THB", "CLP", "COP", "ARS", "PEN", "IDR", "PHP",
    "TWD", "RUB", "SAR", "AED", "MYR", "VND", "PKR", "BDT", "EGP", "NGN", "KES", "GHS", "MAD", "QAR", "KWD", "BHD",
    "OMR", "JOD", "RON", "BGN", "ISK", "UAH", "RSD", "GEL", "KZT", "UZS", "LKR", "NPR", "MUR", "BWP", "NAD", "ZMW",
    "UGX", "TZS", "BOB", "UYU", "PYG", "DOP", "CRC", "GTQ", "HNL", "NIO", "PAB", "BAM", "ALL", "MKD", "MDL", "AMD",
    "AZN",
];

pub fn is_fiat(code: &str) -> bool {
    FIAT.contains(&code)
}

fn is_fiat_pair(ticker: &str) -> bool {
    ticker.is_ascii() && ticker.len() == 6 && is_fiat(&ticker[..3]) && is_fiat(&ticker[3..])
}

/// A resolved security.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Security {
    pub ticker: String,
    pub class: AssetClass,
}

impl Security {
    pub fn new(ticker: impl Into<String>, class: AssetClass) -> Self {
        Self { ticker: ticker.into().to_ascii_uppercase(), class }
    }

    pub fn equity(ticker: &str) -> Self {
        Self::new(ticker, AssetClass::Equity)
    }

    /// Canonical market key, e.g. `AAPL US Equity`, `BTCUSD Curncy`.
    pub fn key(&self) -> String {
        match self.class {
            AssetClass::Equity | AssetClass::Etf => format!("{} US Equity", self.ticker),
            c => format!("{} {}", self.ticker, c.yellow_key().label()),
        }
    }

    /// Resolve user input such as `BTCUSD <CRNCY>`, `aapl us equity`, `US10Y GOVT`, `CL1`.
    /// Returns the security and how many input tokens it consumed.
    pub fn resolve_tokens(tokens: &[&str]) -> Option<(Self, usize)> {
        let (&head, rest) = tokens.split_first()?;
        // `BTC/USD` is a pair; `BRK/B` is a share class.
        let up = head.trim().to_ascii_uppercase();
        let ticker = match up.split_once('/') {
            Some((a, b)) if a.len() >= 3 && b.len() >= 3 => format!("{a}{b}"),
            Some((a, b)) => format!("{a}.{b}"),
            None => up,
        };
        if !looks_like_ticker(&ticker) {
            return None;
        }
        // Optional market/country word (`US`) then a yellow key.
        let mut used = 1;
        let mut key = None;
        for t in rest.iter().take(2) {
            if let Some(k) = YellowKey::parse(t) {
                key = Some(k);
                used += 1;
                break;
            }
            if t.eq_ignore_ascii_case("US") {
                used += 1;
                continue;
            }
            break;
        }
        if key.is_none() && used > 1 {
            used = 1; // a stray `US` without a yellow key belongs to the next function
        }
        Some((Self::classify(&ticker, key), used))
    }

    /// Parse a full key or bare ticker (e.g. persisted settings).
    pub fn parse(input: &str) -> Option<Self> {
        let tokens: Vec<&str> = input.split_whitespace().collect();
        Self::resolve_tokens(&tokens).filter(|(_, used)| *used == tokens.len()).map(|(s, _)| s)
    }

    fn classify(ticker: &str, key: Option<YellowKey>) -> Self {
        // Yahoo's three-letter shorthand means USD/quote. Only currency inputs
        // use bare fiat codes, so equity tickers such as CAD keep their meaning.
        let yahoo_fx = ticker.strip_suffix("=X");
        let fx_ticker = yahoo_fx.unwrap_or(ticker);
        if (key == Some(YellowKey::Curncy) || key.is_none() && yahoo_fx.is_some()) && is_fiat(fx_ticker) {
            return Self::new(format!("USD{fx_ticker}"), AssetClass::Fx);
        }
        if key.is_none_or(|k| k == YellowKey::Curncy) && is_fiat_pair(fx_ticker) {
            return Self::new(fx_ticker, AssetClass::Fx);
        }
        // Recover persisted FX keys carrying an extra USD suffix.
        if key.is_none_or(|k| k == YellowKey::Curncy)
            && let Some(pair) = ticker.strip_suffix("USD").filter(|pair| is_fiat_pair(pair))
        {
            return Self::new(pair, AssetClass::Fx);
        }
        let listed = catalog::lookup(ticker, key);
        if let Some(l) = listed {
            return Self::new(l.ticker, l.class);
        }
        // Only a known crypto base alias may acquire a USD quote. In particular,
        // never append USD to arbitrary currency-key input or a complete FX pair.
        if key.is_none_or(|k| k == YellowKey::Curncy)
            && let Some(l) = catalog::lookup(&format!("{ticker}USD"), Some(YellowKey::Curncy))
            && l.class == AssetClass::Crypto
        {
            return Self::new(l.ticker, l.class);
        }
        let class = match key {
            Some(YellowKey::Index) => AssetClass::Index,
            Some(YellowKey::Govt) => AssetClass::Govt,
            Some(YellowKey::Comdty) => AssetClass::Cmdty,
            Some(YellowKey::Curncy) => AssetClass::Crypto,
            Some(YellowKey::Equity) => AssetClass::Equity,
            None => AssetClass::Equity,
        };
        Self::new(ticker, class)
    }

    /// Display name from the catalog, if known.
    pub fn name(&self) -> Option<&'static str> {
        catalog::lookup(&self.ticker, Some(self.class.yellow_key())).filter(|l| l.class == self.class).map(|l| l.name)
    }

    /// Base/quote split for currency pairs (`BTCUSD` -> `BTC`, `USD`).
    pub fn pair(&self) -> Option<(&str, &str)> {
        if !matches!(self.class, AssetClass::Fx | AssetClass::Crypto) {
            return None;
        }
        if self.class == AssetClass::Fx && is_fiat_pair(&self.ticker) {
            return Some(self.ticker.split_at(3));
        }
        for q in ["USDT", "USDC", "USD", "EUR", "GBP", "JPY", "BTC", "ETH"] {
            if let Some(base) = self.ticker.strip_suffix(q).filter(|b| !b.is_empty()) {
                return Some((base, q));
            }
        }
        None
    }

    /// Price decimals appropriate for the instrument and level.
    pub fn decimals(&self, price: f64) -> usize {
        match self.class {
            AssetClass::Govt => 3,
            AssetClass::Fx if self.ticker.ends_with("JPY") => 3,
            AssetClass::Fx => 4,
            _ => match price.abs() {
                p if p >= 1000.0 => 2,
                p if p >= 1.0 => 2,
                p if p >= 0.01 => 4,
                _ => 6,
            },
        }
    }

    /// Whether congressional (STOCK Act) disclosures can reference this security.
    pub fn has_congress_trades(&self) -> bool {
        matches!(self.class, AssetClass::Equity | AssetClass::Etf)
    }
}

impl fmt::Display for Security {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.key())
    }
}

pub fn looks_like_ticker(s: &str) -> bool {
    let core = s.trim_start_matches('^');
    !core.is_empty()
        && core.len() <= 14
        && core.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '='))
        && core.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> Security {
        Security::parse(s).unwrap()
    }

    #[test]
    fn fx_aliases_and_all_supported_fiat_pairs_are_canonical() {
        for input in ["USDBRL CURNCY", "usdbrl curncy", "BRL Curncy", "BRL=X", "brl=x", "USDBRL=X", "USDBRLUSD Curncy"]
        {
            assert_eq!(r(input), Security::new("USDBRL", AssetClass::Fx), "{input}");
            assert_eq!(r(input).pair(), Some(("USD", "BRL")));
        }
        assert_eq!(r("EURBRL Curncy"), Security::new("EURBRL", AssetClass::Fx));
        assert_eq!(r("EURBRL Curncy").pair(), Some(("EUR", "BRL")));
        for code in FIAT {
            let pair = format!("USD{code}");
            assert_eq!(r(&format!("{pair} Curncy")).ticker, pair);
            assert_eq!(r(&pair).class, AssetClass::Fx, "{pair}");
            assert_eq!(r(&format!("{code} Curncy")).ticker, pair);
            assert_eq!(r(&format!("{code}=X")).ticker, pair);
        }
        for input in ["BTC", "BTCUSD", "BTCUSD Curncy"] {
            assert_eq!(r(input), Security::new("BTCUSD", AssetClass::Crypto));
        }
        assert_eq!(r("SPY").class, AssetClass::Etf);
        assert_eq!(r("SPX Index").class, AssetClass::Index);
        assert_eq!(r("CAD Equity").class, AssetClass::Equity);
        assert_eq!(r("UNKNOWN Curncy").ticker, "UNKNOWN", "unknowns do not acquire a USD suffix");
        for input in ["???", "", "ação", "💱", &"A".repeat(100_000)] {
            assert!(Security::parse(input).is_none());
        }
    }

    #[test]
    fn market_keys() {
        assert_eq!(r("BTCUSD <CRNCY>"), Security::new("BTCUSD", AssetClass::Crypto));
        assert_eq!(r("EURUSD <CRNCY>"), Security::new("EURUSD", AssetClass::Fx));
        assert_eq!(r("US10Y <GOVT>"), Security::new("US10Y", AssetClass::Govt));
        assert_eq!(r("CL1 <CMDTY>"), Security::new("CL1", AssetClass::Cmdty));
        assert_eq!(r("SPX <INDEX>"), Security::new("SPX", AssetClass::Index));
        assert_eq!(r("AAPL <EQUITY>"), Security::new("AAPL", AssetClass::Equity));
        assert_eq!(r("aapl us equity"), Security::new("AAPL", AssetClass::Equity));
        assert_eq!(r("CL1 Comdty").key(), "CL1 Comdty");
        assert_eq!(r("SPY").class, AssetClass::Etf);
    }

    #[test]
    fn bare_tickers_use_catalog_then_heuristics() {
        assert_eq!(r("BTCUSD").class, AssetClass::Crypto);
        assert_eq!(r("EURUSD").class, AssetClass::Fx);
        assert_eq!(r("AUDNZD").class, AssetClass::Fx);
        assert_eq!(r("SPX").class, AssetClass::Index);
        assert_eq!(r("US10Y").class, AssetClass::Govt);
        assert_eq!(r("ZZZQ").class, AssetClass::Equity);
        assert_eq!(r("PEPEUSD CRNCY").class, AssetClass::Crypto);
        assert_eq!(r("btc/usd").ticker, "BTCUSD");
        assert_eq!(r("brk/b").ticker, "BRK.B");
    }

    #[test]
    fn tokens_consumed() {
        let t = ["AAPL", "US", "EQUITY", "RSI"];
        assert_eq!(Security::resolve_tokens(&t).unwrap().1, 3);
        let t = ["AAPL", "RSI"];
        assert_eq!(Security::resolve_tokens(&t).unwrap().1, 1);
        let t = ["AAPL", "US", "CONG"];
        assert_eq!(Security::resolve_tokens(&t).unwrap().1, 1);
    }

    #[test]
    fn keys_and_helpers() {
        assert_eq!(Security::new("BTCUSD", AssetClass::Crypto).key(), "BTCUSD Curncy");
        assert_eq!(Security::new("US10Y", AssetClass::Govt).key(), "US10Y Govt");
        assert_eq!(Security::new("BTCUSDT", AssetClass::Crypto).pair(), Some(("BTC", "USDT")));
        assert_eq!(Security::new("USDJPY", AssetClass::Fx).decimals(150.0), 3);
        assert!(!Security::new("BTCUSD", AssetClass::Crypto).has_congress_trades());
        assert_eq!(r("BTCUSD Curncy").name(), Some("Bitcoin / US Dollar"));
    }
}
