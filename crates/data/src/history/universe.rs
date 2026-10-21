//! Static named research catalogue, available without any connection. This is a
//! curated broad US universe, not a promise of current index membership.
use crate::whats_news::Symbol;
use anyhow::{Result, bail};
use std::collections::BTreeMap;

pub const LISTS: &[(&str, &str)] = &[
    ("US", "Broad US equities (static)"),
    ("NASDAQ", "Nasdaq research basket (static)"),
    ("ETFS", "Major ETFs and indices"),
    ("ALL", "All bundled symbols"),
    ("WHATS-NEWS", "Connected Whats-News universe"),
];
fn rows(text: &str, group: &str) -> Vec<Symbol> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
        .map(|(s, n)| Symbol { symbol: s.into(), name: Some(n.into()), group_tag: Some(group.into()), last_date: None })
        .collect()
}
pub fn normalize(input: &str) -> Result<String> {
    let s = input.trim().to_ascii_uppercase().replace('.', "-");
    if s.is_empty()
        || s.len() > 24
        || !s.chars().all(|c| c.is_ascii_alphanumeric() || "-^=_".contains(c))
        || !s.chars().any(|c| c.is_ascii_alphanumeric())
    {
        bail!("Invalid Yahoo symbol: {input:?}");
    }
    Ok(s)
}
pub fn bundled(list: &str) -> Option<Vec<Symbol>> {
    let stocks = || rows(include_str!("us_universe.tsv"), "US static");
    let etfs = || rows(include_str!("etfs.tsv"), "ETF / index");
    let data = match list.to_ascii_uppercase().as_str() {
        "US" => stocks(),
        "NASDAQ" | "NDX100" | "NASDAQ100" => {
            let names = " AAPL MSFT NVDA AMZN GOOGL GOOG META AVGO TSLA COST NFLX ADBE PEP AMD CSCO TMUS TXN AMGN QCOM INTU AMAT BKNG ISRG LRCX ADP GILD MDLZ REGN PANW SNPS KLAC CDNS PYPL SBUX MELI CRWD MAR ORLY CTAS ASML ABNB CSX CEG MRVL FTNT MNST PCAR ADSK NXPI MCHP ROST KDP AEP PAYX ODFL LULU EXC IDXX FAST AZN WDAY DDOG VRSK CSGP FANG EA XEL BKR GEHC TEAM ANSS ZS DXCM BIIB ON WBD CDW TTWO GFS ILMN MDB ARM DASH APP PLTR MSTR AXON TTD SHOP INTC MU VRTX HON CMCSA CHTR PDD JD BIDU NTES TRV ";
            let mut all = stocks();
            all.extend(rows(
                "TMUS\tT-Mobile US\nMRVL\tMarvell Technology\nAZN\tAstraZeneca\nNTES\tNetEase",
                "Nasdaq static",
            ));
            all.into_iter().filter(|s| names.contains(&format!(" {} ", s.symbol))).collect()
        }
        "ETFS" | "ETF" | "INDICES" => etfs(),
        "ALL" => {
            let mut s = stocks();
            s.extend(bundled("NASDAQ").unwrap());
            s.extend(etfs());
            s
        }
        _ => return None,
    };
    Some(data.into_iter().map(|s| (s.symbol.clone(), s)).collect::<BTreeMap<_, _>>().into_values().collect())
}
pub fn resolve(input: &str) -> Result<Vec<Symbol>> {
    let mut out = BTreeMap::new();
    for token in input.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()) {
        if let Some(rows) = bundled(token) {
            for r in rows {
                out.insert(r.symbol.clone(), r);
            }
        } else {
            let symbol = normalize(token)?;
            let known = bundled("ALL").unwrap().into_iter().find(|r| r.symbol == symbol);
            out.insert(
                symbol.clone(),
                known.unwrap_or(Symbol { symbol, name: None, group_tag: None, last_date: None }),
            );
        }
    }
    if out.is_empty() {
        bail!("Use HIST ADD US, NASDAQ, ETFS, ALL, WHATS-NEWS, or ticker symbols");
    }
    Ok(out.into_values().collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn broad_named_unique_offline_catalogue() {
        let all = bundled("ALL").unwrap();
        assert!(all.len() > 550, "{}", all.len());
        assert!(
            all.iter()
                .all(|r| r.name.as_ref().is_some_and(|n| !n.is_empty()) && normalize(&r.symbol).unwrap() == r.symbol)
        );
        assert!(bundled("NASDAQ").unwrap().len() >= 100);
        assert_eq!(resolve("aapl,AAPL brk.b").unwrap().len(), 2);
        assert!(resolve("../bad").is_err());
    }
}
